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
    static let ids = HandleCatalog.productIds
    var handles: [PurchasedHandle] = []
    var plan: HandlePlan?
    let state: String
    var claims = 0
    weak var account: Account?
    init(state: String, account: Account) {
        self.state = state; self.account = account
        let count = state == "ten" ? 10 : state == "owned" ? 1 : 0
        if count > 0 {
            plan = HandlePlan(originalTransactionId: "fixture-plan", productId: Self.ids[count-1], environment: "Sandbox",
                expiresAt: Int(Date().addingTimeInterval(365 * 86400).timeIntervalSince1970), capacity: count, active: true)
        }
        for index in 0..<count {
            handles.append(PurchasedHandle(originalTransactionId: "fixture-plan",
                namespace: index == 0 ? "/alex" : "/alex\(index + 1)", productId: Self.ids[count-1],
                environment: "Sandbox", expiresAt: Int(Date().addingTimeInterval(365 * 86400).timeIntervalSince1970), active: true))
        }
    }
    func offer() throws -> HandleOffer {
        if state == "unavailable" { throw APIError(status: 404, code: "not_found", detail: nil) }
        var offer = HandleOffer(productId: Self.ids[0], namespace: handles.first?.namespace, expiresAt: handles.first?.expiresAt)
        offer.productIds = Self.ids; offer.maxHandles = 10; offer.account = "fixture"
        offer.appAccountToken = Self.token.uuidString; offer.handles = handles; offer.plan = plan
        return offer
    }
    func check(_ name: String) -> HandleAvailability {
        let reason: String? = ["admin", "support"].contains(name) ? "reserved" : name == "taken" || handles.contains(where: { $0.namespace == "/" + name }) ? "taken" : nil
        return HandleAvailability(name: name, available: reason == nil, reason: reason)
    }
    func claim(_ id: String, _ name: String?, purchases: FixtureHandlePurchases) throws -> HandleOffer {
        claims += 1
        if state == "retry" && claims == 1 { throw APIError(status: 503, code: "appstore_unavailable", detail: nil) }
        guard let transaction = purchases.values.first(where: { $0.id == id || $0.originalId == id }),
              let capacity = HandleCatalog.capacity(transaction.productId) else { throw HandlePurchaseError.unverified }
        plan = HandlePlan(originalTransactionId: transaction.originalId, productId: transaction.productId, environment: "Sandbox",
            expiresAt: Int(transaction.expiresAt!.timeIntervalSince1970), capacity: capacity, active: true)
        for index in handles.indices { handles[index].productId = transaction.productId; handles[index].active = index < capacity }
        if let name, !handles.contains(where: { $0.namespace == "/" + name }) {
            guard check(name).available else { throw APIError(status: 409, code: "namespace_taken", detail: nil) }
            guard handles.filter(\.active).count < capacity else { throw APIError(status: 409, code: "plan_capacity_reached", detail: nil) }
            handles.append(PurchasedHandle(originalTransactionId: transaction.originalId, namespace: "/" + name,
                productId: transaction.productId, environment: "Sandbox", expiresAt: plan!.expiresAt, active: true))
        }
        var value = HandleOffer(productId: nil, namespace: name.map { "/" + $0 }, expiresAt: plan!.expiresAt)
        value.plan = plan
        return value
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
    init(state: String) {
        self.state = state
        let count = state == "ten" ? 10 : state == "owned" ? 1 : 0
        if count > 0 { values = [HandleTransaction(id: "fixture-initial", originalId: "fixture-plan",
            productId: HandleCatalog.productIds[count-1], accountToken: HandleFixtureBackend.token,
            expiresAt: Date().addingTimeInterval(365 * 86400), revoked: false)] }
    }
    func products(_ ids: [String]) async throws -> [HandleProduct] {
        if state == "soon" { return [] }
        return ids.enumerated().map { index, id in
            HandleProduct(id: id, displayPrice: "$\((index+1)*8).00", price: Decimal((index+1)*8), currencyCode: "USD",
                          displayName: "\(index+1) \(index == 0 ? "name" : "names") — yearly")
        }
    }
    func purchase(_ id: String, accountToken: UUID) async throws -> HandlePurchaseResult {
        if state == "cancelled" { return .cancelled }
        if state == "pending" { return .pending }
        let transaction = HandleTransaction(id: UUID().uuidString, originalId: "fixture-plan", productId: id,
            accountToken: accountToken, expiresAt: Date().addingTimeInterval(365 * 86400), revoked: false)
        values = [transaction]
        return .purchased(transaction)
    }
    func transactions() async -> [HandleTransaction] { values }
    func restore() async throws {}
    func finish(_ id: String) async {}
}
#endif
