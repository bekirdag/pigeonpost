import XCTest

/// Nests on the phone, in fixture mode: the Products page, a Nest's page and the header above its
/// conversation. Every screen exists only while the `nests` capability is on.
final class NestTests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")

    override func setUpWithError() throws { continueAfterFailure = false }

    private func openProducts(_ state: String) {
        app.launchArguments = ["-fixtures", "-sheet=settings", "-nests=\(state)"]
        app.launch()
        let products = app.buttons["settings-products"]
        XCTAssertTrue(products.waitForExistence(timeout: 10), "Products row")
        products.tap()
    }

    private func reveal(_ element: XCUIElement) {
        for _ in 0..<10 where !(element.exists && element.isHittable) { app.swipeUp() }
    }

    func testSaleLists() {
        openProducts("sale")
        let buy = app.buttons["nest-buy-nest.ai.starter.monthly"]
        reveal(buy)
        XCTAssertTrue(buy.exists, "a package to buy")
    }

    func testOwnedNestApprovalAndManagement() {
        openProducts("owned")
        let row = app.buttons["nest-row"]
        reveal(row)
        XCTAssertTrue(row.exists, "the Nest is listed")
        row.tap()
        let approve = app.buttons["nest-approval-approve"]
        XCTAssertTrue(approve.waitForExistence(timeout: 5), "a card waiting")
        XCTAssertTrue(app.staticTexts["nest-approval-body"].exists, "the exact message is shown")
        approve.tap()
        let gone = NSPredicate(format: "exists == false")
        expectation(for: gone, evaluatedWith: approve)
        waitForExpectations(timeout: 5)
        let delete = app.buttons["nest-delete"]
        reveal(delete)
        XCTAssertTrue(app.buttons["nest-pause"].exists, "an owner may pause")
        XCTAssertTrue(delete.exists, "and delete")
    }

    func testConversationHeaderOpensTheNest() {
        app.launchArguments = ["-fixtures", "-nests=owned", "-open=/bekir/agent1"]
        app.launch()
        let header = app.buttons["nest-conversation-header"]
        XCTAssertTrue(header.waitForExistence(timeout: 10), "status above the conversation")
        let waiting = NSPredicate(format: "label CONTAINS %@", "1 waiting for your approval")
        expectation(for: waiting, evaluatedWith: header)
        waitForExpectations(timeout: 5)
        header.tap()
        XCTAssertTrue(app.buttons["nest-approval-approve"].waitForExistence(timeout: 5), "the card, from the conversation")
    }

    func testProductsHiddenWithoutTheCapability() {
        app.launchArguments = ["-fixtures", "-features=", "-sheet=settings", "-nests=owned"]
        app.launch()
        XCTAssertTrue(app.buttons["settings-handles"].waitForExistence(timeout: 10), "Settings is open")
        XCTAssertFalse(app.buttons["settings-products"].exists, "no Products row")
    }

    func testHiddenWithoutTheCapability() {
        app.launchArguments = ["-fixtures", "-features=", "-nests=owned", "-open=/bekir/agent1"]
        app.launch()
        XCTAssertTrue(app.navigationBars.firstMatch.waitForExistence(timeout: 10))
        sleep(2)
        XCTAssertFalse(app.buttons["nest-conversation-header"].exists, "no Nest header")
    }
}
