//  Nest purchases (Nests B1/B2): the postbox is asked before any sheet, and a StoreKit transaction
//  is finished only after the postbox accepted it — so a paid purchase interrupted before the
//  claim comes back on the next launch. Plain swiftc, like HandleStoreTests.

import Foundation

@MainActor
private final class Purchases: NestPurchasing {
    let updates = AsyncStream<HandleTransaction> { _ in }
    var log: [String] = []
    var held: [HandleTransaction] = []
    var outcome = "success"
    var tokenUsed: UUID?
    func products(_ ids: [String]) async throws -> [HandleProduct] {
        ids.map { HandleProduct(id: $0, displayPrice: "$10.00", price: 10, currencyCode: "USD") }
    }
    func purchase(_ id: String, accountToken: UUID) async throws -> HandlePurchaseResult {
        log.append("purchase:\(id)")
        tokenUsed = accountToken
        if outcome == "cancelled" { return .cancelled }
        if outcome == "pending" { return .pending }
        let t = HandleTransaction(id: "t\(held.count + 1)", originalId: "o1", productId: id, accountToken: accountToken,
                                  expiresAt: Date().addingTimeInterval(86400), revoked: false)
        held.append(t)
        return .purchased(t)
    }
    func transactions() async -> [HandleTransaction] { held }
    func restore() async throws { log.append("restore") }
    func finish(_ id: String) async {
        log.append("finish:\(id)")
        held.removeAll { $0.id == id }
    }
}

@MainActor
private final class Backend {
    let token = UUID()
    let purchases = Purchases()
    var claimError: APIError?
    var intentError: APIError?
    var log: [String] { purchases.log }
    var funded = false
    let package = NestPackage(code: "nest.ai.starter.monthly", name: "Pigeonpost AI", rank: 2, slots: 1, storageBytes: 5 << 30,
                              activeHours: 10, inputTokens: 1_000_000, outputTokens: 100_000, model: "managed", sellable: true,
                              appleProductId: "dev.pigeonpost.inbox.nests.ai.starter.monthly")
    let pro = NestPackage(code: "nest.ai.pro.monthly", name: "Pro", rank: 4, slots: 10, storageBytes: 1, activeHours: 1,
                          inputTokens: 1, outputTokens: 1, model: "managed", sellable: false,
                          appleProductId: "dev.pigeonpost.inbox.nests.ai.pro.monthly")
    func snapshot() -> NestSnapshot {
        NestSnapshot(allowance: NestAllowance(funding: funded ? NestFunding(entitlementId: "e", source: "apple", package: package.code, status: "active", paidThrough: nil) : nil,
                                              slots: funded ? 1 : 0, used: 0, duplicates: []),
                     package: funded ? package : nil, nests: [], setups: [])
    }
    func store() -> NestStore {
        NestStore(services: NestServices(
            subject: { "acct" },
            catalog: { NestCatalog(catalogVersion: "v", packages: [self.package, self.pro],
                                   rails: .init(apple: true, google: false, web: true), appAccountToken: self.token.uuidString) },
            snapshot: { self.snapshot() },
            intent: { code in
                self.purchases.log.append("intent:\(code)")
                if let e = self.intentError { throw e }
                return NestIntent(intentId: "i", appleProductId: self.package.appleProductId, appAccountToken: self.token.uuidString)
            },
            claim: { id in
                self.purchases.log.append("claim:\(id)")
                if let e = self.claimError { throw e }
                self.funded = true
                return self.snapshot()
            },
            retry: { _ in self.snapshot() },
            purchases: purchases))
    }
}

@main
@MainActor
struct NestStoreTests {
    static var checks = 0
    static func expect(_ value: @autoclosure () -> Bool, _ what: String, line: UInt = #line) {
        checks += 1
        if !value() { fatalError("Nest test line \(line): \(what)") }
    }

