import XCTest

final class IOSUXTests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
    private var history: XCUIElement { app.tables["conversation-history"] }

    override func setUpWithError() throws { continueAfterFailure = false }

    override func tearDownWithError() throws {
        if let run = testRun, run.failureCount > 0 { screenshot("failure") }
    }

    private func open(_ extra: [String] = []) {
        app.launchArguments = ["-fixtures", "-report-landing", "-open=/bekir/agent1"] + extra
        app.launch()
        XCTAssertTrue(history.waitForExistence(timeout: 10))
    }

    private func expectState(_ fragment: String) {
        expectation(for: NSPredicate(format: "value CONTAINS %@", fragment), evaluatedWith: history)
        waitForExpectations(timeout: 8)
    }

    private func screenshot(_ name: String) {
        let capture = XCTAttachment(screenshot: app.screenshot())
        capture.name = name
        capture.lifetime = .keepAlways
        add(capture)
    }

    private func assertBottom(_ id: String) {
        let cell = history.cells["message:" + id]
        XCTAssertTrue(cell.waitForExistence(timeout: 8))
        let composer = app.textFields.firstMatch
        XCTAssertTrue(composer.exists)
        XCTAssertLessThanOrEqual(cell.frame.maxY, composer.frame.minY + 16)
        XCTAssertGreaterThan(cell.frame.maxY, composer.frame.minY - 50)
        expectState("latest=true")
    }

    func testLongHistoryStartsAtNewestWithTenRows() {
        for count in [90, 400] {
            open(["-long", "-long=\(count)"])
            expectState("loaded=10;")
            assertBottom("long-\(count - 1)")
            screenshot("latest-of-\(count)")
        }
    }

    func testOlderPagesAndArrivalPreserveReadingPosition() {
        open(["-ios-history"])
        expectState("loaded=10;")
        assertBottom("history-44")
        history.swipeDown(velocity: .slow)
        expectState("latest=false")
        let value = history.value as? String ?? ""
        XCTAssertFalse(value.contains("loaded=10;"), "An upward history gesture must reveal an older page")
        let visible = history.cells.allElementsBoundByIndex.filter { $0.isHittable && $0.frame.midY > history.frame.minY + 50 }
        let anchor = try! XCTUnwrap(visible.first)
        let id = anchor.identifier
        let y = anchor.frame.minY
        app.buttons["history-arrival"].tap()
        expectState("latest=false")
        XCTAssertEqual(history.cells[id].frame.minY, y, accuracy: 3)
        XCTAssertFalse(history.cells["message:history-arrival"].exists)
        screenshot("reading-older-after-arrival")
        app.buttons["history-latest"].tap()
        expectState("loaded=10;latest=true;first=history-arrival")
        assertBottom("history-arrival")
    }

    func testKeyboardSendAndSubjectSwitchStayAtNewest() {
        open(["-ios-history"])
        let composer = app.textFields.firstMatch
        composer.tap()
        composer.typeText("Newest message from the composer")
        assertBottom("history-44")
        app.buttons["Send"].tap()
        expectState("latest=true")
        XCTAssertTrue(app.staticTexts["Newest message from the composer"].isHittable)
        XCTAssertEqual(app.textFields.firstMatch.value as? String, "Write a message")
        screenshot("sent-above-keyboard")
        app.buttons["the deploy"].tap()
        XCTAssertEqual(history.cells.count, 0)
        app.buttons["General"].tap()
        expectState("loaded=10;latest=true")
        XCTAssertTrue(app.staticTexts["Newest message from the composer"].isHittable)
    }

    func testLateRowGrowthKeepsBottomVisible() {
        open(["-ios-history", "-history-growth"])
        let end = app.staticTexts["Delayed content end"]
        XCTAssertTrue(end.waitForExistence(timeout: 8))
        // The fixture deliberately changes intrinsic height after first layout.
        Thread.sleep(forTimeInterval: 3)
        assertBottom("history-44")
        XCTAssertTrue(end.isHittable)
        screenshot("late-content-growth")
    }

    func testMessageCopyAndContextMenuRemainUsable() {
        open(["-ios-history"])
        let newest = history.cells["message:history-44"]
        newest.buttons["Copy message"].tap()
        XCTAssertTrue(newest.buttons["Copied"].waitForExistence(timeout: 3))
        newest.staticTexts["History message 44"].press(forDuration: 1)
        XCTAssertTrue(app.buttons["Delete"].waitForExistence(timeout: 5))
        app.buttons["Delete"].tap()
        XCTAssertTrue(app.buttons["Cancel"].waitForExistence(timeout: 5))
        app.buttons["Cancel"].tap()
        XCTAssertTrue(newest.exists)
    }

    func testMailboxOrderDoesNotFollowCurrentSelection() {
        app.launchArguments = ["-fixtures", "-ios-mailboxes", "-sheet=identities"]
        app.launch()
        let first = app.buttons["mailbox:/bekir/main"]
        XCTAssertTrue(first.waitForExistence(timeout: 10))
        app.swipeUp()
        let keys = ["/bekir/main", "/lidya/main", "/sofya/main", "/bekir/work", "/sofya/agent", "/k/ios-raw"]
        let buttons = keys.map { app.buttons["mailbox:" + $0] }
        for pair in zip(buttons, buttons.dropFirst()) { XCTAssertLessThan(pair.0.frame.minY, pair.1.frame.minY) }
        XCTAssertEqual(app.buttons["mailbox:/sofya/main"].value as? String, "Selected")
        screenshot("mailbox-order-with-sofya-selected")
        first.tap()
        app.buttons["Acting as /bekir. Change mailbox"].tap()
        XCTAssertEqual(app.buttons["mailbox:/bekir/main"].value as? String, "Selected")
        XCTAssertLessThan(app.buttons["mailbox:/bekir/main"].frame.minY, app.buttons["mailbox:/lidya/main"].frame.minY)
    }
}
