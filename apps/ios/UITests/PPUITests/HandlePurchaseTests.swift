import XCTest

final class HandlePurchaseTests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func open(_ state: String) {
        app.launchArguments = ["-fixtures", "-sheet=settings", "-handle=\(state)"]
        app.launch()
        let handles = app.buttons["settings-handles"]
        expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: handles)
        waitForExpectations(timeout: 8)
        handles.tap()
        app.buttons["settings-purchases"].tap()
    }

    private func screenshot(_ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testPurchasePageIsDirectlyAvailableFromSettings() {
        app.launchArguments = ["-fixtures", "-sheet=settings", "-handle=sale"]
        app.launch()
        let purchasePage = app.buttons["settings-purchases"]
        expectation(for: NSPredicate(format: "enabled == true"), evaluatedWith: purchasePage)
        waitForExpectations(timeout: 8)
        purchasePage.tap()
        XCTAssertTrue(app.textFields["yourname"].waitForExistence(timeout: 8))
        XCTAssertEqual(app.staticTexts["handle-product-name"].label, "Pigeonpost handle")
        XCTAssertEqual(app.staticTexts["handle-product-price"].label, "$8.00 per year")
        XCTAssertTrue(app.buttons["Buy for $8.00 a year"].exists)
        screenshot("direct-handle-purchase")
    }

    func testUnavailableCatalogOffersRetryWithoutInventingAPrice() {
        open("soon")
        XCTAssertTrue(app.buttons["handle-retry-products"].waitForExistence(timeout: 8))
        XCTAssertTrue(app.buttons["Subscription unavailable"].exists)
        XCTAssertFalse(app.buttons["Subscription unavailable"].isEnabled)
        XCTAssertFalse(app.staticTexts["handle-product-price"].exists)
        XCTAssertFalse(app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Buy for'")).firstMatch.exists)
        app.buttons["handle-retry-products"].tap()
        XCTAssertTrue(app.buttons["handle-retry-products"].waitForExistence(timeout: 8))
        screenshot("subscription-retry")
    }

    private func expectPastedAddress(_ address: String) {
        app.buttons["New conversation"].tap()
        let field = app.textFields["/bekir/agent1 or /k/…"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        field.press(forDuration: 1)
        let paste = app.menuItems["Paste"]
        XCTAssertTrue(paste.waitForExistence(timeout: 5))
        paste.tap()
        XCTAssertEqual(field.value as? String, address)
        app.buttons["Cancel"].tap()
    }

    func testCopyAddressFromPickerAndAccount() {
        app.launchArguments = ["-fixtures", "-handle=owned"]
        app.launch()
        XCTAssertFalse(app.buttons["copy-address:/bekir/main"].exists)
        app.buttons["Acting as /bekir. Change mailbox"].tap()
        let copy = app.buttons["copy-address:/bekir/main"]
        XCTAssertTrue(copy.waitForExistence(timeout: 8))
        copy.tap()
        // XCTest can wait longer for UI idle than the two-second confirmation lasts.
        // Verify the lasting clipboard result below and that copying keeps the picker open.
        XCTAssertTrue(app.navigationBars["Mailboxes"].exists)
        screenshot("mailbox-address-copied")
        app.buttons["Done"].tap()
        expectPastedAddress("/bekir/main")
        app.buttons["Settings"].tap()
        app.buttons["settings-account"].tap()
        app.buttons.matching(identifier: "copy-address:/bekir/main").allElementsBoundByIndex.last!.tap()
        screenshot("account-copy-address")
        app.buttons["Done"].tap()
        expectPastedAddress("/bekir/main")
    }

    func testCopyOtherMailboxAndUnnamedAddressKeepsCurrentInbox() {
        app.launchArguments = ["-fixtures", "-sheet=identities"]
        app.launch()
        let named = app.buttons["copy-address:/bekir/docdex"]
        XCTAssertTrue(named.waitForExistence(timeout: 8))
        named.tap()
        XCTAssertTrue(app.navigationBars["Mailboxes"].exists)
        app.buttons["Done"].tap()
        expectPastedAddress("/bekir/docdex")
        app.buttons["Acting as /bekir. Change mailbox"].tap()
        let address = "/k/qq2222v2h90vnwefj7g7ezvbh7"
        app.buttons["copy-address:\(address)"].tap()
        screenshot("mailbox-copy-address")
        app.buttons["Done"].tap()
        XCTAssertFalse(app.buttons["copy-address:/bekir/main"].exists)
        expectPastedAddress(address)
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
        XCTAssertFalse(app.staticTexts["/alex"].exists, "Owned names belong under Handles")
        XCTAssertFalse(app.staticTexts["Active App Store subscriptions"].exists)
        XCTAssertTrue(app.textFields["yourname"].waitForExistence(timeout: 8),
                      "An existing owner needs a way to add another handle")
        XCTAssertEqual(app.staticTexts["handle-product-name"].label, "Handle 2 — yearly")
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
        XCTAssertTrue(app.staticTexts["/cosmos is ready."].waitForExistence(timeout: 8))
        app.navigationBars["Get a handle"].buttons.element(boundBy: 0).tap()
        let inbox = app.buttons["Open /cosmos"]
        XCTAssertTrue(inbox.waitForExistence(timeout: 8))
        screenshot("registered-handle")
        inbox.tap()
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: app.navigationBars["Get a handle"])
        waitForExpectations(timeout: 5)
    }

    func testEveryHandleSubscriptionIsListedAndCanBeBought() {
        open("sale")
        let ids = ["dev.pigeonpost.inbox.handle.yearly"] + (2...10).map { "dev.pigeonpost.inbox.handle\($0).yearly" }
        XCTAssertTrue(app.buttons["handle-product-" + ids[0]].waitForExistence(timeout: 8))
        screenshot("all-handle-products")
        for id in ids {
            let row = app.buttons["handle-product-" + id]
            for _ in 0..<6 where !row.isHittable { app.swipeUp() }
            XCTAssertTrue(row.isHittable, "\(id) must be visible on the purchase screen")
        }
        app.buttons["handle-product-" + ids[4]].tap()
        for _ in 0..<6 where !app.textFields["yourname"].isHittable { app.swipeDown() }
        XCTAssertEqual(app.staticTexts["handle-product-name"].label, "Handle 5 — yearly")
        enter("fifth")
        waitForEnabledBuy()
        buy.tap()
        XCTAssertTrue(app.staticTexts["/fifth is ready."].waitForExistence(timeout: 8))
        let bought = app.buttons["handle-product-" + ids[4]]
        for _ in 0..<6 where !bought.isHittable { app.swipeUp() }
        XCTAssertFalse(bought.isEnabled, "A subscription already bought cannot be chosen again")
        screenshot("fifth-handle-bought")
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
        XCTAssertTrue(app.staticTexts["/cosmos is ready."].waitForExistence(timeout: 8))
        app.navigationBars["Get a handle"].buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.buttons["Open /cosmos"].waitForExistence(timeout: 8))
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
        XCTAssertTrue(app.staticTexts["You have all ten handle subscriptions."].waitForExistence(timeout: 8))
        XCTAssertFalse(app.textFields["yourname"].exists)
        XCTAssertFalse(buy.exists)
        screenshot("ten-handles")
    }
}
