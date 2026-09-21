#if DEBUG
import Foundation

@MainActor
enum HandleFixtures {
    static func make(_ state: String, account: Account) -> HandleStore {
        let backend = HandleFixtureBackend(state: state, account: account)
        let purchases = FixtureHandlePurchases(state: state)
        let defaults = UserDefaults(suiteName: "ppi_handle_fixture_" + UUID().uuidString)!
        let store = HandleStore(services: HandleServices(subject: { "fixture" },
            offer: { try backend.offer() }, availability: { backend.check($0) },
            claim: { try backend.claim($0, $1, purchases: purchases) },
            ensureMailbox: { backend.ensure($0) }, purchases: purchases, accountHandles: {
                backend.handles.map { AccountHandle(namespace: $0.namespace, source: "apple", expiresAt: $0.expiresAt, active: $0.active) }
                    + (state == "owned" ? [AccountHandle(namespace: "studio", source: "google", expiresAt: Int(Date().addingTimeInterval(86400).timeIntervalSince1970), active: true),
                        AccountHandle(namespace: "previous", source: "google", expiresAt: Int(Date().addingTimeInterval(-86400).timeIntervalSince1970), active: false)] : [])
            }, defaults: defaults))
        store.wantedName = state == "owned" || state == "ten" ? "" : "alex"
        return store
    }
}

@MainActor
private final class HandleFixtureBackend {
    static let token = UUID(uuidString: "3f777e63-bb10-4877-89bb-44fbb0803d8d")!
    static let ids = ["dev.pigeonpost.inbox.handle.yearly"] + (2...10).map { "dev.pigeonpost.inbox.handle\($0).yearly" }
    var handles: [PurchasedHandle] = []
    let state: String
    var claims = 0
    weak var account: Account?
    init(state: String, account: Account) {
        self.state = state; self.account = account
        let count = state == "ten" ? 10 : state == "owned" ? 1 : 0
        for index in 0..<count {
            handles.append(PurchasedHandle(originalTransactionId: "fixture-owned-\(index)",
                namespace: index == 0 ? "/alex" : "/alex\(index + 1)", productId: Self.ids[index],
                environment: "Sandbox", expiresAt: Int(Date().addingTimeInterval(365 * 86400).timeIntervalSince1970), active: true))
        }
    }
    func offer() throws -> HandleOffer {
        if state == "unavailable" { throw APIError(status: 404, code: "not_found", detail: nil) }
        var offer = HandleOffer(productId: Self.ids[0], namespace: handles.first?.namespace, expiresAt: handles.first?.expiresAt)
        offer.productIds = Self.ids; offer.maxHandles = 10; offer.account = "fixture"
        offer.appAccountToken = Self.token.uuidString; offer.handles = handles
        return offer
    }
    func check(_ name: String) -> HandleAvailability {
        let reason: String? = ["admin", "support"].contains(name) ? "reserved" : name == "taken" || handles.contains(where: { $0.namespace == "/" + name }) ? "taken" : nil
        return HandleAvailability(name: name, available: reason == nil, reason: reason)
    }
    func claim(_ id: String, _ name: String?, purchases: FixtureHandlePurchases) throws -> HandleOffer {
        claims += 1
        if state == "retry" && claims == 1 { throw APIError(status: 503, code: "appstore_unavailable", detail: nil) }
        guard let transaction = purchases.values.first(where: { $0.id == id }) else { throw HandlePurchaseError.unverified }
        let namespace: String
        if let existing = handles.first(where: { $0.originalTransactionId == transaction.originalId }) {
            namespace = existing.namespace
        } else {
            guard let name else { throw APIError(status: 400, code: "name_required", detail: nil) }
            guard check(name).available else { throw APIError(status: 409, code: "namespace_taken", detail: nil) }
            namespace = "/" + name
            handles.append(PurchasedHandle(originalTransactionId: transaction.originalId, namespace: namespace,
                productId: transaction.productId, environment: "Sandbox",
                expiresAt: Int(transaction.expiresAt!.timeIntervalSince1970), active: true))
        }
        return HandleOffer(productId: nil, namespace: namespace, expiresAt: Int(transaction.expiresAt!.timeIntervalSince1970))
    }
    func ensure(_ name: String) -> Bool {
        guard let account else { return false }
        if account.mailbox(inNamespace: name) != nil { return true }
        let mailbox = Mailbox(address: "/k/fixture-" + name.dropFirst(), handle: name + "/main", label: "main")
        account.installFixtures(mailboxes: account.mailboxes + [mailbox], me: account.me ?? mailbox)
        return true
    }
}

@MainActor
private final class FixtureHandlePurchases: HandlePurchasing {
    let updates: AsyncStream<HandleTransaction> = AsyncStream { _ in }
    let state: String
    var values: [HandleTransaction] = []
    init(state: String) { self.state = state }
    func products(_ ids: [String]) async throws -> [HandleProduct] {
        if state == "soon" { return [] }
        return ids.enumerated().map { index, id in
            HandleProduct(id: id, displayPrice: "$8.00", price: 8, currencyCode: "USD",
                          displayName: index == 0 ? "Pigeonpost handle" : "Handle \(index + 1) — yearly")
        }
    }
    func purchase(_ id: String, accountToken: UUID) async throws -> HandlePurchaseResult {
        if state == "cancelled" { return .cancelled }
        if state == "pending" { return .pending }
        let transaction = HandleTransaction(id: "fixture-\(values.count)", originalId: "fixture-\(values.count)", productId: id,
            accountToken: accountToken, expiresAt: Date().addingTimeInterval(365 * 86400), revoked: false)
        values.append(transaction)
        return .purchased(transaction)
    }
    func transactions() async -> [HandleTransaction] { values }
    func restore() async throws {}
    func finish(_ id: String) async {}
}
#endif
