import Foundation

@MainActor
private final class Purchases: HandlePurchasing {
    let updates = AsyncStream<HandleTransaction> { _ in }
    var values: [HandleTransaction] = []
    var calls = 0
    var finished: [String] = []
    var outcome = "success"
    func products(_ ids: [String]) async throws -> [HandleProduct] {
        ids.map { HandleProduct(id: $0, displayPrice: "$8.00", price: 8, currencyCode: "USD") }
    }
    func purchase(_ id: String, accountToken: UUID) async throws -> HandlePurchaseResult {
        calls += 1
        if outcome == "cancelled" { return .cancelled }
        if outcome == "pending" { return .pending }
        let value = transaction(id, token: accountToken)
        values.append(value)
        return .purchased(value)
    }
    func transaction(_ product: String, token: UUID) -> HandleTransaction {
        let id = UUID().uuidString
        return HandleTransaction(id: id, originalId: id, productId: product, accountToken: token,
            expiresAt: Date().addingTimeInterval(86400), revoked: false)
    }
    func transactions() async -> [HandleTransaction] { values }
    func restore() async throws {}
    func finish(_ id: String) async { finished.append(id) }
}

@MainActor
private final class Backend {
    let ids = ["dev.pigeonpost.inbox.handle.yearly"] + (2...10).map { "dev.pigeonpost.inbox.handle\($0).yearly" }
    let token = UUID()
    let purchases = Purchases()
    let defaults = UserDefaults(suiteName: "handle-tests-" + UUID().uuidString)!
    var subject: String? = "account-a"
    var handles: [PurchasedHandle] = []
    var claims: [(String, String?)] = []
    var mailboxes: Set<String> = []
    var failures = 0
    var probe: ((String) async -> HandleAvailability)?
    var offerHook: (() async -> Void)?
    var accountRows: [AccountHandle] = []
    var ownershipHook: (() async throws -> Void)?
    var storeUnavailable = false
    func offer() -> HandleOffer {
        var value = HandleOffer(productId: ids[0], namespace: handles.first?.namespace, expiresAt: handles.first?.expiresAt)
        value.productIds = ids; value.maxHandles = 10; value.appAccountToken = token.uuidString
        value.account = subject; value.handles = handles
        return value
    }
    func claim(_ id: String, _ name: String?) throws -> HandleOffer {
        claims.append((id, name))
        if failures > 0 { failures -= 1; throw APIError(status: 503, code: "appstore_unavailable", detail: nil) }
        let transaction = purchases.values.first { $0.id == id }!
        let handle: PurchasedHandle
        if let owned = handles.first(where: { $0.originalTransactionId == transaction.originalId }) {
            handle = owned
        } else {
            guard let name else { throw APIError(status: 400, code: "name_required", detail: nil) }
            handle = PurchasedHandle(originalTransactionId: transaction.originalId, namespace: "/" + name,
                productId: transaction.productId, environment: "Sandbox", expiresAt: Int(transaction.expiresAt!.timeIntervalSince1970), active: true)
            handles.append(handle)
        }
        return HandleOffer(productId: handle.productId, namespace: handle.namespace, expiresAt: handle.expiresAt)
    }
    func store() -> HandleStore {
        HandleStore(services: HandleServices(subject: { self.subject }, offer: {
            await self.offerHook?()
            if self.storeUnavailable { throw APIError(status: 404, code: "not_found", detail: nil) }
            return self.offer()
        }, availability: { name in
            if let probe = self.probe { return await probe(name) }
            return HandleAvailability(name: name, available: name != "taken", reason: name == "taken" ? "taken" : nil)
        }, claim: { try self.claim($0, $1) }, ensureMailbox: { self.mailboxes.insert($0); return true },
            purchases: purchases, accountHandles: { try await self.ownershipHook?(); return self.accountRows }, defaults: defaults, timeout: 0.3))
    }
}

