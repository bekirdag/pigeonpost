import XCTest

/// The widgets' views, drawn by the app's debug gallery (`-widget-gallery`) from the fixture
/// snapshot: every face renders, message text stays hidden unless previews are on, and a widget
/// link opens the screen it names. The screenshots attached here are the ones in the pull request.
final class WidgetGalleryTests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
    private var pad: Bool { UIDevice.current.userInterfaceIdiom == .pad }

    override func setUpWithError() throws { continueAfterFailure = false }

    private func launch(_ arguments: [String]) {
        app.terminate()
        app.launchArguments = ["-fixtures", "-widget-gallery"] + arguments
        app.launch()
        XCTAssertTrue(element("widget-gallery").waitForExistence(timeout: 15), "the gallery did not open")
    }

    private func element(_ id: String) -> XCUIElement { app.descendants(matching: .any)[id] }
    private func text(containing words: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", words)).firstMatch
    }

    private func keep(_ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "\(pad ? "ipad" : "iphone")-\(name)"
        shot.lifetime = .keepAlways
        add(shot)
    }

    func testInboxFacesRenderWithoutMessageText() {
        for dark in [false, true] {
            launch(dark ? ["-widget-dark"] : [])
            for face in pad ? ["small", "medium", "large", "extraLarge"] : ["small", "medium", "large"] {
                XCTAssertTrue(element("widget-inbox-\(face)").exists, "inbox \(face) is missing")
            }
            XCTAssertTrue(text(containing: "Ada Yılmaz").exists, "the latest conversation's name is shown")
            XCTAssertTrue(text(containing: "New message").exists, "unread rows say New message")
            XCTAssertFalse(text(containing: "Signed the offer letter").exists, "message text is shown with previews off")
            keep("inbox-\(dark ? "dark" : "light")")
        }
    }

    func testPreviewsAppearOnlyWhenSwitchedOn() {
        launch(["-widget-previews"])
        XCTAssertTrue(text(containing: "Signed the offer letter").exists, "previews on shows the latest message")
        keep("inbox-previews")
    }

    func testOtherWidgetsRender() {
        for dark in [false, true] {
            launch(["-widget-page=2"] + (dark ? ["-widget-dark"] : []))
            XCTAssertTrue(element("widget-quick-actions").exists)
            XCTAssertTrue(element("widget-nests-small").exists)
            XCTAssertTrue(element("widget-nests-medium").exists)
            XCTAssertTrue(element("widget-agenda-medium").exists)
            XCTAssertTrue(text(containing: "Research desk").exists, "a Nest's name")
            XCTAssertTrue(text(containing: "Design review").exists, "today's event")
            if !pad { XCTAssertTrue(text(containing: "Get a Nest").exists, "the empty Nests state links to buying one") }
            keep("more-\(dark ? "dark" : "light")")
            if !pad {
                launch(["-widget-page=3"] + (dark ? ["-widget-dark"] : []))
                XCTAssertTrue(element("widget-agenda-large").exists)
                XCTAssertTrue(element("widget-unread-circular").exists)
                XCTAssertTrue(element("widget-latest-rectangular").exists)
                keep("agenda-lock-\(dark ? "dark" : "light")")
            }
        }
    }

    /// A widget's New message action opens the composer in the real app (not the gallery).
    func testComposeLinkOpensNewConversation() throws {
        // The route is the same code on both; the phone's bar gives a stable "inbox is up" marker.
        try XCTSkipIf(pad, "covered on the phone; the iPad's columns have no single ready marker")
        app.terminate()
        app.launchArguments = ["-fixtures"]
        app.launch()
        XCTAssertTrue(app.buttons["Settings"].waitForExistence(timeout: 15))
        app.open(URL(string: "dev.pigeonpost.inbox://widget/compose")!)
        XCTAssertTrue(element("newConversationPeer").waitForExistence(timeout: 10), "the composer did not open")
    }
}
