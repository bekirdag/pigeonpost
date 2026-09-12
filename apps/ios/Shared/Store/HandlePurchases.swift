import Foundation
import StoreKit

struct HandleProduct: Equatable, Identifiable, Sendable {
    let id: String
    let displayPrice: String
    let price: Decimal
    let currencyCode: String
    var displayName: String? = nil

    func total(for count: Int) -> String {
        (price * Decimal(count)).formatted(.currency(code: currencyCode))
    }
}

struct HandleTransaction: Equatable, Sendable {
    let id: String
    let originalId: String
    let productId: String
    let accountToken: UUID?
    let expiresAt: Date?
    let revoked: Bool
    var active: Bool { !revoked && (expiresAt.map { $0 > Date() } ?? false) }
}

enum HandlePurchaseResult { case purchased(HandleTransaction), cancelled, pending }

@MainActor
protocol HandlePurchasing: AnyObject {
    var updates: AsyncStream<HandleTransaction> { get }
    func products(_ ids: [String]) async throws -> [HandleProduct]
    func purchase(_ id: String, accountToken: UUID) async throws -> HandlePurchaseResult
    func transactions() async -> [HandleTransaction]
    func restore() async throws
    func finish(_ id: String) async
}

enum HandlePurchaseError: LocalizedError {
    case unavailable, unverified, timedOut
    var errorDescription: String? {
        switch self {
        case .unavailable: return "This handle subscription is unavailable in the App Store. Try again later."
        case .unverified: return "Apple could not verify that purchase. Try restoring your purchases."
        case .timedOut: return "The request took too long. Try again."
        }
    }
}

/// Only this adapter deals with StoreKit objects. The controller can exercise failures and
/// interrupted purchases without replacing Apple verification in a shipped build.
@MainActor
final class AppleHandlePurchases: HandlePurchasing {
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
                guard case let .verified(transaction) = update else { continue }
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
            expiresAt: transaction.expirationDate, revoked: transaction.revocationDate != nil)
        if publish { continuation.yield(value) }
        return value
    }

    func products(_ ids: [String]) async throws -> [HandleProduct] {
        let values = try await Product.products(for: ids)
        try Task.checkCancellation()
        let annual = values.filter { $0.type == .autoRenewable && $0.subscription?.subscriptionPeriod.unit == .year && $0.subscription?.subscriptionPeriod.value == 1 }
        for product in annual { catalog[product.id] = product }
        return ids.compactMap { id in annual.first(where: { $0.id == id }).map {
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
            if case let .verified(transaction) = result { values[String(transaction.id)] = record(transaction) }
        }
        for await result in Transaction.currentEntitlements {
            if case let .verified(transaction) = result { values[String(transaction.id)] = record(transaction) }
        }
        // Apply the latest period first. An older unfinished period must not roll back renewal.
        return values.values.sorted { ($0.expiresAt ?? .distantPast) > ($1.expiresAt ?? .distantPast) }
    }

    func restore() async throws { try await AppStore.sync() }
    func finish(_ id: String) async {
        if let transaction = known[id] { await transaction.finish(); known.removeValue(forKey: id) }
    }
}

/// Returns at the deadline even if an Apple lookup ignores task cancellation.
@MainActor
func withHandleDeadline<T: Sendable>(seconds: Double = 20, _ work: @escaping @MainActor () async throws -> T) async throws -> T {
    let race = HandleDeadline<T>()
    return try await withTaskCancellationHandler {
        try await withCheckedThrowingContinuation { continuation in
            race.continuation = continuation
            race.worker = Task {
                do { race.complete(.success(try await work())) }
                catch { race.complete(.failure(error)) }
            }
            race.timer = Task {
                do { try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000)) }
                catch { return }
                race.complete(.failure(HandlePurchaseError.timedOut))
            }
        }
    } onCancel: {
        Task { @MainActor in race.complete(.failure(CancellationError())) }
    }
}

@MainActor
private final class HandleDeadline<T: Sendable> {
    var continuation: CheckedContinuation<T, Error>?
    var worker: Task<Void, Never>?
    var timer: Task<Void, Never>?
    func complete(_ result: Result<T, Error>) {
        guard let continuation else { return }
        self.continuation = nil
        worker?.cancel(); timer?.cancel()
        worker = nil; timer = nil
        continuation.resume(with: result)
    }
}
