import XCTest

/// Drives the real calendar API/store/screens against the DEBUG-only .invalid transport. These
/// are interaction checks, not evidence that a fixture implements the postbox correctly.
final class CalendarUITests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
    private let review = "calendar-event:occ_evt_review_original"
    override func setUpWithError() throws {
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
    }
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
    private func revealRow(_ item: XCUIElement, in formID: String) {
        let collection = app.collectionViews[formID].firstMatch
        let form = collection.exists ? collection : element(formID)
        XCTAssertTrue(form.waitForExistence(timeout: 5), "Expected calendar form " + formID)
        var visible = false
        for _ in 0..<14 {
            let bounds = form.frame.intersection(app.frame)
            let top = max(bounds.minY, app.navigationBars.allElementsBoundByIndex.last { $0.isHittable }?.frame.maxY ?? bounds.minY) + 8
            var bottom = min(bounds.maxY, app.frame.maxY - 34) - 12
            if app.keyboards.firstMatch.exists { bottom = min(bottom, app.keyboards.firstMatch.frame.minY - 8) }
            let rect = item.exists ? item.frame : .zero
            if item.exists && item.isHittable && rect.minY >= top && rect.maxY <= bottom { visible = true; break }
            let start = app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: bounds.midX, dy: (top + bottom) / 2))
            let distance = min(120, (bottom - top) / 4)
            let direction: CGFloat = item.exists && rect.midY < top ? 1 : -1
            start.press(forDuration: 0.05, thenDragTo: start.withOffset(CGVector(dx: 0, dy: distance * direction)))
        }
        XCTAssertTrue(visible, "The calendar control must be inside " + formID)
    }
    private func tapRow(_ item: XCUIElement, in formID: String) { revealRow(item, in: formID); item.tap() }
    private func typeRow(_ text: String, into field: XCUIElement, in formID: String) { revealRow(field, in: formID); type(text, into: field) }

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
    private func revealConversations() {
        let button = app.buttons["showSidebar"].firstMatch
        if button.waitForExistence(timeout: 2), button.isHittable { button.tap() }
    }
    private func visibleReloadButton() -> XCUIElement? {
        // iPadOS26 exposes the action's wrapper and actual button with the same identifier.
        app.buttons.matching(identifier: "calendar-confirm-reload").allElementsBoundByIndex.first { $0.isHittable }
    }
    private func edit(_ choice: String) {
        tap(element("calendar-edit")); tapPopup(app.buttons[choice].firstMatch)
        XCTAssertTrue(element("calendar-title").waitForExistence(timeout: 5))
    }

    func testCalendarEntryRespectsCapabilities() {
        launch(calendar: false)
        revealConversations()
        XCTAssertTrue(app.buttons["Settings"].waitForExistence(timeout: 10))
        XCTAssertFalse(element("calendarButton").exists)
        launch(open: false)
        revealConversations()
        XCTAssertTrue(element("calendarButton").waitForExistence(timeout: 10))
        tap(element("calendarButton"))
        XCTAssertTrue(app.navigationBars["Calendar"].waitForExistence(timeout: 5))
    }
    func testReminderOpensAuthorizedEvent() {
        launch(["-calendar-notice=current"], open: false)
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Design review"].firstMatch.exists)
        XCTAssertFalse(app.textViews["messageComposer"].exists)
    }
    func testMeetingLinkShowsAnExplicitCallChoice() {
        launch(["-meeting-link-fixture"], open: false)
        XCTAssertTrue(app.navigationBars["Scheduled meeting"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["meeting-link-occurrence:occ_link_first"].waitForExistence(timeout: 5))
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: app.buttons["meeting-link-voice"])
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 5), .completed)
        XCTAssertTrue(app.buttons["meeting-link-video"].isEnabled)
        XCTAssertFalse(element("scheduled-call-preparing").exists, "opening a link must not start a call")
    }
    func testMeetingLinkOverlapRequiresOccurrenceSelection() {
        launch(["-meeting-link-fixture", "-meeting-link-overlap"], open: false)
        XCTAssertTrue(app.navigationBars["Scheduled meeting"].waitForExistence(timeout: 10))
        let second = app.buttons["meeting-link-occurrence:occ_link_second"]
        XCTAssertTrue(second.waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["meeting-link-voice"].isEnabled)
        XCTAssertFalse(app.buttons["meeting-link-video"].isEnabled)
        tap(second)
        XCTAssertTrue(app.buttons["meeting-link-voice"].isEnabled)
        XCTAssertTrue(app.buttons["meeting-link-video"].isEnabled)
        XCTAssertFalse(element("scheduled-call-preparing").exists)
    }
    func testUnavailableMeetingLinkKeepsCallButtonsDisabled() {
        launch(["-meeting-link-fixture", "-meeting-link-unavailable"], open: false)
        XCTAssertTrue(app.navigationBars["Scheduled meeting"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["This meeting is not open for this inbox."].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["meeting-link-voice"].isEnabled)
        XCTAssertFalse(app.buttons["meeting-link-video"].isEnabled)
    }
    func testMeetingLinkRespectsCalendarCapability() {
        launch(["-meeting-link-fixture"], calendar: false, open: false)
        XCTAssertTrue(app.staticTexts["Calendar is unavailable for this account."].firstMatch.waitForExistence(timeout: 10))
        XCTAssertFalse(app.navigationBars["Scheduled meeting"].exists)
    }
    func testScannedMeetingOpensAfterScannerDismisses() {
        launch(["-sheet=scanner", "-meeting-scan-fixture"], open: false)
        XCTAssertTrue(app.navigationBars["Scheduled meeting"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.buttons["meeting-link-occurrence:occ_link_first"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.navigationBars["Scan code"].exists)
        XCTAssertFalse(element("scheduled-call-preparing").exists)
    }
    func testSettingsScannerClosesBothSheetsBeforeOpeningMeeting() {
        launch(["-sheet=settings", "-meeting-scan-fixture"], open: false)
        let scan = app.buttons["scan-code"]
        XCTAssertTrue(scan.waitForExistence(timeout: 10))
        scan.tap()
        XCTAssertTrue(app.navigationBars["Scheduled meeting"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.buttons["meeting-link-occurrence:occ_link_first"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.navigationBars["Scan code"].exists)
    }
    func testReminderSelectsOnlyAnOwnedMailbox() {
        launch(["-calendar-notice=other"], open: false)
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Calendar for /k/zz1111v2h90vnwefj7g7ezvbh9"].firstMatch.exists)
        XCTAssertFalse(app.textViews["messageComposer"].exists)
    }
    func testLegacyReminderOpensCalendarWithoutInferringAnEvent() {
        launch(["-calendar-notice=legacy"], open: false)
        XCTAssertTrue(app.navigationBars["Calendar"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.navigationBars["Event"].exists)
    }
    func testNoticeFromAnotherAccountDoesNotOpenCalendar() {
        launch(["-calendar-notice=unowned"], open: false)
        XCTAssertTrue(app.staticTexts["This calendar notice belongs to another account."].firstMatch.waitForExistence(timeout: 10))
        XCTAssertFalse(app.navigationBars["Calendar"].exists)
        XCTAssertFalse(app.navigationBars["Event"].exists)
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
        let dismissed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: element("calendar-title"))
        XCTAssertEqual(XCTWaiter.wait(for: [dismissed], timeout: 5), .completed)
        let created = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@ AND label CONTAINS %@", "calendar-event:", "Candidate interview")).firstMatch
        tap(created)
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
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in self.visibleReloadButton() != nil }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 5), .completed)
        guard let confirm = visibleReloadButton() else { return XCTFail("The reload action must be hittable") }
        confirm.tap()
        let updated = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.app.textFields["calendar-title"].firstMatch.value as? String == "Updated on another device"
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [updated], timeout: 10), .completed)
        XCTAssertEqual(app.textFields["calendar-title"].firstMatch.value as? String, "Updated on another device")
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
        typeRow("abc", into: reminders, in: "calendar-event-details")
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
    func testParticipantsCanBeAddedAndRemovedExplicitly() {
        launch(["-calendar-people-fixture"]); openReview()
        tapRow(app.buttons["calendar-manage-participants"], in: "calendar-event-details")
        tapRow(app.buttons["calendar-add-person"], in: "calendar-people-form")
        typeRow("/fixture/new", into: app.textFields["calendar-new-attendee-address"].firstMatch, in: "calendar-people-form")
        tap(app.buttons["calendar-save-people"])
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["/fixture/new"].firstMatch.waitForExistence(timeout: 5))
        tapRow(app.buttons["calendar-manage-participants"], in: "calendar-event-details")
        tapRow(app.buttons["calendar-remove-attendee:att_existing"], in: "calendar-people-form")
        tap(app.buttons["calendar-save-people"])
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["/fixture/new"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Existing participant"].exists)
    }
    func testParticipantConflictKeepsChangesUntilExplicitReload() {
        launch(["-calendar-people-fixture", "-calendar-conflict-once"]); openReview()
        tapRow(app.buttons["calendar-manage-participants"], in: "calendar-event-details")
        tapRow(app.buttons["calendar-add-person"], in: "calendar-people-form")
        typeRow("/fixture/unsent", into: app.textFields["calendar-new-attendee-address"].firstMatch, in: "calendar-people-form")
        tap(app.buttons["calendar-save-people"])
        XCTAssertTrue(app.buttons["calendar-people-reload"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.textFields["calendar-new-attendee-address"].firstMatch.value as? String, "/fixture/unsent")
        XCTAssertFalse(app.buttons["calendar-save-people"].isEnabled)
        tapRow(app.buttons["calendar-people-reload"], in: "calendar-people-form")
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.app.buttons.matching(identifier: "calendar-confirm-people-reload").allElementsBoundByIndex.contains { $0.isHittable }
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 5), .completed)
        app.buttons.matching(identifier: "calendar-confirm-people-reload").allElementsBoundByIndex.first { $0.isHittable }?.tap()
        let cleared = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            !self.app.textFields["calendar-new-attendee-address"].firstMatch.exists && self.app.buttons["calendar-add-person"].isEnabled
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [cleared], timeout: 10), .completed)
        XCTAssertTrue(app.staticTexts["Existing participant"].exists)
    }
    func testRemovingFocusedInviteeKeepsTheNewEventUsable() {
        launch()
        tap(element("calendar-new-event"))
        tapRow(app.buttons["Add a person"], in: "calendar-event-editor")
        typeRow("/fixture/removed", into: app.textFields["calendar-create-attendee-address"].firstMatch, in: "calendar-event-editor")
        tapRow(app.buttons["Remove"].firstMatch, in: "calendar-event-editor")
        XCTAssertFalse(app.textFields["calendar-create-attendee-address"].firstMatch.exists)
        tapRow(app.buttons["Add a person"], in: "calendar-event-editor")
        XCTAssertTrue(app.textFields["calendar-create-attendee-address"].firstMatch.waitForExistence(timeout: 5))
        let value = app.textFields["calendar-create-attendee-address"].firstMatch.value as? String
        XCTAssertTrue(value == "" || value == "Pigeonpost handle or contact address")
    }
    func testPreviouslyLinkedGroupCanBeExplicitlyCleared() {
        launch(["-calendar-people-fixture"]); openReview()
        tapRow(app.buttons["calendar-manage-linked-group"], in: "calendar-event-details")
        tap(app.buttons["calendar-linked-group-picker"])
        tapPopup(app.buttons["No linked group"].firstMatch)
        tap(app.buttons["calendar-save-linked-group"])
        XCTAssertTrue(app.staticTexts["No group linked."].firstMatch.waitForExistence(timeout: 10))
    }
    func testInvitationPlacementDoesNotChangeItsResponse() {
        launch()
        tap(app.staticTexts["Team invitation"].firstMatch)
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 5))
        tapRow(app.buttons["calendar-move-invitation"], in: "calendar-event-details")
        tap(element("calendar-placement-picker"))
        tapPopup(app.buttons["Personal"].firstMatch)
        tap(app.buttons["calendar-save-placement"])
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["In Personal"].waitForExistence(timeout: 5))
        for _ in 0..<6 where !app.staticTexts["Needs a response"].firstMatch.isHittable { app.swipeDown() }
        XCTAssertTrue(app.staticTexts["Needs a response"].firstMatch.exists)
    }
    func testSubscriptionLinkIsShownOnlyOnCreationAndCanBeRevoked() {
        launch()
        tap(element("calendar-options")); tapPopup(app.buttons["Manage calendars"].firstMatch)
        tap(element("calendar-folder:cal_work")); tap(element("calendar-subscription"))
        XCTAssertTrue(app.navigationBars["Calendar subscription"].waitForExistence(timeout: 5))
        XCTAssertFalse(element("calendar-feed-issued-link").exists)
        let create = app.buttons["calendar-feed-create"]
        let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: create)
        XCTAssertEqual(XCTWaiter.wait(for: [enabled], timeout: 5), .completed)
        tapRow(create, in: "calendar-feed-form")
        XCTAssertTrue(element("calendar-feed-issued-link").waitForExistence(timeout: 5))
        tap(app.navigationBars["Calendar subscription"].buttons["Done"])
        tap(element("calendar-subscription"))
        XCTAssertTrue(app.buttons["calendar-feed-disable"].waitForExistence(timeout: 5))
        XCTAssertFalse(element("calendar-feed-issued-link").exists, "status reads cannot recover a subscription capability")
        tapRow(app.buttons["calendar-feed-disable"], in: "calendar-feed-form")
        let disable = app.buttons.matching(NSPredicate(format: "label == %@", "Disable subscription"))
        tapPopup(disable.allElementsBoundByIndex.first { $0.isHittable } ?? disable.firstMatch)
        XCTAssertTrue(app.staticTexts["Subscription off"].waitForExistence(timeout: 5))
    }
    func testImportConfirmationKeepsSelectionAfterLostResponse() {
        launch(["-calendar-import-preview-fixture", "-calendar-import-loss-once"])
        tap(element("calendar-options")); tapPopup(app.buttons["Import calendar file"].firstMatch)
        XCTAssertTrue(app.staticTexts["Imported planning"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.switches["calendar-import-entry:2"].firstMatch.isEnabled)
        tapRow(app.buttons["calendar-import-confirm"], in: "calendar-import-form")
        XCTAssertTrue(element("calendar-import-error").waitForExistence(timeout: 5))
        XCTAssertTrue(element("calendar-import-unconfirmed").exists)
        XCTAssertFalse(app.switches["calendar-import-entry:0"].firstMatch.isEnabled)
        XCTAssertFalse(app.switches["calendar-import-entry:1"].firstMatch.isEnabled)
        tapRow(app.buttons["calendar-import-confirm"], in: "calendar-import-form")
        XCTAssertTrue(app.staticTexts["Imported 2 events"].waitForExistence(timeout: 10))
        tap(app.navigationBars["Import calendar"].buttons["Done"])
        XCTAssertTrue(app.navigationBars["Calendar"].waitForExistence(timeout: 5))
    }
    func testLocalCalendarFileCanBeSelectedAndPreviewed() {
        launch(["-calendar-picker-fixture"])
        tap(element("calendar-options")); tapPopup(app.buttons["Import calendar file"].firstMatch)
        tap(app.buttons["calendar-import-choose-file"])
        let browse = app.buttons["Browse"].firstMatch
        if browse.waitForExistence(timeout: 3) { browse.tap() }
        let local = app.staticTexts.matching(NSPredicate(format: "label == %@ OR label == %@", "On My iPhone", "On My iPad")).firstMatch
        if local.waitForExistence(timeout: 3) { local.tap() }
        let folder = app.cells.matching(NSPredicate(format: "identifier BEGINSWITH %@", "Pigeonpost calendar tests,")).firstMatch
        XCTAssertTrue(folder.waitForExistence(timeout: 10))
        folder.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.25)).tap()
        let file = app.cells.matching(NSPredicate(format: "identifier BEGINSWITH %@", "Pigeonpost import fixture")).firstMatch
        XCTAssertTrue(file.waitForExistence(timeout: 5))
        file.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.25)).tap()
        let open = app.buttons["Open"].firstMatch
        if open.waitForExistence(timeout: 2), open.isHittable { open.tap() }
        XCTAssertTrue(app.staticTexts["Local picker interview"].firstMatch.waitForExistence(timeout: 10))
        XCTAssertFalse(element("calendar-import-error").exists)
    }
    func testEventExportChooserCanBeCancelled() {
        launch(); openReview()
        tapRow(app.buttons["calendar-export-event"], in: "calendar-event-details")
        let cancel = app.buttons["Cancel"].firstMatch
        // The phone picker can restore a nested folder, where Back replaces Cancel. Return to
        // locations; a tablet already keeps Cancel in its outer toolbar.
        for _ in 0..<4 where !cancel.waitForExistence(timeout: 1) {
            let candidates = app.navigationBars.buttons.matching(NSPredicate(format: "label IN %@", ["On My iPhone", "On My iPad", "Browse"]))
            guard let back = candidates.allElementsBoundByIndex.first(where: { $0.isHittable }) else { break }
            back.tap()
        }
        XCTAssertTrue(cancel.waitForExistence(timeout: 10))
        cancel.tap()
        XCTAssertTrue(app.navigationBars["Event"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["calendar-export-event"].isEnabled)
    }
    private func checkJoinDismissal(_ id: String) {
        launch(["-calendar-meeting-fixture"]); openReview()
        let button = app.buttons[id]
        // The navigation header can appear while event details and their controls are loading.
        XCTAssertTrue(button.waitForExistence(timeout: 5))
        for _ in 0..<6 where !button.isHittable { app.swipeUp() }
        XCTAssertGreaterThanOrEqual(button.frame.height, 44)
        button.tap()
        // The UI fixture deliberately does not attach any call page or use a real token. This
        // checks routing and sheet dismissal; CallHostTests exercises the actual WebKit bridge.
        XCTAssertTrue(app.staticTexts["Calling isn’t ready. Reopen your inbox and try again."].waitForExistence(timeout: 8))
        XCTAssertFalse(app.navigationBars["Event"].exists)
        XCTAssertFalse(app.navigationBars["Calendar"].exists)
        revealConversations()
        XCTAssertTrue(element("calendarButton").exists)
    }
}
