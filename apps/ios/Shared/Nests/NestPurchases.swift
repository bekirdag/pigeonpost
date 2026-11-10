//  Nest packages in the App Store (StoreKit 2). Group "Pigeonpost Nests", one monthly
//  auto-renewing product per package; the postbox's catalog names which product is which, and
//  the postbox — never this app — decides what a transaction is worth.
//
//  The product and transaction values are the handle purchases' own (`HandleProduct`,
//  `HandleTransaction`): the same StoreKit facts, a different catalog.

import Foundation
import StoreKit

enum NestProducts {
    static let ids = [
        "dev.pigeonpost.inbox.nests.ai.starter.monthly",
        "dev.pigeonpost.inbox.nests.ai.plus.monthly",
        "dev.pigeonpost.inbox.nests.host.starter.monthly",
    ]
    static func isNest(_ id: String) -> Bool { id.hasPrefix("dev.pigeonpost.inbox.nests.") }
}

@MainActor
protocol NestPurchasing: AnyObject {
    var updates: AsyncStream<HandleTransaction> { get }
    func products(_ ids: [String]) async throws -> [HandleProduct]
    func purchase(_ id: String, accountToken: UUID) async throws -> HandlePurchaseResult
    /// Unfinished transactions first (a purchase the postbox has not accepted yet), then the
    /// current entitlements (a restore on a new device).
    func transactions() async -> [HandleTransaction]
    func restore() async throws
    func finish(_ id: String) async
}

@MainActor
final class AppleNestPurchases: NestPurchasing {
    let updates: AsyncStream<HandleTransaction>
    private let continuation: AsyncStream<HandleTransaction>.Continuation
    private var catalog: [String: Product] = [:]
    private var known: [String: Transaction] = [:]
    private nonisolated(unsafe) var listener: Task<Void, Never>?

    init() {
        let stream = AsyncStream<HandleTransaction>.makeStream()
        updates = stream.stream
        continuation = stream.continuation
        listener = Task { [weak self] in
            for await update in Transaction.updates {
                guard case let .verified(transaction) = update, NestProducts.isNest(transaction.productID) else { continue }
                self?.record(transaction, publish: true)
            }
        }
    }

    deinit { listener?.cancel() }

    @discardableResult
    private func record(_ transaction: Transaction, publish: Bool = false) -> HandleTransaction {
        known[String(transaction.id)] = transaction
        let value = HandleTransaction(id: String(transaction.id), originalId: String(transaction.originalID),
            productId: transaction.productID, accountToken: transaction.appAccountToken,
            expiresAt: transaction.expirationDate, revoked: transaction.revocationDate != nil || transaction.isUpgraded)
        if publish { continuation.yield(value) }
        return value
    }

    func products(_ ids: [String]) async throws -> [HandleProduct] {
        let values = try await Product.products(for: ids)
        try Task.checkCancellation()
        let monthly = values.filter { $0.type == .autoRenewable && $0.subscription?.subscriptionPeriod.unit == .month }
        for product in monthly { catalog[product.id] = product }
        return ids.compactMap { id in monthly.first(where: { $0.id == id }).map {
            HandleProduct(id: $0.id, displayPrice: $0.displayPrice, price: $0.price,
                          currencyCode: $0.priceFormatStyle.currencyCode, displayName: $0.displayName)
        } }
    }

    func purchase(_ id: String, accountToken: UUID) async throws -> HandlePurchaseResult {
        guard let product = catalog[id] else { throw HandlePurchaseError.unavailable }
        switch try await product.purchase(options: [.appAccountToken(accountToken)]) {
        case let .success(result):
            guard case let .verified(transaction) = result else { throw HandlePurchaseError.unverified }
            return .purchased(record(transaction))
        case .userCancelled: return .cancelled
        case .pending: return .pending
        @unknown default: throw HandlePurchaseError.unavailable
        }
    }

    func transactions() async -> [HandleTransaction] {
        var values: [String: HandleTransaction] = [:]
        for await result in Transaction.unfinished {
            if case let .verified(t) = result, NestProducts.isNest(t.productID) { values[String(t.id)] = record(t) }
        }
        for await result in Transaction.currentEntitlements {
            if case let .verified(t) = result, NestProducts.isNest(t.productID) { values[String(t.id)] = record(t) }
        }
        return values.values.sorted { ($0.expiresAt ?? .distantPast) > ($1.expiresAt ?? .distantPast) }
    }

    func restore() async throws { try await AppStore.sync() }

    func finish(_ id: String) async {
        if let transaction = known[id] { await transaction.finish(); known.removeValue(forKey: id) }
    }
}
