import Foundation
import Observation

@MainActor
struct HandleServices {
    var subject: () -> String?
    var offer: () async throws -> HandleOffer
    var availability: (String) async throws -> HandleAvailability
    var claim: (String, String?) async throws -> HandleOffer
    var ensureMailbox: (String) async -> Bool
    var purchases: HandlePurchasing
    var accountHandles: (() async throws -> [AccountHandle])? = nil
    var defaults: UserDefaults = .standard
    var timeout: Double = 20
}

private struct PendingHandle: Codable {
    var productId: String
    var name: String
    var awaitingApproval = false
}

@MainActor
@Observable
final class HandleStore {
    enum Activity: Equatable { case none, loading, checking, buying, claiming, restoring }
    private(set) var activity: Activity = .none
    private(set) var loaded = false
    private(set) var enabled = true
    private(set) var handles: [PurchasedHandle] = []
    private(set) var accountHandles: [AccountHandle] = []
    private(set) var ownershipLoaded = false
    private(set) var ownershipError: String?
    private(set) var products: [HandleProduct] = []
    private(set) var maximum = 10
    private(set) var availability: HandleAvailability?
    private(set) var message: String?
    private(set) var unassigned: [HandleTransaction] = []
    private(set) var missingMailboxes: Set<String> = []
    /// The subscription the person picked from the list. `nil` means the first one not yet used.
    var selectedProductId: String?
    var wantedName = "" {
        didSet { if Self.tidy(oldValue) != Self.tidy(wantedName) { availability = nil } }
    }

    @ObservationIgnored private var services: HandleServices
    @ObservationIgnored private var offer: HandleOffer?
    @ObservationIgnored private var pending: [PendingHandle] = []
    @ObservationIgnored private var epoch = 0
    @ObservationIgnored private var pendingRefresh = false
    @ObservationIgnored private nonisolated(unsafe) var listener: Task<Void, Never>?

    init(services: HandleServices) {
        self.services = services
        listener = Task { [weak self, updates = services.purchases.updates] in
            for await _ in updates {
                guard let self else { return }
                if self.activity != .none { self.pendingRefresh = true }
                else { await self.refresh() }
            }
        }
    }
    deinit { listener?.cancel() }

    var busy: Bool { activity != .none }
    var activeCount: Int { Set(handles.filter(\.active).map(\.namespace)).count }
    var waitingForApproval: Bool { pending.contains(where: \.awaitingApproval) }
    var occupiedProductIds: Set<String> { Set(handles.map(\.productId)) }
    func owner(of product: HandleProduct) -> PurchasedHandle? {
        handles.first { $0.productId == product.id }
    }
    var nextProduct: HandleProduct? {
        let occupied = occupiedProductIds
        if let selectedProductId, !occupied.contains(selectedProductId),
           let chosen = products.first(where: { $0.id == selectedProductId }) { return chosen }
        return products.first { !occupied.contains($0.id) }
    }
    func select(_ product: HandleProduct) {
        guard !busy, !occupiedProductIds.contains(product.id) else { return }
        selectedProductId = product.id
    }
    var canBuy: Bool {
        guard !busy, Self.valid(wantedName), !waitingForApproval else { return false }
        if !unassigned.isEmpty { return true }
        return enabled && offer?.appAccountToken.flatMap(UUID.init(uuidString:)) != nil
            && activeCount < maximum && nextProduct != nil && availability?.available == true
            && availability?.name == Self.tidy(wantedName)
    }
    var checkedMessage: String? {
        guard let availability, availability.name == Self.tidy(wantedName) else { return nil }
        if availability.available { return "/\(availability.name) is available." }
        switch availability.reason {
        case "reserved": return "That name is reserved. Choose another."
        case "invalid": return "Use 1–32 letters, numbers, dots, underscores or hyphens."
        default: return "That name is already taken. Choose another."
        }
    }

