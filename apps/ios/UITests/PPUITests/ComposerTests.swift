import XCTest

/// Types into the real composer with a real keyboard, then sends. The only question asked is
/// whether the field is empty afterwards — the rest of the send path is covered by the
/// thread-model suite, which cannot press keys.
final class ComposerTests: XCTestCase {

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
        (app.textFields.firstMatch.value as? String) ?? ""
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
        let field = app.textFields.firstMatch
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
        app.textFields.firstMatch.typeText("second")
        Thread.sleep(forTimeInterval: 1)
        XCTAssertEqual(drafted(app), "second", "keys went somewhere else after the send")
    }

    /// Ordinary prose, long enough to grow the field past one line.
    func testDraftClearsAfterSending() throws {
        let app = openThread()
        let field = app.textFields.firstMatch
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
        let field = app.textFields.firstMatch
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
        let field = app.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 15), "no composer")
        field.tap()
        field.typeText("first message")
        app.buttons["Send"].tap()
        Thread.sleep(forTimeInterval: 2)
        assertCleared(app, "first of two")
        app.textFields.firstMatch.tap()
        app.textFields.firstMatch.typeText("second message")
        Thread.sleep(forTimeInterval: 1)
        app.buttons["Send"].tap()
        Thread.sleep(forTimeInterval: 2)
        assertCleared(app, "second of two")
    }

    /// A newline typed into a `.vertical` field, then a send. Multi-line drafts are the shape the
    /// report described.
    func testDraftClearsAfterMultilineEntry() throws {
        let app = openThread()
        let field = app.textFields.firstMatch
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
        let field = app.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 15), "no composer")

        func round(_ what: String, _ text: String, doubleTap: Bool = false) {
            let field = app.textFields.firstMatch
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
