import XCTest

/// Drives the real calendar API/store/screens against the DEBUG-only .invalid transport. These
/// are interaction checks, not evidence that a fixture implements the postbox correctly.
final class CalendarUITests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
    private let review = "calendar-event:occ_evt_review_original"
    override func setUpWithError() throws { continueAfterFailure = false }
    override func tearDownWithError() throws {
        if let run = testRun, run.failureCount > 0 {
            let shot = XCTAttachment(screenshot: app.screenshot()); shot.lifetime = .keepAlways; add(shot)
            let tree = XCTAttachment(string: app.debugDescription); tree.lifetime = .keepAlways; add(tree)
        }
    }
    private func launch(_ extra: [String] = [], calendar: Bool = true, open: Bool = true) {
        app.terminate()
        app.launchArguments = ["-fixtures", "-appearance=light", calendar ? "-features=calendar" : "-features="]
            + (calendar ? ["-calendar-fixtures"] + (open ? ["-sheet=calendar"] : []) : []) + extra
        app.launch()
    }
    private func element(_ id: String) -> XCUIElement { app.descendants(matching: .any).matching(identifier: id).firstMatch }
    private func tap(_ item: XCUIElement, file: StaticString = #filePath, line: UInt = #line) {
        for _ in 0..<6 where !item.isHittable { app.swipeUp() }
        XCTAssertTrue(item.waitForExistence(timeout: 5), file: file, line: line)
        XCTAssertTrue(item.isHittable, file: file, line: line)
        item.tap()
    }
    private func type(_ text: String, into field: XCUIElement) {
        tap(field)
        for _ in 0..<3 where !app.keyboards.firstMatch.exists {
            let frame = field.frame
            app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: frame.midX, dy: frame.midY)).tap()
            if app.keyboards.firstMatch.waitForExistence(timeout: 2) { break }
        }
        XCTAssertTrue(app.keyboards.firstMatch.exists)
        field.typeText(text)
    }
    private func tapPopup(_ item: XCUIElement) {
        XCTAssertTrue(item.waitForExistence(timeout: 5))
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: item)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 5), .completed)
        // Swiping to find a popup item can dismiss or move its presentation on iPad.
        item.tap()
    }
    private func openReview() {
        XCTAssertTrue(app.navigationBars["Calendar"].waitForExistence(timeout: 10))
        tap(element(review))
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 5))
    }
    private func edit(_ choice: String) {
        tap(element("calendar-edit")); tapPopup(app.buttons[choice].firstMatch)
        XCTAssertTrue(element("calendar-title").waitForExistence(timeout: 5))
    }

    func testCalendarEntryRespectsCapabilities() {
        launch(calendar: false)
        XCTAssertTrue(app.buttons["Settings"].waitForExistence(timeout: 10))
        XCTAssertFalse(element("calendarButton").exists)
        launch(open: false)
        XCTAssertTrue(element("calendarButton").waitForExistence(timeout: 10))
        tap(element("calendarButton"))
        XCTAssertTrue(app.navigationBars["Calendar"].waitForExistence(timeout: 5))
    }
    func testMonthLaunchNavigationAndAgenda() {
        launch()
        let day = element("calendar-day:2026-10-25")
        XCTAssertTrue(day.waitForExistence(timeout: 10))
        XCTAssertGreaterThanOrEqual(day.frame.height, 44)
        XCTAssertTrue(day.isSelected)
        XCTAssertTrue(app.buttons["Next month"].exists)
        app.buttons["Next month"].tap()
        XCTAssertTrue(app.staticTexts["November 2026"].waitForExistence(timeout: 5))
        app.buttons["Today"].tap()
        XCTAssertTrue(element("calendar-day:2026-10-25").waitForExistence(timeout: 5))
        openReview()
        XCTAssertTrue(app.staticTexts["Design review"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(element("calendar-edit").exists)
    }
    func testAmbiguousCreateKeepsDraftAndCanRetry() {
        launch(["-calendar-create-loss-once"])
        tap(element("calendar-new-event"))
        let title = element("calendar-title")
        XCTAssertTrue(title.waitForExistence(timeout: 5))
        type("Candidate interview", into: title)
        tap(element("calendar-save"))
        XCTAssertTrue(element("calendar-editor-error").waitForExistence(timeout: 5))
        XCTAssertEqual(title.value as? String, "Candidate interview")
        XCTAssertTrue(app.buttons["calendar-save"].isEnabled)
        tap(element("calendar-save"))
        XCTAssertTrue(app.navigationBars["Calendar"].waitForExistence(timeout: 10))
        tap(app.staticTexts["Candidate interview"].firstMatch)
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 5))
    }
    func testOccurrenceEditCancelThenSave() {
        launch(); openReview(); edit("Only this occurrence")
        let title = element("calendar-title")
        type(" revised", into: title)
        tap(app.buttons["Cancel"].firstMatch)
        XCTAssertTrue(app.staticTexts["Design review"].firstMatch.waitForExistence(timeout: 5))
        edit("Only this occurrence")
        XCTAssertEqual(title.value as? String, "Design review")
        type(" revised", into: title)
        tap(element("calendar-save"))
        XCTAssertTrue(app.staticTexts["Design review revised"].firstMatch.waitForExistence(timeout: 10))
    }
    func testConflictKeepsDraftUntilExplicitReload() {
        launch(["-calendar-conflict-once"]); openReview(); edit("Whole series")
        type(" unsent", into: element("calendar-title"))
        tap(element("calendar-save"))
        XCTAssertTrue(element("calendar-review-conflict").waitForExistence(timeout: 5))
        XCTAssertEqual(element("calendar-title").value as? String, "Design review unsent")
        XCTAssertFalse(app.buttons["calendar-save"].isEnabled)
        tap(element("calendar-review-conflict"))
        let confirm = app.buttons["calendar-confirm-reload"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: confirm)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 5), .completed)
        confirm.tap()
        let updated = NSPredicate(format: "value == %@", "Updated on another device")
        expectation(for: updated, evaluatedWith: element("calendar-title"))
        waitForExpectations(timeout: 5)
        XCTAssertTrue(app.buttons["calendar-save"].isEnabled)
    }
    func testFilteredInvitationHasItsOwnViewAndCanBeAnswered() {
        launch()
        tap(element("calendar-invitations"))
        XCTAssertTrue(app.navigationBars["Invitations"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Filtered invitation"].exists)
        tap(app.buttons["Filtered"].firstMatch)
        tap(app.staticTexts["Filtered invitation"].firstMatch)
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 5))
        XCTAssertFalse(element("calendar-edit").exists)
        tap(app.buttons["Maybe"].firstMatch)
        XCTAssertTrue(app.staticTexts["Maybe"].firstMatch.waitForExistence(timeout: 10))
        tap(app.navigationBars["Event"].buttons["Done"].firstMatch)
        XCTAssertTrue(app.staticTexts["No filtered invitations."].waitForExistence(timeout: 10))
    }
    func testInvalidRemindersKeepExistingDraftVisible() {
        launch(); openReview()
        let reminders = element("calendar-reminders")
        type("abc", into: reminders)
        tap(app.buttons["Save reminders"].firstMatch)
        XCTAssertTrue(element("calendar-detail-error").waitForExistence(timeout: 5))
        XCTAssertEqual(reminders.value as? String, "10abc")
    }
    func testCalendarPrivacySaveKeepsTimezone() {
        launch()
        tap(element("calendar-options")); tapPopup(app.buttons["Calendar settings"].firstMatch)
        let previews = element("calendar-notification-previews")
        XCTAssertTrue(previews.waitForExistence(timeout: 5))
        // SwiftUI exposes a labeled switch row and its actual UISwitch. Tapping the row's
        // text center does not operate the native control on every supported runtime.
        let control = previews.switches.firstMatch
        tap(control.exists ? control : previews)
        XCTAssertEqual(previews.value as? String, "1", "operate the actual switch before testing its persistence")
        tap(app.buttons["Save"].firstMatch)
        XCTAssertTrue(app.navigationBars["Calendar"].waitForExistence(timeout: 10))
        tap(element("calendar-options")); tapPopup(app.buttons["Calendar settings"].firstMatch)
        XCTAssertEqual(element("calendar-notification-previews").value as? String, "1")
        XCTAssertTrue(app.textFields.matching(NSPredicate(format: "value == %@", "Europe/Istanbul")).firstMatch.exists)
    }
    func testVoiceJoinDismissesCalendarBeforeReportingUnavailableFixtureCalls() {
        checkJoinDismissal("calendar-join-voice")
    }
    func testVideoJoinDismissesCalendarBeforeReportingUnavailableFixtureCalls() {
        checkJoinDismissal("calendar-join-video")
    }
    private func checkJoinDismissal(_ id: String) {
        launch(["-calendar-meeting-fixture"]); openReview()
        let button = app.buttons[id]
        for _ in 0..<6 where !button.isHittable { app.swipeUp() }
        XCTAssertTrue(button.waitForExistence(timeout: 5))
        XCTAssertGreaterThanOrEqual(button.frame.height, 44)
        button.tap()
        // The UI fixture deliberately does not attach any call page or use a real token. This
        // checks routing and sheet dismissal; CallHostTests exercises the actual WebKit bridge.
        XCTAssertTrue(app.staticTexts["Calling isn’t ready. Reopen your inbox and try again."].waitForExistence(timeout: 8))
        XCTAssertFalse(app.navigationBars["Event"].exists)
        XCTAssertFalse(app.navigationBars["Calendar"].exists)
        XCTAssertTrue(element("calendarButton").exists)
    }
}