    private func current(_ stamp: Int, _ subject: String) -> Bool {
        epoch == stamp && services.subject() == subject
    }
    private var pendingKey: String? { services.subject().map { "ppi_handle_pending:" + $0 } }
    private func persist() {
        guard let key = pendingKey, let data = try? JSONEncoder().encode(pending) else { return }
        services.defaults.set(data, forKey: key)
    }
    private func finishOperation(_ stamp: Int, _ subject: String) {
        guard current(stamp, subject) else { return }
        activity = .none
        if pendingRefresh {
            pendingRefresh = false
            Task { [weak self] in await self?.refresh() }
        }
    }
    private func install(_ value: HandleOffer) {
        offer = value
        maximum = min(10, max(1, value.maxHandles ?? 1))
        handles = value.handles ?? value.namespace.map {
            [PurchasedHandle(originalTransactionId: "legacy", namespace: $0,
                productId: value.productId ?? "", environment: "Production", expiresAt: value.expiresAt ?? 0, active: true)]
        } ?? []
        loaded = true
    }

    func refresh(restoring: Bool = false) async {
        guard !busy, let subject = services.subject() else { return }
        let stamp = epoch
        activity = restoring ? .restoring : .loading
        message = nil
        defer { finishOperation(stamp, subject) }
        await reloadOwnership(stamp, subject)
        guard current(stamp, subject) else { return }
        do {
            if let key = pendingKey, let data = services.defaults.data(forKey: key) {
                pending = (try? JSONDecoder().decode([PendingHandle].self, from: data)) ?? []
            }
            if restoring { try await services.purchases.restore() }
            let value = try await withHandleDeadline(seconds: services.timeout, services.offer)
            guard current(stamp, subject) else { return }
            enabled = true
            install(value)
            let ids = HandleCatalog.merged(with: value.productIds ?? value.productId.map { [$0] } ?? [])
            let loadedProducts = try await withHandleDeadline(seconds: services.timeout) { [purchases = services.purchases] in
                try await purchases.products(ids)
            }
            guard current(stamp, subject) else { return }
            products = loadedProducts
            let transactions = try await withHandleTransactions()
            guard current(stamp, subject) else { return }
            unassigned = []
            for transaction in transactions where ids.contains(transaction.productId) {
                guard current(stamp, subject) else { return }
                await settle(transaction, desiredName: nil, stamp: stamp, subject: subject)
            }
            guard current(stamp, subject) else { return }
            let updated = try await withHandleDeadline(seconds: services.timeout, services.offer)
            guard current(stamp, subject) else { return }
            install(updated)
            for handle in handles where handle.active {
                let exists = await services.ensureMailbox(handle.namespace)
                guard current(stamp, subject) else { return }
                if exists { missingMailboxes.remove(handle.namespace) }
                else { missingMailboxes.insert(handle.namespace) }
            }
            if products.isEmpty && message == nil {
                message = "The App Store did not return these subscriptions. You can still restore purchases or try again."
            } else if restoring && handles.isEmpty && unassigned.isEmpty && message == nil {
                message = "No active handle purchases were found for this Apple account."
            }
        } catch is CancellationError { }
        catch let error as APIError where error.status == 404 {
            if current(stamp, subject) { enabled = false; loaded = true; message = "Handle purchases are temporarily unavailable." }
        } catch {
            if current(stamp, subject) { message = Self.explain(error) }
        }
    }

    private func withHandleTransactions() async throws -> [HandleTransaction] {
        try await withHandleDeadline(seconds: services.timeout) { [purchases = services.purchases] in
            await purchases.transactions()
        }
    }

    private func reloadOwnership(_ stamp: Int, _ subject: String) async {
        guard let fetch = services.accountHandles else { return }
        do {
            let rows = try await withHandleDeadline(seconds: services.timeout, fetch)
            guard current(stamp, subject) else { return }
            accountHandles = rows
            ownershipLoaded = true
            ownershipError = nil
        } catch {
            if current(stamp, subject) { ownershipError = "Could not refresh your account handles. Your registrations are saved. Try Refresh again." }
        }
    }