    static func main() async {
        // Happy path: intent → purchase with the account token → claim → finish, in that order.
        do {
            let api = Backend(), store = api.store()
            await store.refresh()
            expect(store.forSale.map(\.code) == ["nest.ai.starter.monthly"], "unsellable packages are not offered")
            expect(store.price(api.package) == "$10.00", "the price is the App Store's")
            await store.buy(api.package)
            expect(api.log == ["intent:nest.ai.starter.monthly", "purchase:dev.pigeonpost.inbox.nests.ai.starter.monthly", "claim:t1", "finish:t1"],
                   "order: \(api.log)")
            expect(api.purchases.tokenUsed == api.token, "the purchase carries the account's appAccountToken")
            expect(store.isCurrent(api.package) && !store.awaitingClaim && store.message == nil, "funded after the claim")
        }
        // The postbox refuses a second charge: no App Store sheet at all.
        do {
            let api = Backend(), store = api.store()
            await store.refresh()
            api.intentError = APIError(status: 409, code: "nest_package_active", detail: nil)
            await store.buy(api.package)
            expect(!api.log.contains { $0.hasPrefix("purchase:") }, "no purchase after a refused intent")
            expect(store.message?.contains("bought elsewhere") == true, "the refusal is said plainly")
        }
        // The postbox is down after payment: the transaction stays unfinished, and the next launch
        // claims and finishes it (closed-app recovery).
        do {
            let api = Backend(), store = api.store()
            await store.refresh()
            api.claimError = APIError(status: 503, code: "billing_unavailable", detail: nil)
            await store.buy(api.package)
            expect(!api.log.contains("finish:t1"), "never finished before the postbox accepted it")
            expect(store.awaitingClaim && api.purchases.held.count == 1, "kept for later")
            api.claimError = nil
            let relaunched = api.store()
            await relaunched.refresh()
            expect(api.log.suffix(2) == ["claim:t1", "finish:t1"], "recovered on the next launch: \(api.log)")
            expect(relaunched.isCurrent(api.package) && !relaunched.awaitingClaim, "and funded")
        }
        // A definite refusal (bound to another account) finishes it: retrying would never succeed.
        do {
            let api = Backend(), store = api.store()
            await store.refresh()
            api.claimError = APIError(status: 409, code: "purchase_already_used", detail: nil)
            await store.buy(api.package)
            expect(api.log.last == "finish:t1", "a refused purchase is finished")
            expect(store.message?.contains("another Pigeonpost account") == true, "and explained")
        }
        // Apple has not confirmed it yet: kept, not finished.
        do {
            let api = Backend(), store = api.store()
            await store.refresh()
            api.claimError = APIError(status: 409, code: "purchase_inactive", detail: nil)
            await store.buy(api.package)
            expect(!api.log.contains("finish:t1"), "an unconfirmed payment is kept for later")
        }
        // A transaction made for another Pigeonpost account is neither claimed nor finished.
        do {
            let api = Backend(), store = api.store()
            api.purchases.held = [HandleTransaction(id: "x", originalId: "x", productId: api.package.appleProductId,
                                                   accountToken: UUID(), expiresAt: nil, revoked: false)]
            await store.refresh()
            expect(!api.log.contains("claim:x") && !api.log.contains("finish:x"), "someone else's purchase is left alone")
        }
        // Cancel and Ask to Buy grant nothing and claim nothing.
        for outcome in ["cancelled", "pending"] {
            let api = Backend(), store = api.store()
            await store.refresh()
            api.purchases.outcome = outcome
            await store.buy(api.package)
            expect(!api.log.contains { $0.hasPrefix("claim:") }, "\(outcome): no claim")
            expect(!store.isCurrent(api.package), "\(outcome): nothing granted")
        }
        // Restore syncs with Apple and claims what is there.
        do {
            let api = Backend(), store = api.store()
            await store.refresh()
            api.purchases.held = [HandleTransaction(id: "r1", originalId: "o", productId: api.package.appleProductId,
                                                   accountToken: api.token, expiresAt: nil, revoked: false)]
            await store.restore()
            expect(api.log.contains("restore") && api.log.contains("claim:r1") && api.log.contains("finish:r1"), "restore claims: \(api.log)")
        }
        expect(NestWords.fileName("Q3 plan/../é.pdf") == "Q3%20plan%2F..%2F%C3%A9.pdf", "file names are percent-encoded")
        expect(NestWords.fileName("a\u{7}b\nc") == "abc", "control characters are dropped")
        expect(NestWords.fileName(String(repeating: "x", count: 300)).count == 200, "at most 200 characters")
        expect(NestWords.refusal(APIError(status: 409, code: "approval_changed", detail: nil)).contains("changed"), "approval refusals are explained")
        print("nest store: \(checks) checks passed")
    }
}
