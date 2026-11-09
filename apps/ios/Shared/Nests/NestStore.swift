//  Buying Nest packages and keeping the account's Nests in view (Nests Phase B1–B2).
//
//  The one rule that matters: a StoreKit transaction is finished only after the postbox has
//  accepted it. Until then it stays unfinished, so a purchase that was paid for but not yet
//  claimed (the app was closed, the network dropped, the postbox was down) comes back on the
//  next launch through `Transaction.unfinished` and is claimed then. Apple renews and refunds
//  without the app; the postbox re-reads those itself.

import Foundation
import Observation

@MainActor
struct NestServices {
    var subject: () -> String?
    var catalog: () async throws -> NestCatalog
    var snapshot: () async throws -> NestSnapshot
    var intent: (String) async throws -> NestIntent
    var claim: (String) async throws -> NestSnapshot
    var retry: (String) async throws -> NestSnapshot
    var purchases: NestPurchasing
}

@MainActor
@Observable
final class NestStore {
    enum Activity: Equatable { case none, loading, buying, claiming, restoring }
    private(set) var activity: Activity = .none
    private(set) var catalog: NestCatalog?
    private(set) var snapshot: NestSnapshot?
    private(set) var products: [String: HandleProduct] = [:]
    private(set) var message: String?
    /// A paid purchase the postbox has not accepted yet; retried on refresh and next launch.
    private(set) var awaitingClaim = false

    @ObservationIgnored private let services: NestServices
    @ObservationIgnored private var subject: String?
    @ObservationIgnored private nonisolated(unsafe) var listener: Task<Void, Never>?

    init(services: NestServices) {
        self.services = services
        listener = Task { [weak self, updates = services.purchases.updates] in
            for await _ in updates {
                guard let self, self.activity == .none else { continue }
                await self.recover()
            }
        }
    }
    deinit { listener?.cancel() }

    var busy: Bool { activity != .none }
    var nests: [NestView] { snapshot?.nests ?? [] }
    var setups: [NestSetup] { snapshot?.setups ?? [] }
    var funding: NestFunding? { snapshot?.allowance.funding }
    var currentPackage: NestPackage? { snapshot?.package }
    var forSale: [NestPackage] { (catalog?.packages ?? []).filter(\.sellable).sorted { $0.rank > $1.rank } }
    var nestAddresses: Set<String> { Set(nests.compactMap(\.address)) }
    /// Whether this rail funds the account (an upgrade is bought here); another rail is managed there.
    var fundedElsewhere: Bool { funding.map { $0.source != "apple" && $0.source != "grant" && $0.source != "test" } ?? false }

    func price(_ package: NestPackage) -> String? { products[package.appleProductId]?.displayPrice }
    func isCurrent(_ package: NestPackage) -> Bool { funding?.package == package.code }

    func reset() {
        catalog = nil; snapshot = nil; products = [:]; message = nil; subject = nil; awaitingClaim = false
    }

    /// Catalog, Nests, StoreKit prices, then any purchase still waiting for the postbox.
    func refresh() async {
        guard activity == .none else { return }
        activity = .loading
        defer { activity = .none }
        if subject != services.subject() { reset(); subject = services.subject() }
        do {
            async let c = services.catalog()
            async let s = services.snapshot()
            (catalog, snapshot) = try await (c, s)
            message = nil
        } catch {
            message = NestWords.refusal(error)
            return
        }
        let ids = forSale.map(\.appleProductId)
        if let values = try? await services.purchases.products(ids) {
            products = Dictionary(uniqueKeysWithValues: values.map { ($0.id, $0) })
        }
        activity = .none
        await recover()
    }

    /// Claim every Nest transaction StoreKit still holds. Finished only when the postbox gave a
    /// definite answer; an outage leaves it unfinished for next time.
    func recover() async {
        guard let token = catalog.flatMap({ UUID(uuidString: $0.appAccountToken) }) else { return }
        let pending = await services.purchases.transactions()
        var waiting = false
        for transaction in pending where NestProducts.isNest(transaction.productId) {
            // A purchase made for another Pigeonpost account is not ours to claim or finish.
            if let owner = transaction.accountToken, owner != token { continue }
            switch await claim(transaction) {
            case .accepted, .refused: await services.purchases.finish(transaction.id)
            case .later: waiting = true
            }
        }
        awaitingClaim = waiting
    }

    private enum ClaimOutcome { case accepted, refused, later }

    private func claim(_ transaction: HandleTransaction) async -> ClaimOutcome {
        activity = .claiming
        defer { activity = .none }
        do {
            snapshot = try await services.claim(transaction.id)
            return .accepted
        } catch let error as APIError where (400..<500).contains(error.status) && error.status != 429
                    && error.code != "purchase_inactive" && error.code != "feature_disabled" {
            // Definite: not ours, inactive, already used. Said once, not retried for ever.
            message = NestWords.refusal(error)
            return .refused
        } catch {
            message = "Your payment is safe. Pigeonpost will finish setting up your Nest when the connection is back."
            return .later
        }
    }

    /// Buy (or upgrade to) `package`. The postbox is asked first, so a second charge is refused
    /// before any App Store sheet appears.
    func buy(_ package: NestPackage) async {
        guard activity == .none else { return }
        activity = .buying
        defer { if activity == .buying { activity = .none } }
        let intent: NestIntent
        do { intent = try await services.intent(package.code) } catch {
            message = NestWords.refusal(error)
            return
        }
        guard let token = UUID(uuidString: intent.appAccountToken) else {
            message = "The App Store could not be prepared. Try again."
            return
        }
        let result: HandlePurchaseResult
        do { result = try await services.purchases.purchase(intent.appleProductId, accountToken: token) } catch {
            message = (error as? LocalizedError)?.errorDescription ?? "The App Store did not complete the purchase."
            return
        }
        activity = .none
        switch result {
        case .cancelled: message = nil
        case .pending: message = "The purchase is waiting for approval. Your Nest is set up as soon as it is approved."
        case let .purchased(transaction):
            switch await claim(transaction) {
            case .accepted:
                await services.purchases.finish(transaction.id)
                message = nil
                awaitingClaim = false
            case .refused:
                await services.purchases.finish(transaction.id)
            case .later:
                awaitingClaim = true
            }
        }
    }

    func restore() async {
        guard activity == .none else { return }
        activity = .restoring
        do { try await services.purchases.restore() } catch {
            message = (error as? LocalizedError)?.errorDescription ?? "Restore did not complete."
        }
        activity = .none
        await recover()
        if let s = try? await services.snapshot() { snapshot = s }
    }

    func retry(_ setup: NestSetup) async {
        guard activity == .none else { return }
        activity = .loading
        defer { activity = .none }
        do { snapshot = try await services.retry(setup.setupId) } catch { message = NestWords.refusal(error) }
    }

    func reload() async {
        if let s = try? await services.snapshot() { snapshot = s }
    }
}
