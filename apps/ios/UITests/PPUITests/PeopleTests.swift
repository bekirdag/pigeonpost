import XCTest

final class PeopleUITests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
    override func setUpWithError() throws { continueAfterFailure = false; XCUIDevice.shared.orientation = .portrait }
    override func tearDownWithError() throws {
        if let run = testRun, run.failureCount > 0 {
            let image = XCTAttachment(screenshot: app.screenshot()); image.lifetime = .keepAlways; add(image)
            let tree = XCTAttachment(string: app.debugDescription); tree.lifetime = .keepAlways; add(tree)
        }
    }
    private func launch(_ extra: [String] = []) {
        app.terminate()
        app.launchArguments = ["-fixtures", "-appearance=light", "-features=display_names,phone_numbers,contact_discovery,sms_invites"] + extra
        app.launch()
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 10))
    }
    private func item(_ id: String) -> XCUIElement { app.descendants(matching: .any).matching(identifier: id).firstMatch }
    private func tap(_ item: XCUIElement) {
        XCTAssertTrue(item.waitForExistence(timeout: 12))
        for _ in 0..<6 {
            if item.isHittable { break }
            app.swipeUp()
        }
        XCTAssertTrue(item.isHittable); item.tap()
    }
    func testFreeNameSave() {
        launch(["-sheet=name"])
        let field = app.textFields["people-name"]
        tap(field); field.typeText("Marina")
        let save = item("people-save-name")
        XCTAssertTrue(save.isEnabled); tap(save)
        XCTAssertTrue(app.staticTexts["Your name is saved."].waitForExistence(timeout: 8))
    }
    func testFirstInboxNameCanBeSkipped() {
        launch()
        tap(item("people-skip-name"))
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: item("people-skip-name"))
        XCTAssertEqual(XCTWaiter.wait(for: [gone], timeout: 8), .completed)
        XCTAssertFalse(item("people-save-name").exists)
    }
    func testContactDisclosureCanBeCancelled() {
        launch(["-sheet=find-people"])
        tap(item("contacts-choose"))
        XCTAssertTrue(item("contacts-disclosure-continue").waitForExistence(timeout: 5))
        tap(app.buttons["Cancel"].firstMatch)
        XCTAssertTrue(item("contacts-choose").waitForExistence(timeout: 5))
        XCTAssertFalse(item("contacts-disclosure-continue").exists)
        XCTAssertFalse(app.alerts.firstMatch.exists)
    }
    func testPhoneSettingsDoNotSendUntilRequested() {
        launch(["-sheet=phone"])
        XCTAssertTrue(item("people-phone").waitForExistence(timeout: 10))
        XCTAssertFalse(item("people-send-code").isEnabled)
        XCTAssertFalse(item("people-code").exists)
        XCTAssertTrue(app.staticTexts["No verified numbers yet."].waitForExistence(timeout: 5))
    }
    func testDeniedContactsPermissionOffersSettings() {
        app.terminate()
        app.resetAuthorizationStatus(for: .contacts)
        launch(["-sheet=find-people"])
        tap(item("contacts-choose")); tap(item("contacts-disclosure-continue"))
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let deny = springboard.buttons.matching(NSPredicate(format: "label IN %@", ["Don’t Allow", "Don't Allow"])).firstMatch
        XCTAssertTrue(deny.waitForExistence(timeout: 10)); deny.tap()
        XCTAssertTrue(item("contacts-settings").waitForExistence(timeout: 10))
        XCTAssertFalse(item("contacts-find").exists)
    }
    func testPhoneVerificationUsesSixDigitCode() {
        launch(["-sheet=phone"])
        let phone = app.textFields["people-phone"]; tap(phone); phone.typeText("+442079460123")
        let send = item("people-send-code")
        XCTAssertTrue(send.waitForExistence(timeout: 12))
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true AND hittable == true"), object: send)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 8), .completed)
        // On iPad the accessibility frame includes the wide, empty part of the List row.
        // Touch the visible leading label, while keeping the same one-tap send assertion.
        send.coordinate(withNormalizedOffset: CGVector(dx: 0.25, dy: 0.5)).tap()
        let code = app.textFields["people-code"]; tap(code); code.typeText("123456")
        tap(item("people-verify"))
        XCTAssertTrue(app.staticTexts["Your phone number is verified."].waitForExistence(timeout: 10))
        XCTAssertFalse(item("people-code").exists)
    }
    func testSmsInviteHasSeparateCancelAndConfirmation() {
        launch(["-sheet=find-people", "-people-discovery-fixtures"])
        tap(item("contacts-find"))
        let invite = item("contacts-invite-7c0dc19d3eae376a630cee358d0f8912")
        tap(invite)
        XCTAssertTrue(item("contacts-confirm-invite").waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Invitation sent."].exists)
        tap(app.buttons["Cancel"].firstMatch)
        XCTAssertFalse(item("contacts-confirm-invite").exists)
        XCTAssertFalse(app.staticTexts["Invitation sent."].exists)
        tap(invite); tap(item("contacts-confirm-invite"))
        XCTAssertTrue(app.staticTexts["Invitation sent."].waitForExistence(timeout: 10))
    }

}
