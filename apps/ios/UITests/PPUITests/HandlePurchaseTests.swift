import XCTest

final class HandlePurchaseTests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func open(_ state: String) {
        app.launchArguments = ["-fixtures", "-sheet=settings", "-handle=\(state)"]
        app.launch()
        app.buttons["settings-handles"].tap()
        app.buttons["settings-purchases"].tap()
    }

    private func screenshot(_ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testSettingsMenuHasFocusedPagesAndAccountActions() {
        app.launchArguments = ["-fixtures", "-sheet=settings", "-handle=owned"]
        app.launch()
        XCTAssertTrue(app.buttons["settings-account"].waitForExistence(timeout: 8))
        XCTAssertFalse(app.textFields["yourname"].exists)
        XCTAssertFalse(app.buttons["Restore purchases"].exists)
        screenshot("settings-menu")
        app.buttons["settings-account"].tap()
        XCTAssertTrue(app.links["deleteAccount"].exists || app.buttons["deleteAccount"].exists)
        XCTAssertTrue(app.buttons["Sign out"].exists)
        app.navigationBars["Account"].buttons.element(boundBy: 0).tap()
        app.buttons["settings-handles"].tap()
        XCTAssertTrue(app.staticTexts["account-handle-previous"].waitForExistence(timeout: 8))
        screenshot("settings-handles")
        app.navigationBars["Handles"].buttons.element(boundBy: 0).tap()
        app.buttons["settings-contacts"].tap()
        XCTAssertTrue(app.buttons["Add a sender"].waitForExistence(timeout: 8))
        app.navigationBars["Contacts and permissions"].buttons.element(boundBy: 0).tap()
        app.buttons["settings-help"].tap()
        screenshot("settings-help")
        app.buttons["Done"].tap()
        XCTAssertFalse(app.navigationBars["Help and about"].exists)
    }

    func testBackNavigationPreservesUnfinishedHandleName() {
        open("sale")
        enter("cosmos")
        app.navigationBars["Get a handle"].buttons.element(boundBy: 0).tap()
        app.buttons["settings-purchases"].tap()
        XCTAssertEqual(app.textFields["yourname"].value as? String, "cosmos")
    }

    func testInvalidNameCannotBePurchased() {
        open("sale")
        let field = app.textFields["yourname"]
        XCTAssertTrue(field.waitForExistence(timeout: 8))
        field.tap()
        let existing = field.value as? String ?? ""
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: existing.count))
        field.typeText("bad/name")
        screenshot("invalid-name")
        let buy = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Buy for'")).firstMatch
        XCTAssertTrue(buy.exists)
        XCTAssertFalse(buy.isEnabled, "A malformed name must never open the Apple payment sheet")
    }

    func testOwnedHandleCanAddAnother() {
        open("owned")
        screenshot("existing-owner")
        XCTAssertTrue(app.textFields["yourname"].waitForExistence(timeout: 8),
                      "An existing owner needs a way to add another handle")
    }

    private var buy: XCUIElement {
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Buy for'")).firstMatch
    }

    private func enter(_ name: String) {
        let field = app.textFields["yourname"]
        XCTAssertTrue(field.waitForExistence(timeout: 8))
        field.tap()
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: (field.value as? String ?? "").count))
        field.typeText(name)
        if app.keyboards.buttons["Done"].exists { app.keyboards.buttons["Done"].tap() }
    }

    private func waitForEnabledBuy() {
        let enabled = NSPredicate(format: "enabled == true")
        expectation(for: enabled, evaluatedWith: buy)
        waitForExpectations(timeout: 8)
    }

    func testAvailableNameRegistersAndOpensInbox() {
        open("sale")
        enter("cosmos")
        waitForEnabledBuy()
        buy.tap()
        let inbox = app.buttons["handle-inbox-/cosmos"]
        XCTAssertTrue(inbox.waitForExistence(timeout: 8))
        screenshot("registered-handle")
        inbox.tap()
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: app.navigationBars["Get a handle"])
        waitForExpectations(timeout: 5)
    }

    func testTakenNameNeverEnablesPayment() {
        open("sale")
        enter("taken")
        XCTAssertTrue(app.staticTexts["That name is already taken. Choose another."].waitForExistence(timeout: 8))
        XCTAssertFalse(buy.isEnabled)
        screenshot("taken-name")
    }

    func testCancelledPurchaseKeepsEditableName() {
        open("cancelled")
        enter("cosmos")
        waitForEnabledBuy(); buy.tap()
        XCTAssertTrue(app.staticTexts["Purchase cancelled. Your name is still here."].waitForExistence(timeout: 8))
        XCTAssertEqual(app.textFields["yourname"].value as? String, "cosmos")
        XCTAssertTrue(app.textFields["yourname"].isEnabled)
    }

    func testFailedClaimCanFinishWithoutAnotherPayment() {
        open("retry")
        enter("cosmos")
        waitForEnabledBuy(); buy.tap()
        let finish = app.buttons["Finish registration — no further payment"]
        XCTAssertTrue(finish.waitForExistence(timeout: 8))
        XCTAssertEqual(app.textFields["yourname"].value as? String, "cosmos")
        screenshot("recover-purchase")
        finish.tap()
        XCTAssertTrue(app.buttons["handle-inbox-/cosmos"].waitForExistence(timeout: 8))
    }

    func testPendingApprovalKeepsNameAndDisablesPayment() {
        open("pending")
        enter("cosmos")
        waitForEnabledBuy(); buy.tap()
        XCTAssertTrue(app.staticTexts["Waiting for Apple purchase approval. The name will be registered when approval arrives."].waitForExistence(timeout: 8))
        XCTAssertEqual(app.textFields["yourname"].value as? String, "cosmos")
        XCTAssertFalse(buy.isEnabled)
    }

    func testTenHandlesHaveNoEleventhPurchase() {
        open("ten")
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS '10 of 10'")).firstMatch.waitForExistence(timeout: 8))
        XCTAssertFalse(app.textFields["yourname"].exists)
        XCTAssertFalse(buy.exists)
        screenshot("ten-handles")
    }
}
