import XCTest

/// The paperclip, driven the way a thumb drives it.
///
/// How far this can go is set by where the photo picker runs. `PhotosPicker` presents Apple's own
/// picker out of process and hands bytes back across it, which is exactly why the app needs no
/// photo-library permission — and also why the picker's own grid is in no accessibility hierarchy
/// a UI test can reach. So this checks that both places a phone keeps things are offered, and that
/// choosing the library really does bring the system picker up. What happens inside the picker is
/// Apple's; choosing a photo from it and seeing it staged was verified by hand.
final class AttachmentTests: XCTestCase {

    /// A plain `tap()` asks the element to scroll itself into view first, and the composer lives in
    /// a `safeAreaInset` rather than in anything that scrolls, so that request fails and takes the
    /// tap down with it. Tapping the middle of where it already is asks for none of that.
    private func press(_ element: XCUIElement, _ what: String) {
        XCTAssertTrue(element.waitForExistence(timeout: 15), "no \(what)")
        element.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
    }

    private func openComposer() -> XCUIApplication {
        let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
        app.launchArguments = ["-fixtures", "-open=/bekir/agent1"]
        app.launch()
        XCTAssertTrue(app.textFields.firstMatch.waitForExistence(timeout: 20), "no composer")
        return app
    }

    /// Both places a phone keeps things are offered. The complaint was that only one of them was:
    /// everything a person photographs is in the library, and reaching it through Files meant
    /// Browse > Photos, on the chance somebody knew it was there at all.
    func testPaperclipOffersPhotosAndFiles() {
        let app = openComposer()
        press(app.buttons["Attach"], "paperclip")
        XCTAssertTrue(
            app.buttons["Photo Library"].waitForExistence(timeout: 10),
            "the paperclip does not offer the photo library"
        )
        XCTAssertTrue(app.buttons["Files"].exists, "the paperclip no longer offers Files")
    }

    /// Files still opens too.
    ///
    /// Worth its own run rather than assumed: the composer now carries a `photosPicker` and a
    /// `fileImporter` on the same view, and two presentation modifiers stacked on one view is
    /// exactly the shape that lost `ConversationsView` its Settings sheet — see the note on
    /// `ThreadView.sheet`. There the second one silently never presented.
    func testFilesStillOpens() {
        let app = openComposer()
        let field = app.textFields.firstMatch
        press(app.buttons["Attach"], "paperclip")
        press(app.buttons["Files"], "files option")
        expectation(for: NSPredicate(format: "isHittable == false"), evaluatedWith: field)
        waitForExpectations(timeout: 20) { error in
            XCTAssertNil(error, "the file browser never came up over the conversation")
        }
    }

    /// And choosing the library brings the picker up.
    ///
    /// Asked as "is the composer covered", because that is the whole of what this side of the
    /// process boundary can see: the picker is a sheet, our own views go behind it, and a composer
    /// that is still there but no longer reachable is a sheet having been presented over it. A
    /// `PhotosPicker` that failed to present would leave the field hittable, which is the failure
    /// this is here to catch.
    func testChoosingTheLibraryBringsUpThePicker() {
        let app = openComposer()
        let field = app.textFields.firstMatch
        XCTAssertTrue(field.isHittable, "the composer was covered before anything was tapped")
        press(app.buttons["Attach"], "paperclip")
        press(app.buttons["Photo Library"], "photo library option")

        expectation(for: NSPredicate(format: "isHittable == false"), evaluatedWith: field)
        waitForExpectations(timeout: 20) { error in
            XCTAssertNil(error, "the photo picker never came up over the conversation")
        }
    }
}
