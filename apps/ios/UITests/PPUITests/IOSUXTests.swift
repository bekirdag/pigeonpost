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
        let composer = app.textViews["messageComposer"]
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
        let composer = app.textViews["messageComposer"]
        composer.tap()
        composer.typeText("Newest message from the composer")
        assertBottom("history-44")
        app.buttons["Send"].tap()
        expectState("latest=true")
        XCTAssertTrue(app.staticTexts["Newest message from the composer"].isHittable)
        XCTAssertEqual(app.textViews["messageComposer"].value as? String, "")
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
        let confirmation = app.staticTexts["Delete this message?"]
        XCTAssertTrue(confirmation.waitForExistence(timeout: 5))
        if app.buttons["Cancel"].exists {
            app.buttons["Cancel"].tap()
        } else {
            // Native popovers omit Cancel; a tap outside is their cancellation action.
            // iOS 26.5 uses this presentation on the iPhone as well as regular-size layouts.
            XCTAssertTrue(app.popovers.firstMatch.exists)
            history.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.05)).tap()
        }
        XCTAssertFalse(confirmation.exists)
        XCTAssertTrue(newest.exists)
    }

    /// A long press on a message opens its menu once, over the message drawn upright, and leaves
    /// the message where it was. The history is a table turned upside down with each row turned
    /// back, and the system's own snapshot of a row came out mirrored or collapsed to a hairline and
    /// jumped as it lifted (screen recording, 2026-10-20). Received markdown with a table, a sent
    /// request, and a held request card; then Copy from the menu copies the whole message.
    func testLongPressOpensMessageMenuCleanly() throws {
        app.launchArguments = ["-fixtures", "-open=/bekir/agent1"]
        app.launch()
        XCTAssertTrue(history.waitForExistence(timeout: 10))
        let targets: [(id: String, mine: Bool)] = [("m_reply", false), ("m_out2", true), ("m2", false)]
        for target in targets {
            let cell = history.cells["message:" + target.id]
            XCTAssertTrue(cell.waitForExistence(timeout: 8), target.id)
            for _ in 0..<4 where !cell.isHittable || cell.frame.minY < history.frame.minY { history.swipeDown(velocity: .slow) }
            let words = cell.staticTexts.firstMatch
            XCTAssertTrue(words.exists, target.id)
            let before = cell.frame
            words.press(forDuration: 1.0)
            XCTAssertTrue(app.buttons["Copy"].waitForExistence(timeout: 4), "no menu for \(target.id)")
            XCTAssertEqual(app.buttons.matching(identifier: "Copy").count, 1, "one menu, not two")
            XCTAssertEqual(app.buttons["Report spam"].exists, !target.mine)
            XCTAssertTrue(app.buttons["Delete"].exists)
            XCTAssertEqual(app.menuItems.count, 0, "the long press opened the message menu, not a text selection")
            screenshot("long-press-" + target.id)
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.05, dy: 0.12)).tap()
            expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: app.buttons["Copy"])
            waitForExpectations(timeout: 4)
            XCTAssertEqual(cell.frame.minY, before.minY, accuracy: 1, "\(target.id) moved under the menu")
            XCTAssertEqual(cell.frame.height, before.height, accuracy: 1, "\(target.id) changed size under the menu")
        }
        // Copy, from the menu, copies the whole message.
        let reply = history.cells["message:m_reply"]
        for _ in 0..<4 where !reply.isHittable { history.swipeUp(velocity: .slow) }
        reply.staticTexts.firstMatch.press(forDuration: 1.0)
        XCTAssertTrue(app.buttons["Copy"].waitForExistence(timeout: 4))
        app.buttons["Copy"].tap()
        let composer = app.textViews["messageComposer"]
        composer.tap()
        composer.tap()
        let paste = app.menuItems["Paste"]
        XCTAssertTrue(paste.waitForExistence(timeout: 4))
        paste.tap()
        if app.alerts.buttons["Allow Paste"].waitForExistence(timeout: 1) { app.alerts.buttons["Allow Paste"].tap() }
        XCTAssertTrue((composer.value as? String ?? "").contains("Pinned it"), "Copy took the whole message")
    }

    /// Part of a message, not all of it: a double tap selects one word, the system's edit menu
    /// offers Copy, and what lands on the pasteboard is that word alone — checked by pasting it into
    /// the composer, since the test runner itself is not allowed to read the pasteboard.
    func testMessageTextSelectsAndCopiesPartially() {
        open(["-ios-history"])
        let newest = history.cells["message:history-44"]
        XCTAssertTrue(newest.waitForExistence(timeout: 8))
        let text = newest.staticTexts["History message 44"]
        XCTAssertTrue(text.waitForExistence(timeout: 5), newest.debugDescription)
        // The first word of "History message 44".
        text.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0.5)).withOffset(CGVector(dx: 14, dy: 0)).doubleTap()
        let copy = app.menuItems["Copy"]
        XCTAssertTrue(copy.waitForExistence(timeout: 4), app.debugDescription)
        screenshot("partial-selection")
        copy.tap()

        let composer = app.textViews["messageComposer"]
        composer.tap()
        composer.tap()
        let paste = app.menuItems["Paste"]
        XCTAssertTrue(paste.waitForExistence(timeout: 4), app.debugDescription)
        paste.tap()
        XCTAssertEqual(composer.value as? String, "History", "Only the selected word was copied")
        screenshot("partial-selection-pasted")
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

    /// iPad: the conversations column can be hidden, and brought back from the thread's bar.
    func testIPadSidebarCanBeHiddenAndShownAgain() throws {
        try XCTSkipUnless(UIDevice.current.userInterfaceIdiom == .pad, "iPad only")
        XCUIDevice.shared.orientation = .landscapeLeft
        app.launchArguments = ["-fixtures", "-open=/bekir/agent1"]
        app.launch()
        let hide = app.buttons["hideSidebar"], show = app.buttons["showSidebar"]
        XCTAssertTrue(hide.waitForExistence(timeout: 10))
        hide.tap()
        XCTAssertTrue(show.waitForExistence(timeout: 5))
        XCTAssertTrue(show.isHittable)
        screenshot("sidebar-hidden")
        show.tap()
        XCTAssertTrue(hide.waitForExistence(timeout: 5))
        XCTAssertTrue(hide.isHittable)
        screenshot("sidebar-shown")
    }

    /// A call can be minimised to a strip over the inbox and brought back. Needs the local call
    /// harness (stand-in postbox, call service, page server on :8460); skipped otherwise.
    func testCallMinimisesToAStripAndComesBack() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["PP_CALL_HARNESS"] == "1", "needs the call harness")
        app.launchArguments = ["-fixtures", "-calls-page", "http://127.0.0.1:8460/native-call.html",
                               "-calls-token", "tok-alice", "-calls-mailbox", "/k/alice0000000000000000000a",
                               "-calls-handle", "/alice", "-call", "/bob:audio"]
        app.launch()
        let back = app.webViews.buttons["Back to the inbox (the call keeps going)"]
        XCTAssertTrue(back.waitForExistence(timeout: 20))
        screenshot("call-full")
        back.tap()
        let open = app.webViews.buttons["Return to the call"].firstMatch
        XCTAssertTrue(open.waitForExistence(timeout: 5))
        XCTAssertLessThan(open.frame.maxY, 140, "the strip sits at the top")
        screenshot("call-mini")
        open.tap()
        XCTAssertTrue(back.waitForExistence(timeout: 5))
        app.webViews.buttons["Leave call"].firstMatch.tap()
    }

    /// App Store screenshots from the fixture mailbox: `TEST_RUNNER_PP_STORE_SHOTS=<directory>`.
    /// iPad in landscape, iPhone in portrait. Skipped otherwise.
    func testStoreScreenshots() throws {
        let path = ProcessInfo.processInfo.environment["PP_STORE_SHOTS"] ?? ""
        try XCTSkipIf(path.isEmpty, "set PP_STORE_SHOTS to take store screenshots")
        let directory = URL(fileURLWithPath: path)
        let pad = UIDevice.current.userInterfaceIdiom == .pad
        XCUIDevice.shared.orientation = pad ? .landscapeLeft : .portrait
        let scenes: [(String, [String])] = pad
            ? [("1-conversation", ["-open=/bekir/agent1"]),
               ("2-first-contact", ["-open=/k/eeee5555ffff6666gggg7777hh"]),
               ("3-settings", ["-open=/bekir/agent1", "-sheet=settings"])]
            : [("1-list", []), ("2-thread", ["-open=/bekir/agent1"])]
        for (name, extra) in scenes {
            app.terminate()
            app.launchArguments = ["-fixtures", "-appearance=dark"] + extra
            app.launch()
            sleep(4)
            let shot = XCUIScreen.main.screenshot()
            try shot.pngRepresentation.write(to: directory.appendingPathComponent("store-\(name).png"))
        }
    }
}

