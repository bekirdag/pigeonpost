import XCTest
import StoreKit
import StoreKitTest

/// Exercises the shipped Apple adapter with Apple's local StoreKit service. No real charges.
@MainActor
final class NativeHandleStoreKitTests: XCTestCase {
    func testTenIndependentAnnualPurchasesRestoreWithoutReplacingEachOther() async throws {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "HandleSubscriptions", withExtension: "storekit"))
        let session = try SKTestSession(contentsOf: url)
        session.resetToDefaultState()
        session.disableDialogs = true
        session.timeRate = .realTime
        session.clearTransactions()
        defer { session.clearTransactions() }
        let ids = ["dev.pigeonpost.inbox.handle.yearly"] + (2...10).map { "dev.pigeonpost.inbox.handle\($0).yearly" }
        let apple = AppleHandlePurchases()
        let products = try await apple.products(ids)
        XCTAssertEqual(products.map(\.id), ids)
        XCTAssertEqual(products.reduce(Decimal(0)) { $0 + $1.price }, 80)
        XCTAssertTrue(products.allSatisfy { $0.price == 8 && $0.currencyCode == "USD" })
        let token = UUID()
        var originals: Set<String> = []
        for id in ids {
            guard case let .purchased(transaction) = try await apple.purchase(id, accountToken: token) else {
                return XCTFail("Apple did not complete \(id)")
            }
            XCTAssertTrue(transaction.active)
            XCTAssertEqual(transaction.accountToken, token)
            originals.insert(transaction.originalId)
            await apple.finish(transaction.id)
        }
        XCTAssertEqual(originals.count, 10)
        let restored = await apple.transactions()
        XCTAssertEqual(Set(restored.filter(\.active).map(\.productId)), Set(ids))
        XCTAssertEqual(Set(restored.map(\.originalId)), originals)
        try session.expireSubscription(productIdentifier: ids[0])
        var remaining = await apple.transactions()
        for _ in 0..<20 where remaining.filter(\.active).count == 10 {
            try await Task.sleep(nanoseconds: 100_000_000)
            remaining = await apple.transactions()
        }
        XCTAssertEqual(Set(remaining.filter(\.active).map(\.productId)), Set(ids.dropFirst()), "Expiring one name preserves the other nine")
    }
}
