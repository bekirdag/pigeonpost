import XCTest

/// Drives the real confirmation store/API/UI against a reserved .invalid fixture origin.
final class DeviceApprovalUITests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
    override func setUpWithError() throws { continueAfterFailure = false; XCUIDevice.shared.orientation = .portrait }
    override func tearDownWithError() throws {
        if let run = testRun, run.failureCount > 0 {
            let shot = XCTAttachment(screenshot: app.screenshot()); shot.lifetime = .keepAlways; add(shot)
            let tree = XCTAttachment(string: app.debugDescription); tree.lifetime = .keepAlways; add(tree)
        }
    }
    private func launch(_ extra: [String] = [], link: Bool = true, enabled: Bool = true) {
        app.terminate()
        app.launchArguments = ["-fixtures", "-appearance=light", "-device-approval-fixtures", enabled ? "-features=orgs" : "-features="]
            + (link ? ["-device-approval-link-fixture"] : ["-sheet=settings"]) + extra
        app.launch()
    }
    private func element(_ id: String) -> XCUIElement { app.descendants(matching: .any).matching(identifier: id).firstMatch }
    private func reveal(_ item: XCUIElement, towardsTop: Bool = false) {
        let form = app.collectionViews["device-approval-form"].firstMatch
        if !form.exists { XCTAssertTrue(item.waitForExistence(timeout: 10)); XCTAssertTrue(item.isHittable); return }
        var visible = false
        for _ in 0..<14 {
            let bounds = form.frame.intersection(app.frame)
            let top = max(bounds.minY, app.navigationBars.allElementsBoundByIndex.last?.frame.maxY ?? bounds.minY) + 8
            var bottom = min(bounds.maxY, app.frame.maxY - 34) - 12
            if app.keyboards.firstMatch.exists { bottom = min(bottom, app.keyboards.firstMatch.frame.minY - 8) }
            let rect = item.exists ? item.frame : .zero
            if item.exists && rect.width > 0 && rect.height > 0 && rect.minY >= top && rect.maxY <= bottom { visible = true; break }
            let point = app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: bounds.midX, dy: (top+bottom)/2))
            let direction: CGFloat = item.exists ? (rect.midY < top ? 1 : -1) : (towardsTop ? 1 : -1)
            point.press(forDuration: 0.05, thenDragTo: point.withOffset(CGVector(dx: 0, dy: min(120, (bottom-top)/4)*direction)))
        }
        XCTAssertTrue(visible, "Expected the machine control inside its form")
    }
    private func tap(_ item: XCUIElement, towardsTop: Bool = false) { reveal(item, towardsTop: towardsTop); item.tap() }
    private func confirmHold() {
        let hold = app.buttons["device-approval-hold"]
        reveal(hold); hold.press(forDuration: 1.7)
        let confirm = app.buttons["Approve machine"].firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5)); confirm.tap()
    }
    func testStrongSignInReturnsToReviewAndRequiresAnotherHold() {
        launch()
        reveal(element("device-approval-creates-agent"))
        reveal(element("device-approval-number"))
        let hold = app.buttons["device-approval-hold"]
        tap(hold)
        XCTAssertFalse(app.buttons["Approve machine"].exists)
        hold.press(forDuration: 0.4)
        XCTAssertFalse(app.buttons["Approve machine"].exists)
        confirmHold()
        tap(app.buttons["device-approval-verify"])
        reveal(hold)
        XCTAssertFalse(element("device-approval-approved").exists)
        XCTAssertFalse(app.buttons["Approve machine"].exists)
        confirmHold()
        XCTAssertTrue(element("device-approval-approved").waitForExistence(timeout: 10))
    }
    func testDenyShowsResolvedDenial() {
        launch(); tap(app.buttons["device-approval-deny"])
        XCTAssertTrue(element("device-approval-denied").waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["device-approval-hold"].exists)
    }
    func testDenyRaceReportsThatApprovalWon() {
        launch(["-device-approval-deny-race"]); tap(app.buttons["device-approval-deny"])
        XCTAssertTrue(element("device-approval-approved").waitForExistence(timeout: 10))
        XCTAssertFalse(element("device-approval-denied").exists)
    }
    func testExpiredRequestCannotBeApproved() {
        launch(["-device-approval-expired"])
        reveal(element("device-approval-message"))
        XCTAssertTrue(element("device-approval-message").label.contains("expired"))
        XCTAssertFalse(app.buttons["device-approval-hold"].exists)
    }
    func testMissingDelegationDisablesApproval() {
        launch(["-device-approval-forbidden"])
        reveal(element("device-approval-message"))
        XCTAssertTrue(element("device-approval-message").label.contains("operator"))
        let hold = app.buttons["device-approval-hold"]
        reveal(hold); XCTAssertFalse(hold.isEnabled)
    }
    func testSettingsClosesBeforeOpeningManualCodeEntry() {
        launch(link: false)
        tap(app.buttons["device-approval-open"])
        XCTAssertTrue(app.navigationBars["Sign in a machine"].waitForExistence(timeout: 10))
        let code = app.textFields["device-approval-code"]
        tap(code); code.typeText("ABCD-1234")
        tap(app.buttons["device-approval-review"])
        reveal(element("device-approval-number"))
    }
    func testChangingTheCodeRetiresTheConfirmation() {
        launch()
        reveal(element("device-approval-number"))
        let code = app.textFields["device-approval-code"]
        tap(code, towardsTop: true); code.typeText(XCUIKeyboardKey.delete.rawValue)
        XCTAssertFalse(app.buttons["device-approval-hold"].exists)
        XCTAssertFalse(element("device-approval-number").exists)
        XCTAssertFalse(app.buttons["device-approval-verify"].exists)
    }
    func testServerWithoutOrganizationsOffersNoMachineSignIn() {
        launch(link: false, enabled: false)
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["device-approval-open"].exists)
    }
}