    func checkAvailability() async {
        guard !busy, let subject = services.subject() else { return }
        let name = Self.tidy(wantedName), stamp = epoch
        guard Self.valid(name) else {
            availability = nil; message = "Use 1–32 letters, numbers, dots, underscores or hyphens; start and end with a letter or number."
            return
        }
        activity = .checking; message = nil
        defer { finishOperation(stamp, subject) }
        do {
            let result = try await withHandleDeadline(seconds: services.timeout) { [probe = services.availability] in try await probe(name) }
            guard current(stamp, subject), Self.tidy(wantedName) == name else { return }
            availability = result
        } catch { if current(stamp, subject) { message = Self.explain(error) } }
    }

    func buy() async {
        guard canBuy else { return }
        if let transaction = unassigned.first {
            await finishPurchase(transaction)
            return
        }
        guard let product = nextProduct else { return }
        await purchase(product, name: Self.tidy(wantedName))
    }

    func renew(_ handle: PurchasedHandle) async {
        guard !busy, !handle.active, let product = products.first(where: { $0.id == handle.productId }) else { return }
        await purchase(product, name: Self.tidy(handle.namespace))
    }

    private func purchase(_ product: HandleProduct, name: String) async {
        guard !busy, let subject = services.subject(), let token = offer?.appAccountToken.flatMap(UUID.init(uuidString:)),
              Self.valid(name), activeCount < maximum else { return }
        let stamp = epoch
        activity = .checking; message = nil
        defer { finishOperation(stamp, subject) }
        do {
            let checked = try await withHandleDeadline(seconds: services.timeout) { [probe = services.availability] in try await probe(name) }
            guard current(stamp, subject) else { return }
            guard checked.available, checked.name == name else { availability = checked; message = "That name cannot be bought. Choose an available name."; return }
            pending.removeAll { $0.productId == product.id }
            pending.append(PendingHandle(productId: product.id, name: name))
            persist()
            activity = .buying
            // The Apple confirmation sheet belongs to the person. It has no artificial deadline.
            let result = try await services.purchases.purchase(product.id, accountToken: token)
            guard current(stamp, subject) else { return }
            switch result {
            case let .purchased(transaction):
                activity = .claiming
                await settle(transaction, desiredName: name, stamp: stamp, subject: subject)
                guard current(stamp, subject) else { return }
                let updated = try await withHandleDeadline(seconds: services.timeout, services.offer)
                guard current(stamp, subject) else { return }
                install(updated)
            case .cancelled:
                pending.removeAll { $0.productId == product.id }; persist()
                message = "Purchase cancelled. Your name is still here."
            case .pending:
                if let index = pending.firstIndex(where: { $0.productId == product.id }) { pending[index].awaitingApproval = true; persist() }
                message = "Waiting for Apple purchase approval. The name will be registered when approval arrives."
            }
        } catch { if current(stamp, subject) { message = Self.explain(error) } }
    }

    private func finishPurchase(_ transaction: HandleTransaction) async {
        guard !busy, let subject = services.subject(), Self.valid(wantedName) else { return }
        let stamp = epoch
        activity = .claiming; message = nil
        defer { finishOperation(stamp, subject) }
        await settle(transaction, desiredName: Self.tidy(wantedName), stamp: stamp, subject: subject)
        guard current(stamp, subject) else { return }
        do {
            let value = try await withHandleDeadline(seconds: services.timeout, services.offer)
            guard current(stamp, subject) else { return }
            install(value)
        } catch { if current(stamp, subject) { message = Self.explain(error) } }
    }

