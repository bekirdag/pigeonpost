import XCTest

/// Types into the real composer with a real keyboard, then sends. The only question asked is
/// whether the field is empty afterwards — the rest of the send path is covered by the
/// thread-model suite, which cannot press keys.
final class ComposerTests: XCTestCase {

    private func actionApp(features: String? = nil) -> XCUIApplication {
        let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
        app.launchArguments = ["-fixtures", "-open=/bekir/agent1", "-appearance=light"]
        if let features { app.launchArguments.append("-features=" + features) }
        app.launch()
        XCTAssertTrue(app.textViews["messageComposer"].waitForExistence(timeout: 15))
        return app
    }

    private func revealAction(_ id: String, in app: XCUIApplication) -> XCUIElement {
        let button = app.buttons[id]
        let history = app.tables["conversation-history"]
        for _ in 0..<12 {
            if button.exists {
                let top = history.frame.minY + 8
                let bottom = min(history.frame.maxY, app.textViews["messageComposer"].frame.minY) - 8
                let frame = button.frame
                if button.isHittable && frame.minY >= top && frame.maxY <= bottom { return button }
                let distance = min(140, max(60, (bottom - top) / 3))
                let direction: CGFloat = frame.maxY > bottom ? -1 : 1
                let start = app.coordinate(withNormalizedOffset: .zero)
                    .withOffset(CGVector(dx: history.frame.midX, dy: (top + bottom) / 2))
                start.press(forDuration: 0.05, thenDragTo: start.withOffset(CGVector(dx: 0, dy: direction * distance)))
            } else { history.swipeDown() }
        }
        XCTAssertTrue(button.isHittable, "message action is not reachable: \(id)")
        return button
    }

