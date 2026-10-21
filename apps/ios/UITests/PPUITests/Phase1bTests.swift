import XCTest

/// Phase 1b on the phone, against the fixture mailbox: the Calls page, Filtered, blocking, and the
/// Privacy, Blocked and Profile pages. Every write is local in `-fixtures`.
final class Phase1bTests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")

    override func setUpWithError() throws { continueAfterFailure = false }

    override func tearDownWithError() throws {
        if let run = testRun, run.failureCount > 0 {
            let shot = XCTAttachment(screenshot: app.screenshot())
            shot.name = "failure"
            shot.lifetime = .keepAlways
            add(shot)
        }
    }

    private func launch(_ arguments: [String]) {
        app.terminate()
        app.launchArguments = ["-fixtures"] + arguments
        app.launch()
    }

    private func element(_ id: String) -> XCUIElement { app.descendants(matching: .any)[id] }

    /// Three trailing buttons and the mailbox picker share the bar without any of them folding into
    /// an overflow menu or sitting over another.
    func testTopBarButtonsAreAllReachable() throws {
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom == .pad, "the phone's bar")
        launch([])
        let calls = app.buttons["callsButton"]
        XCTAssertTrue(calls.waitForExistence(timeout: 10))
        let buttons = [app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Acting as")).firstMatch,
                       calls, app.buttons["New conversation"], app.buttons["Settings"]]
        for button in buttons { XCTAssertTrue(button.exists && button.isHittable, "\(button) is not reachable") }
        for (i, a) in buttons.enumerated() {
            for b in buttons[(i + 1)...] {
                let overlap = a.frame.intersection(b.frame)
                XCTAssertFalse(overlap.width > 2 && overlap.height > 2, "\(a.label) overlaps \(b.label)")
            }
        }
        XCTAssertFalse(app.buttons["More"].exists, "a system overflow menu appeared in the bar")
        calls.tap()
        XCTAssertTrue(element("call:c1").waitForExistence(timeout: 5))
    }

    func testCallsPageFiltersAndRowActions() {
        launch(["-sheet=calls"])
        XCTAssertTrue(element("call:c1").waitForExistence(timeout: 10))
        XCTAssertTrue(element("call:c2").exists)
        XCTAssertFalse(element("call:c6").exists, "filtered calls are not in All")

        let filter = app.segmentedControls["callsFilter"]
        filter.buttons["Missed"].tap()
        XCTAssertTrue(element("call:c4").waitForExistence(timeout: 5))
        XCTAssertFalse(element("call:c2").exists, "an answered call is not missed")
        filter.buttons["Filtered"].tap()
        XCTAssertTrue(element("call:c6").waitForExistence(timeout: 5))
        filter.buttons["All"].tap()
        XCTAssertTrue(element("call:c3").waitForExistence(timeout: 5))

        app.buttons["callMore:c3"].tap()
        app.buttons["Delete from history"].tap()
        let gone = NSPredicate(format: "exists == false")
        expectation(for: gone, evaluatedWith: element("call:c3"))
        waitForExpectations(timeout: 5)

        app.buttons["callMore:c4"].tap()
        app.buttons["Block"].tap()
        XCTAssertTrue(app.navigationBars.staticTexts["Block /k/eeee5555ffff…?"].waitForExistence(timeout: 5)
                      || app.buttons["blockConfirm"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.switches["blockReport"].value as? String, "0", "report is off by default from the log")
        app.buttons["blockCancel"].tap()

        // Calling back closes the page and opens the conversation.
        app.buttons["callBack:c1"].tap()
        XCTAssertFalse(element("callsPage").waitForExistence(timeout: 2) && element("call:c1").isHittable)
    }

    func testFilteredAcceptAndBlock() {
        launch(["-sheet=filtered"])
        let olga = element("filtered:/olga/main")
        XCTAssertTrue(olga.waitForExistence(timeout: 10))
        olga.tap()
        app.buttons["filteredAccept"].tap()
        XCTAssertTrue(app.staticTexts["Already decided"].waitForExistence(timeout: 5))
        app.navigationBars.buttons["Filtered"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()

        let spam = element("filtered:/k/spam0000spam0000spam0000sp")
        XCTAssertTrue(spam.waitForExistence(timeout: 5))
        XCTAssertFalse(element("filtered:/olga/main").exists, "an accepted sender leaves Filtered")
        spam.tap()
        app.buttons["filteredBlock"].tap()
        XCTAssertTrue(app.buttons["blockConfirm"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.switches["blockReport"].value as? String, "1", "report is on by default from Filtered")
        XCTAssertFalse(app.switches["blockWide"].exists, "no namespace to block for a /k/ address")
        app.buttons["blockConfirm"].tap()
        XCTAssertTrue(app.staticTexts["Already decided"].waitForExistence(timeout: 5))
    }

    func testBlockFromThreadMenuShowsBannerAndUnblocks() {
        launch(["-open=/bekir/docdex"])
        let composer = app.textViews["messageComposer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        app.buttons["threadMenu"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Block ")).firstMatch.tap()
        XCTAssertTrue(app.buttons["blockConfirm"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.switches["blockWide"].exists, "a handle in a namespace offers blocking the namespace")
        app.buttons["blockConfirm"].tap()
        XCTAssertTrue(element("blockedBanner").waitForExistence(timeout: 5))
        XCTAssertFalse(composer.exists, "no composer under the banner")
        app.buttons["unblockBanner"].tap()
        XCTAssertTrue(composer.waitForExistence(timeout: 5))
    }

    func testPrivacyPageSaves() {
        launch(["-sheet=privacy"])
        let calls = app.segmentedControls["policy-unknown_calls"]
        XCTAssertTrue(calls.waitForExistence(timeout: 10))
        for field in ["unnamed_messages", "unknown_messages", "unnamed_calls", "low_reputation"] {
            XCTAssertTrue(app.segmentedControls["policy-" + field].exists, field)
        }
        calls.buttons["Silence"].tap()
        XCTAssertTrue(calls.buttons["Silence"].isSelected)
        XCTAssertTrue(app.staticTexts["Saved."].waitForExistence(timeout: 3))
    }

    func testBlockedPageListsAddsAndUnblocks() {
        launch(["-sheet=blocked"])
        XCTAssertTrue(app.buttons["unblock:/spamco/*"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Everyone at /spamco"].exists)
        let field = app.textFields["blockAddressField"]
        field.tap()
        field.typeText("not an address")
        app.buttons["blockAddressButton"].tap()
        XCTAssertTrue(app.staticTexts["Enter an address such as /k/…, /spamco/agent or /spamco/*."].waitForExistence(timeout: 3))
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: 20))
        field.typeText("/evil/*")
        app.buttons["blockAddressButton"].tap()
        XCTAssertTrue(app.buttons["unblock:/evil/*"].waitForExistence(timeout: 5))
        app.buttons["unblock:/spamco/*"].tap()
        let gone = NSPredicate(format: "exists == false")
        expectation(for: gone, evaluatedWith: app.buttons["unblock:/spamco/*"])
        waitForExpectations(timeout: 5)
    }

    func testProfilePage() {
        launch(["-sheet=profile"])
        XCTAssertTrue(element("profilePicture").waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["profileChoose"].exists)
        let visibility = app.segmentedControls["profileVisibility"]
        XCTAssertTrue(visibility.buttons["People I know"].isSelected)
        visibility.buttons["Everyone"].tap()
        XCTAssertTrue(visibility.buttons["Everyone"].isSelected)
        app.buttons["profileRemove"].tap()
        XCTAssertTrue(app.staticTexts["Picture removed. People see your initials."].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["profileRemove"].exists)
    }
}