    private func settle(_ transaction: HandleTransaction, desiredName: String?, stamp: Int, subject: String) async {
        guard current(stamp, subject), transaction.active else { return }
        if let token = transaction.accountToken, token.uuidString.lowercased() != offer?.appAccountToken?.lowercased() { return }
        let existing = handles.first { $0.originalTransactionId == transaction.originalId }
        let selectedName = existing == nil ? (desiredName ?? pending.first { $0.productId == transaction.productId }?.name) : nil
        do {
            let claim = services.claim
            let value = try await withHandleDeadline(seconds: services.timeout) { try await claim(transaction.id, selectedName) }
            guard current(stamp, subject) else { return }
            guard let namespace = value.namespace else { throw APIError(status: 200, code: "bad_response", detail: "The postbox did not confirm a handle. Try again.") }
            await services.purchases.finish(transaction.id)
            guard current(stamp, subject) else { return }
            pending.removeAll { $0.productId == transaction.productId }; persist()
            unassigned.removeAll { $0.originalId == transaction.originalId }
            if Self.tidy(wantedName) == selectedName { wantedName = ""; availability = nil }
            if selectedProductId == transaction.productId { selectedProductId = nil }
            let exists = await services.ensureMailbox(namespace)
            guard current(stamp, subject) else { return }
            if exists { missingMailboxes.remove(namespace) }
            else { missingMailboxes.insert(namespace) }
            if current(stamp, subject), desiredName != nil { message = "\(namespace) is ready." }
            await reloadOwnership(stamp, subject)
        } catch {
            guard current(stamp, subject) else { return }
            if let failure = error as? APIError, ["purchase_already_used", "purchase_expired", "purchase_refunded"].contains(failure.code ?? "") {
                message = Self.explain(error)
                return
            }
            if !unassigned.contains(where: { $0.originalId == transaction.originalId }) { unassigned.append(transaction) }
            if wantedName.isEmpty { wantedName = selectedName ?? "" }
            message = "Your purchase is saved. \(Self.explain(error)) Finish registration here without another payment."
        }
    }

    func repairMailbox(_ namespace: String) async {
        guard !busy, let subject = services.subject() else { return }
        let stamp = epoch
        activity = .claiming
        defer { finishOperation(stamp, subject) }
        let exists = await services.ensureMailbox(namespace)
        guard current(stamp, subject) else { return }
        if exists { missingMailboxes.remove(namespace) }
        else { message = "The name is yours, but its inbox could not be loaded. Try again." }
    }

    func reset() {
        epoch += 1; listener?.cancel(); listener = nil
        activity = .none; loaded = false; offer = nil; handles = []; products = []
        pending = []; unassigned = []; wantedName = ""; availability = nil; message = nil; selectedProductId = nil
        pendingRefresh = false; missingMailboxes = []
        accountHandles = []; ownershipLoaded = false; ownershipError = nil
    }

    static func tidy(_ raw: String) -> String {
        raw.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: "/")).lowercased()
    }
    static func valid(_ raw: String) -> Bool {
        let name = tidy(raw)
        let bytes = Array(name.utf8)
        func alnum(_ b: UInt8) -> Bool { (97...122).contains(b) || (48...57).contains(b) }
        return (1...32).contains(bytes.count) && bytes.first.map(alnum) == true && bytes.last.map(alnum) == true
            && bytes.allSatisfy { alnum($0) || $0 == 45 || $0 == 46 || $0 == 95 }
            && !["k", "gh"].contains(name)
    }
    private static func explain(_ error: Error) -> String {
        if let failure = error as? APIError {
            switch failure.code {
            case "namespace_taken", "handle_already_subscribed": return "That name is already held. Choose another name or restore its purchase."
            case "name_reserved": return "That name is reserved. Choose another."
            case "name_required": return "Choose a name for it."
            case "purchase_already_used": return "This purchase belongs to another Pigeonpost account. Sign in to that account."
            case "handle_limit_reached": return "This account already has ten active Apple handles."
            case "purchase_expired": return "That subscription has expired."
            case "purchase_refunded": return "That purchase was refunded."
            case "appstore_unavailable": return "Apple verification is temporarily unavailable. Try again."
            default: return failure.errorDescription ?? "The postbox could not finish the request."
            }
        }
        return (error as? LocalizedError)?.errorDescription ?? "The request did not complete. Try again."
    }
}