@main
@MainActor
struct HandleStoreTests {
    static var checks = 0
    static func expect(_ value: @autoclosure () -> Bool, _ description: String, line: UInt = #line) {
        checks += 1
        if !value() { fatalError("Handle test line \(line): \(description)") }
    }
    static func ready(_ store: HandleStore, _ name: String) async {
        store.wantedName = name
        await store.checkAvailability()
    }
    static func main() async throws {
        do {
            let api = Backend(), store = api.store()
            api.accountRows = [AccountHandle(namespace: "apple", source: "apple", expiresAt: 300, active: true),
                AccountHandle(namespace: "google", source: "google", expiresAt: 200, active: false),
                AccountHandle(namespace: "web", source: "entitlement", expiresAt: nil, active: true)]
            api.storeUnavailable = true
            await store.refresh()
            expect(store.accountHandles == api.accountRows, "all sources load even while Apple store is unavailable")
            expect(!store.accountHandles[1].active, "expired Google registration stays inactive")
            expect(store.activeCount == 0, "cross-store names never occupy Apple product slots")
            api.ownershipHook = { throw APIError(status: 503, code: "unavailable", detail: nil) }
            await store.refresh()
            expect(store.ownershipError != nil && store.accountHandles.count == 3, "ownership failure preserves prior data and shows an error")
            api.ownershipHook = { try? await Task.sleep(nanoseconds: 40_000_000) }
            let task = Task { await store.refresh() }
            try await Task.sleep(nanoseconds: 5_000_000)
            store.reset(); api.subject = "account-b"
            await task.value
            expect(store.accountHandles.isEmpty && !store.ownershipLoaded, "late old-account ownership cannot survive sign-out")
        }
        for invalid in ["bad/name", "-alice", "alice-", "álice", "k", "gh", String(repeating: "a", count: 33)] {
            expect(!HandleStore.valid(invalid), "invalid name: \(invalid)")
        }
        expect(HandleStore.valid(" /Alice.Name/ "), "normalizes leading slash and case")
        do {
            let api = Backend(), store = api.store()
            await store.refresh()
            for n in 1...10 {
                await ready(store, "name\(n)")
                expect(store.canBuy, "can buy handle \(n)")
                await store.buy()
                expect(store.activeCount == n, "handle \(n) registered")
                expect(store.wantedName.isEmpty, "successful name cleared")
            }
            await ready(store, "eleven")
            expect(!store.canBuy, "no eleventh purchase")
            await store.buy()
            expect(api.purchases.calls == 10, "exactly ten Apple confirmations")
            expect(Set(api.handles.map(\.productId)).count == 10, "independent product per name")
            expect(api.mailboxes.count == 10, "all purchased inboxes created")
            expect(store.products[0].price * 10 == 80, "ten handles cost USD 80")
            store.wantedName = "unrelated"
            await store.refresh(restoring: true)
            expect(store.activeCount == 10, "restores all ten")
            expect(api.claims.suffix(10).allSatisfy { $0.1 == nil }, "restoration uses server binding, never current form")
        }
        do {
            let api = Backend(), store = api.store()
            await store.refresh()
            await ready(store, "taken")
            expect(!store.canBuy, "taken name cannot charge")
            await ready(store, "free")
            store.wantedName = "changed"
            expect(!store.canBuy, "availability belongs to the checked name")
            api.probe = { name in
                try? await Task.sleep(nanoseconds: 40_000_000)
                return HandleAvailability(name: name, available: true, reason: nil)
            }
            let task = Task { await store.checkAvailability() }
            try await Task.sleep(nanoseconds: 5_000_000)
            store.wantedName = "newname"
            await task.value
            expect(store.availability == nil, "late availability is discarded")
        }
        do {
            let api = Backend(), store = api.store()
            await store.refresh(); await ready(store, "cosmos")
            api.purchases.outcome = "cancelled"
            await store.buy()
            expect(store.wantedName == "cosmos" && !store.busy, "cancellation preserves editable name")
            api.purchases.outcome = "success"; api.failures = 1
            await store.buy()
            expect(store.unassigned.count == 1 && api.purchases.finished.isEmpty, "failed claim keeps unfinished payment")
            let charged = api.purchases.calls
            await store.buy()
            expect(api.purchases.calls == charged, "claim retry never charges again")
            expect(store.activeCount == 1 && store.unassigned.isEmpty, "claim retry completes")
        }
        do {
            let api = Backend()
            var store = api.store()
            await store.refresh(); await ready(store, "pendingname")
            api.purchases.outcome = "pending"
            await store.buy()
            expect(store.waitingForApproval && !store.canBuy, "pending confirmation cannot charge twice")
            store.reset()
            api.purchases.values = [api.purchases.transaction(api.ids[0], token: api.token)]
            store = api.store()
            await store.refresh()
            expect(store.handles.first?.namespace == "/pendingname", "approved purchase keeps name across app restart")
            expect(!store.waitingForApproval, "successful approval clears pending state")
        }
        do {
            let api = Backend(), store = api.store()
            api.purchases.values = [api.purchases.transaction(api.ids[0], token: UUID())]
            await store.refresh(restoring: true)
            expect(api.claims.isEmpty, "another Pigeonpost account's purchase is never claimed")
            api.offerHook = { try? await Task.sleep(nanoseconds: 50_000_000) }
            let refresh = Task { await store.refresh() }
            try await Task.sleep(nanoseconds: 5_000_000)
            store.reset(); api.subject = "account-b"
            await refresh.value
            expect(!store.loaded && store.handles.isEmpty && store.products.isEmpty, "sign-out drops stale responses")
        }
        do {
            let api = Backend(), store = api.store()
            api.purchases.values = [api.purchases.transaction(api.ids[0], token: api.token)]
            await store.refresh(restoring: true)
            expect(api.claims.first?.1 == nil, "unassigned restore never invents a name")
            expect(store.unassigned.count == 1, "unassigned payment asks for a name")
            await ready(store, "chosen")
            await store.buy()
            expect(api.purchases.calls == 0 && store.activeCount == 1, "restored payment assigned without charging")
        }
        let started = Date()
        do {
            let _: Int = try await withHandleDeadline(seconds: 0.03) {
                await withCheckedContinuation { continuation in
                    DispatchQueue.global().asyncAfter(deadline: .now() + 0.2) { continuation.resume(returning: 1) }
                }
            }
            fatalError("deadline should fail")
        } catch HandlePurchaseError.timedOut { }
        expect(Date().timeIntervalSince(started) < 0.15, "deadline returns even if work ignores cancellation")
        print("Handle purchase controller: \(checks) checks passed")
    }
}