// MARK: - iPad layout audit (SDS §19.2)

/// Every iPad screen, checked for controls drawn over one another and bar titles cut short, across
/// orientation, appearance and text size. CI runs portrait and landscape in the light appearance at
/// the default text size; `PP_LAYOUT_MATRIX=full` adds dark and extra-large text, and
/// `PP_LAYOUT_SHOTS=<directory>` keeps a screenshot of every state for review
/// (`TEST_RUNNER_PP_LAYOUT_SHOTS` when run through `xcodebuild test`).
///
/// What counts as an overlap is deliberately narrow: two controls a person must be able to reach
/// independently (buttons, fields, switches, links) whose frames intersect, where neither contains
/// the other. A row scrolled under a bar is not one — the bar covers it by design — and neither is
/// a control and the container it sits in.
final class IPadLayoutTests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")

    override func setUpWithError() throws {
        continueAfterFailure = true
        try XCTSkipUnless(UIDevice.current.userInterfaceIdiom == .pad, "iPad only")
    }

    override func tearDown() {
        XCUIDevice.shared.orientation = .portrait
        super.tearDown()
    }

    private struct Scene {
        let name: String
        let arguments: [String]
        var ready: String = "messageComposer"
        var prepare: (XCUIApplication) -> Void = { _ in }
    }

    private var scenes: [Scene] {
        let open = ["-fixtures", "-open=/bekir/agent1"]
        return [
            Scene(name: "sign-in", arguments: [], ready: "signInWithApple"),
            Scene(name: "inbox", arguments: open),
            Scene(name: "inbox-sidebar-hidden", arguments: open) { app in
                let hide = app.buttons["hideSidebar"]
                if hide.waitForExistence(timeout: 5) { hide.tap() }
                _ = app.buttons["showSidebar"].waitForExistence(timeout: 5)
            },
            Scene(name: "inbox-nothing-open", arguments: ["-fixtures"], ready: "Pick a conversation"),
            Scene(name: "long-thread", arguments: open + ["-long"]),
            Scene(name: "composer-attachments", arguments: open) { app in Self.stageAttachments(app) },
            Scene(name: "settings", arguments: ["-fixtures", "-sheet=settings", "-handle=owned"], ready: "settings-handles"),
            Scene(name: "purchase", arguments: ["-fixtures", "-sheet=settings", "-handle=sale"], ready: "settings-purchases") { app in
                let page = app.buttons["settings-purchases"]
                let enabled = NSPredicate(format: "enabled == true")
                for _ in 0..<20 where !enabled.evaluate(with: page) { Thread.sleep(forTimeInterval: 0.25) }
                page.tap()
                _ = app.textFields["yourname"].waitForExistence(timeout: 8)
            },
            Scene(name: "new-conversation", arguments: ["-fixtures", "-sheet=new"], ready: "New conversation"),
            Scene(name: "new-thread", arguments: open + ["-sheet=thread"], ready: "New thread"),
            Scene(name: "mailboxes", arguments: ["-fixtures", "-sheet=identities"], ready: "Mailboxes"),
            Scene(name: "sender", arguments: open + ["-sheet=peer"], ready: "Known sender"),
        ]
    }

    func testIPadScreensHaveNoOverlapsOrCutTitles() throws {
        let full = ProcessInfo.processInfo.environment["PP_LAYOUT_MATRIX"] == "full"
        let only = ProcessInfo.processInfo.environment["PP_LAYOUT_SCENES"].flatMap { $0.isEmpty ? nil : Set($0.split(separator: ",").map(String.init)) }
        let orientations: [(String, UIDeviceOrientation)] = [("portrait", .portrait), ("landscape", .landscapeLeft)]
        let looks = full ? ["light", "dark"] : ["light"]
        let sizes: [(String, [String])] = full
            ? [("default", []), ("xl", ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryXL"])]
            : [("default", [])]
        var problems: [String] = []
        for (orientationName, orientation) in orientations {
            XCUIDevice.shared.orientation = orientation
            for look in looks {
                for (sizeName, sizeArguments) in sizes {
                    for scene in scenes where only?.contains(scene.name) ?? true {
                        let state = "\(orientationName)-\(look)-\(sizeName)-\(scene.name)"
                        app.terminate()
                        // `-fixtures` first and the `-key value` text size last: UserDefaults reads a bare
                        // `-key value` pair and would otherwise swallow the next flag (Fixtures.openPeer).
                        app.launchArguments = scene.arguments + (scene.arguments.isEmpty ? [] : ["-appearance=\(look)"]) + sizeArguments
                        app.launch()
                        if !scene.ready.isEmpty {
                            let ready = app.descendants(matching: .any)[scene.ready]
                            if !ready.waitForExistence(timeout: 15) { problems.append("\(state): never showed \(scene.ready)"); continue }
                        } else {
                            Thread.sleep(forTimeInterval: 2)
                        }
                        scene.prepare(app)
                        Thread.sleep(forTimeInterval: 1.2)
                        keep(state)
                        problems += findProblems().map { "\(state): \($0)" }
                    }
                }
            }
        }
        XCTAssertTrue(problems.isEmpty, "\(problems.count) layout problem(s):\n" + problems.joined(separator: "\n"))
    }

    /// The call over the iPad: calling out, the call itself and its minimised strip in both
    /// orientations, then an incoming ring. Needs the local call harness (stand-in postbox, call
    /// service and page server on :8460, with a browser peer `/bob` that answers after a few
    /// seconds and rings back once the call ends); skipped otherwise.
    func testIPadCallOverlayHasNoOverlaps() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["PP_CALL_HARNESS"] == "1", "needs the call harness")
        let harness = ["-fixtures", "-calls-page", "http://127.0.0.1:8460/native-call.html",
                       "-calls-token", "tok-alice", "-calls-mailbox", "/k/alice0000000000000000000a",
                       "-calls-handle", "/alice"]
        var problems: [String] = []
        func check(_ state: String) {
            Thread.sleep(forTimeInterval: 1.2)
            keep("call-" + state)
            // The call's own controls. While it fills the screen the inbox is still in the
            // accessibility tree under it, but nothing of it can be seen or touched.
            problems += findProblems(in: state.hasPrefix("mini") ? nil : app.webViews.firstMatch).map { "call-\(state): \($0)" }
        }
        XCUIDevice.shared.orientation = .landscapeLeft
        app.terminate()
        app.launchArguments = harness + ["-call", "/bob:video"]
        app.launch()
        let leave = app.webViews.buttons["Leave call"].firstMatch
        XCTAssertTrue(leave.waitForExistence(timeout: 40))
        check("calling-landscape")
        // Connected: the name pill's clock is running.
        let clock = app.webViews.buttons.matching(NSPredicate(format: "label MATCHES %@", ".* [0-9]+:[0-9]{2}$")).firstMatch
        XCTAssertTrue(clock.waitForExistence(timeout: 40), "the peer never answered")
        check("full-landscape")
        XCUIDevice.shared.orientation = .portrait
        check("full-portrait")
        app.webViews.buttons["Back to the inbox (the call keeps going)"].firstMatch.tap()
        let back = app.webViews.buttons["Return to the call"].firstMatch
        XCTAssertTrue(back.waitForExistence(timeout: 5))
        check("mini-portrait")
        XCUIDevice.shared.orientation = .landscapeLeft
        check("mini-landscape")
        back.tap()
        XCTAssertTrue(leave.waitForExistence(timeout: 5))
        leave.tap()
        // Bob rings back once the call has ended.
        app.terminate()
        app.launchArguments = harness
        app.launch()
        let accept = app.webViews.buttons["Accept"].firstMatch
        XCTAssertTrue(accept.waitForExistence(timeout: 60), "no incoming ring")
        check("ringing-landscape")
        XCUIDevice.shared.orientation = .portrait
        check("ringing-portrait")
        app.webViews.buttons["Decline"].firstMatch.tap()
        XCTAssertTrue(problems.isEmpty, "\(problems.count) layout problem(s):\n" + problems.joined(separator: "\n"))
    }

    // MARK: Checks

    private static let controls: Set<XCUIElement.ElementType> = [
        .button, .textField, .secureTextField, .textView, .searchField, .switch, .toggle, .link,
        .segmentedControl, .slider, .stepper, .menuButton, .popUpButton,
    ]
    private static let bars: Set<XCUIElement.ElementType> = [.navigationBar, .toolbar, .tabBar]
    private static let scrollers: Set<XCUIElement.ElementType> = [.table, .collectionView, .scrollView]

    private struct Node {
        let label: String
        let type: XCUIElement.ElementType
        let frame: CGRect
        let path: [Int]
        let inBar: Bool
        let inScroller: Bool
        func contains(_ other: Node) -> Bool { other.path.starts(with: path) }
    }

    private func findProblems(in scope: XCUIElement? = nil) -> [String] {
        guard let root = try? (scope ?? app).snapshot(), let screen = try? app.snapshot().frame else { return ["no accessibility snapshot"] }
        var nodes: [Node] = []
        var titles: [(String, CGRect)] = []
        func hasKeyboard(_ element: XCUIElementSnapshot) -> Bool {
            element.elementType == .keyboard || element.children.contains(where: hasKeyboard)
        }
        func walk(_ element: XCUIElementSnapshot, _ path: [Int], inBar: Bool, inScroller: Bool) {
            // The system keyboard and its bar are Apple's, in a window of their own.
            if element.elementType == .keyboard || (element.elementType == .window && hasKeyboard(element)) { return }
            let bar = inBar || Self.bars.contains(element.elementType)
            let scroller = inScroller || Self.scrollers.contains(element.elementType)
            let frame = element.frame.intersection(screen)
            if Self.controls.contains(element.elementType), frame.width >= 4, frame.height >= 4 {
                let name = element.identifier.isEmpty ? element.label : element.identifier
                nodes.append(Node(label: "\(element.elementType.name) '\(name.prefix(40))'", type: element.elementType,
                                  frame: frame, path: path, inBar: bar, inScroller: scroller))
            }
            if element.elementType == .navigationBar {
                for child in element.children where child.elementType == .staticText && !child.label.isEmpty {
                    titles.append((child.label, child.frame))
                }
            }
            for (index, child) in element.children.enumerated() {
                walk(child, path + [index], inBar: bar, inScroller: scroller)
            }
        }
        walk(root, [], inBar: false, inScroller: false)

        var problems: [String] = []
        for i in nodes.indices {
            for j in nodes.indices where j > i {
                let a = nodes[i], b = nodes[j]
                if a.contains(b) || b.contains(a) { continue }
                // Scrolled content passing under a bar is covered by the bar, not overlapping it.
                if (a.inBar && b.inScroller && !b.inBar) || (b.inBar && a.inScroller && !a.inBar) { continue }
                let overlap = a.frame.intersection(b.frame)
                if overlap.width > 2, overlap.height > 2 {
                    problems.append("\(a.label) \(Self.rect(a.frame)) overlaps \(b.label) \(Self.rect(b.frame))")
                }
            }
        }
        // A bar title cut short: wider set than the room it was given, or drawn under a bar button.
        let barButtons = nodes.filter { $0.inBar }
        for (title, frame) in titles {
            let needed = (title as NSString).size(withAttributes: [.font: UIFont.systemFont(ofSize: 17, weight: .semibold)]).width
            if needed > frame.width * 1.15 + 6 { problems.append("bar title '\(title)' cut short (\(Int(frame.width)) of ~\(Int(needed)) pt)") }
            for button in barButtons where button.frame.intersection(frame).width > 2 && button.frame.intersection(frame).height > 2 {
                problems.append("bar title '\(title)' under \(button.label)")
            }
        }
        return problems
    }

    private static func rect(_ r: CGRect) -> String { "[\(Int(r.minX)),\(Int(r.minY)) \(Int(r.width))x\(Int(r.height))]" }

    // MARK: Helpers

    private static func stageAttachments(_ app: XCUIApplication) {
        let field = app.textViews["messageComposer"]
        field.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        field.typeText("Two files and a caption")
        for _ in 0..<2 {
            let renderer = UIGraphicsImageRenderer(size: CGSize(width: 48, height: 32))
            UIPasteboard.general.image = renderer.image { context in
                UIColor.systemTeal.setFill()
                context.fill(CGRect(x: 0, y: 0, width: 48, height: 32))
            }
            field.press(forDuration: 1.2)
            let paste = app.menuItems["Paste"].waitForExistence(timeout: 4) ? app.menuItems["Paste"] : app.buttons["Paste"]
            if paste.exists { paste.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap() }
            if app.alerts.buttons["Allow Paste"].waitForExistence(timeout: 1) { app.alerts.buttons["Allow Paste"].tap() }
        }
        _ = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Remove Pasted image-")).element(boundBy: 1).waitForExistence(timeout: 8)
    }

    private func keep(_ state: String) {
        let shot = XCUIScreen.main.screenshot()
        let attachment = XCTAttachment(screenshot: shot)
        attachment.name = state
        attachment.lifetime = .keepAlways
        add(attachment)
        if let path = ProcessInfo.processInfo.environment["PP_LAYOUT_SHOTS"], !path.isEmpty {
            let device = UIDevice.current.name.replacingOccurrences(of: " ", with: "_")
            try? shot.pngRepresentation.write(to: URL(fileURLWithPath: path).appendingPathComponent("\(device)-\(state).png"))
        }
    }
}

private extension XCUIElement.ElementType {
    var name: String {
        switch self {
        case .button: "button"
        case .textField: "field"
        case .secureTextField: "secure field"
        case .textView: "text view"
        case .searchField: "search field"
        case .switch, .toggle: "switch"
        case .link: "link"
        case .segmentedControl: "segmented control"
        case .slider: "slider"
        case .stepper: "stepper"
        case .menuButton, .popUpButton: "menu"
        default: "element \(rawValue)"
        }
    }
}