    func testReplyPreservesDraftAndClearsAfterSend() throws {
        let app = actionApp()
        let field = app.textViews["messageComposer"]
        field.tap()
        field.typeText("Keep my draft")
        revealAction("message-reply:m_reply", in: app).tap()
        XCTAssertTrue(app.buttons["Cancel reply"].waitForExistence(timeout: 5))
        XCTAssertEqual(drafted(app), "Keep my draft")
        app.buttons["Cancel reply"].tap()
        XCTAssertEqual(drafted(app), "Keep my draft")
        revealAction("message-reply:m_reply", in: app).tap()
        app.buttons["Send"].tap()
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: app.buttons["Cancel reply"])
        waitForExpectations(timeout: 5)
        assertCleared(app, "reply")
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "reply-quote:local_")).firstMatch.waitForExistence(timeout: 5), "sent reply must show its quoted message")
    }

    func testEditPreservesDraftAndShowsVersionHistory() throws {
        let app = actionApp()
        let field = app.textViews["messageComposer"]
        field.tap()
        field.typeText("My unsent draft")
        revealAction("message-edit:m_out2", in: app).tap()
        XCTAssertTrue(app.buttons["Cancel edit"].waitForExistence(timeout: 5))
        XCTAssertEqual(drafted(app), "pin the flaky macOS test and push a fix")
        app.buttons["Cancel edit"].tap()
        XCTAssertEqual(drafted(app), "My unsent draft", "cancel must restore the ordinary draft")
        revealAction("message-edit:m_out2", in: app).tap()
        field.tap()
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: drafted(app).count) + "Revised task")
        app.buttons["Save edit"].tap()
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: app.buttons["Cancel edit"])
        waitForExpectations(timeout: 5)
        XCTAssertEqual(drafted(app), "My unsent draft", "saving an edit must preserve the ordinary draft")
        revealAction("message-info:m_out2", in: app).tap()
        XCTAssertTrue(app.staticTexts["Original"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Edit 1"].exists)
        XCTAssertTrue(app.staticTexts["Revised task"].exists)
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "native-message-version-history"
        shot.lifetime = .keepAlways
        add(shot)
        app.buttons["Done"].tap()
        XCTAssertEqual(drafted(app), "My unsent draft")
    }

    func testMessageActionsStayHiddenWithoutCapabilities() throws {
        let app = actionApp(features: "")
        XCTAssertFalse(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-edit:")).firstMatch.exists)
        XCTAssertFalse(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-reply:")).firstMatch.exists)
        XCTAssertFalse(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "message-info:")).firstMatch.exists)
        XCTAssertTrue(app.buttons["Copy message"].firstMatch.exists)
    }

    private func openThread() -> XCUIApplication {
        let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
        app.launchArguments = ["-fixtures"]
        app.launch()
        let cell = app.cells.element(boundBy: 1)
        XCTAssertTrue(cell.waitForExistence(timeout: 15), "inbox never drew")
        cell.tap()
        return app
    }

    private func drafted(_ app: XCUIApplication) -> String {
        (app.textViews["messageComposer"].value as? String) ?? ""
    }

    private func assertCleared(_ app: XCUIApplication, _ what: String) {
        let left = drafted(app)
        XCTAssertTrue(
            left.isEmpty || left == "Write a message",
            "\(what): composer still holds \(left.debugDescription) after send"
        )
    }

    /// The composer clears by replacing its text field, which costs the field its first
    /// responder, which the send hands straight back. If that hand-back ever stops working the
    /// keyboard drops between two messages and nothing else looks wrong — so it is asked here
    /// rather than noticed later.
    func testKeyboardStaysUpAcrossASend() throws {
        let app = openThread()
        let field = app.textViews["messageComposer"]
        XCTAssertTrue(field.waitForExistence(timeout: 15), "no composer")
        field.tap()
        field.typeText("still typing after this one")
        Thread.sleep(forTimeInterval: 1)
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5),
                      "no keyboard to begin with — is the simulator's software keyboard off?")
        app.buttons["Send"].tap()
        Thread.sleep(forTimeInterval: 2)
        assertCleared(app, "keyboard round trip")
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5),
                      "the keyboard left when the message did")
        // And the replacement field is the one being typed into, not a ghost of the old one.
        app.textViews["messageComposer"].typeText("second")
        Thread.sleep(forTimeInterval: 1)
        XCTAssertEqual(drafted(app), "second", "keys went somewhere else after the send")
    }

    /// Ordinary prose, long enough to grow the field past one line.
    func testDraftClearsAfterSending() throws {
        let app = openThread()
        let field = app.textViews["messageComposer"]
        XCTAssertTrue(field.waitForExistence(timeout: 15), "no composer")
        field.tap()
        field.typeText("this is a test of the composer clearing after a send is made and it runs long enough to wrap onto several lines")
        Thread.sleep(forTimeInterval: 1)
        XCTAssertTrue(drafted(app).contains("composer"), "typing did not land: \(drafted(app))")
        app.buttons["Send"].tap()
        Thread.sleep(forTimeInterval: 2)
        assertCleared(app, "plain text")
    }

    /// Stops on a misspelling, so the keyboard is holding an autocorrect candidate at the moment
    /// the send happens — the state a binding write is documented not to reach.
    func testDraftClearsWhileAutocorrectIsPending() throws {
        let app = openThread()
        let field = app.textViews["messageComposer"]
        XCTAssertTrue(field.waitForExistence(timeout: 15), "no composer")
        field.tap()
        field.typeText("teh quick brwon fox jumpd")
        Thread.sleep(forTimeInterval: 1)
        // The keyboard's correction bar counts as an interruption, which can invalidate an
        // element between the query and the tap. Re-query and try again rather than call that
        // a product failure.
        for attempt in 0..<3 {
            let send = app.buttons["Send"]
            guard send.waitForExistence(timeout: 5) else { continue }
            send.tap()
            break
            _ = attempt
        }
        Thread.sleep(forTimeInterval: 2)
        assertCleared(app, "autocorrect pending")
    }

    /// Send twice without leaving the thread. If the first send leaves anything behind, the
    /// second inherits it.
    func testDraftClearsOnConsecutiveSends() throws {
        let app = openThread()
        let field = app.textViews["messageComposer"]
        XCTAssertTrue(field.waitForExistence(timeout: 15), "no composer")
        field.tap()
        field.typeText("first message")
        app.buttons["Send"].tap()
        Thread.sleep(forTimeInterval: 2)
        assertCleared(app, "first of two")
        app.textViews["messageComposer"].tap()
        app.textViews["messageComposer"].typeText("second message")
        Thread.sleep(forTimeInterval: 1)
        app.buttons["Send"].tap()
        Thread.sleep(forTimeInterval: 2)
        assertCleared(app, "second of two")
    }

    /// A newline typed into a `.vertical` field, then a send. Multi-line drafts are the shape the
    /// report described.
    func testDraftClearsAfterMultilineEntry() throws {
        let app = openThread()
        let field = app.textViews["messageComposer"]
        XCTAssertTrue(field.waitForExistence(timeout: 15), "no composer")
        field.tap()
        field.typeText("line one\nline two\nline three\nline four")
        Thread.sleep(forTimeInterval: 1)
        app.buttons["Send"].tap()
        Thread.sleep(forTimeInterval: 2)
        assertCleared(app, "multi-line")
    }

    /// The remaining shapes a draft can be in when Send is pressed. One test rather than five,
    /// because the launch is the slow part and the assertion is identical.
    func testDraftClearsForEveryDraftShape() throws {
        let app = openThread()
        let field = app.textViews["messageComposer"]
        XCTAssertTrue(field.waitForExistence(timeout: 15), "no composer")

        func round(_ what: String, _ text: String, doubleTap: Bool = false) {
            let field = app.textViews["messageComposer"]
            field.tap()
            field.typeText(text)
            Thread.sleep(forTimeInterval: 0.5)
            for _ in 0..<3 {
                let send = app.buttons["Send"]
                guard send.waitForExistence(timeout: 5) else { continue }
                send.tap()
                if doubleTap { send.tap() }
                break
            }
            Thread.sleep(forTimeInterval: 1.5)
            assertCleared(app, what)
        }

        round("misspelling with no trailing space", "recieve")
        round("emoji", "shipped it")
        round("double tap on Send", "double tapped", doubleTap: true)
        round("very long single line", String(repeating: "word ", count: 60))
    }
}
