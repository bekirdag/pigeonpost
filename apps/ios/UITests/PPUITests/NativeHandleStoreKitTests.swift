import XCTest
import StoreKit
import StoreKitTest

/// Local StoreKit validation is separate from the required physical sandbox recording.
@MainActor
final class NativeHandleStoreKitTests: XCTestCase {
    func testOneGroupUpgradesRestoresAndExpiresAsOnePlan() async throws {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "HandleSubscriptions", withExtension: "storekit"))
        let session = try SKTestSession(contentsOf: url)
        session.resetToDefaultState()
        session.disableDialogs = true
        session.timeRate = .realTime
        session.clearTransactions()
        defer { session.clearTransactions() }
        let ids = HandleCatalog.productIds
        let apple = AppleHandlePurchases()
        let products = try await apple.products(ids)
        XCTAssertEqual(products.map(\.id), ids)
        XCTAssertEqual(products.map(\.price), (1...10).map { Decimal($0 * 8) })
        let native = try await Product.products(for: ids)
        XCTAssertEqual(Set(native.compactMap { $0.subscription?.subscriptionGroupID }).count, 1)
        let token = UUID()
        var original: String?
        for index in [0, 4, 9] {
            guard case let .purchased(transaction) = try await apple.purchase(ids[index], accountToken: token) else {
                return XCTFail("Apple did not complete the plan purchase")
            }
            XCTAssertEqual(transaction.accountToken, token)
            if let original { XCTAssertEqual(transaction.originalId, original) }
            original = transaction.originalId
            await apple.finish(transaction.id)
            let current = await apple.transactions()
            XCTAssertEqual(Set(current.filter(\.active).map(\.productId)), [ids[index]], "An upgrade replaces the previous level")
        }
        try await apple.restore()
        let restored = await apple.transactions()
        XCTAssertEqual(Set(restored.filter(\.active).map(\.productId)), [ids[9]])
        _ = try await apple.purchase(ids[0], accountToken: token)
        let afterDowngrade = await apple.transactions()
        XCTAssertEqual(Set(afterDowngrade.filter(\.active).map(\.productId)), [ids[9]])
        let ten = try XCTUnwrap(native.first { $0.id == ids[9] })
        let statuses = try await ten.subscription!.status
        XCTAssertTrue(statuses.contains { status in
            if case let .verified(renewal) = status.renewalInfo { return renewal.autoRenewPreference == ids[0] }
            return false
        })
        try session.expireSubscription(productIdentifier: ids[9])
        var remaining = await apple.transactions()
        for _ in 0..<20 where remaining.contains(where: \.active) {
            try await Task.sleep(nanoseconds: 100_000_000)
            remaining = await apple.transactions()
        }
        XCTAssertFalse(remaining.contains(where: \.active))
    }
}
