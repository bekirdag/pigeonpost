import XCTest

/// Real directory/preview/create screens and scoped transport against .invalid fixtures.
final class OrganizationRoomUITests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
    override func setUpWithError() throws { continueAfterFailure = false; XCUIDevice.shared.orientation = .portrait }
    override func tearDownWithError() throws {
        if let run = testRun, run.failureCount > 0 {
            let screenshot = XCTAttachment(screenshot: app.screenshot()); screenshot.lifetime = .keepAlways; add(screenshot)
            let tree = XCTAttachment(string: app.debugDescription); tree.lifetime = .keepAlways; add(tree)
        }
    }
    private func launch(_ extra: [String] = [], staged: Bool = true, features: String = "groups,orgs") {
        app.terminate()
        app.launchArguments = ["-fixtures", "-appearance=light", "-room-fixtures", "-features=" + features]
            + (staged ? ["-sheet=org-rooms"] : []) + extra
        app.launch()
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 10))
        if staged { XCTAssertTrue(app.navigationBars["Organization rooms"].waitForExistence(timeout: 15)) }
        else {
            // iPadOS 26 retains a collapsed conversation sidebar across fixture launches.
            // Follow its observed navigation control before inspecting the compose commands.
            let show = app.buttons["showSidebar"].firstMatch
            if show.waitForExistence(timeout: 2) {
                XCTAssertTrue(show.isEnabled && show.frame.width > 0 && app.frame.contains(show.frame))
                show.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
            }
        }
    }
    private func element(_ id: String) -> XCUIElement { app.descendants(matching: .any).matching(identifier: id).firstMatch }
    private func tap(_ item: XCUIElement, form: String = "org-rooms-directory") {
        let collection = app.collectionViews[form].firstMatch
        if collection.waitForExistence(timeout: 1) {
            for _ in 0..<14 {
                if item.exists && item.isHittable { break }
                let bounds = collection.frame.intersection(app.frame)
                let top = max(bounds.minY, app.navigationBars.allElementsBoundByIndex.last?.frame.maxY ?? bounds.minY) + 12
                var bottom = min(bounds.maxY, app.frame.maxY - 40) - 12
                if app.keyboards.firstMatch.exists { bottom = min(bottom, app.keyboards.firstMatch.frame.minY - 12) }
                let point = app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: bounds.midX, dy: (top + bottom) / 2))
                let direction: CGFloat = item.exists && item.frame.midY < top ? 1 : -1
                point.press(forDuration: 0.05, thenDragTo: point.withOffset(CGVector(dx: 0, dy: min(120, (bottom - top) / 4) * direction)))
            }
        }
        XCTAssertTrue(item.waitForExistence(timeout: 10)); XCTAssertTrue(item.isHittable); item.tap()
    }
    private func openRoom(_ id: String) {
        tap(element("org-room:" + id))
        XCTAssertTrue(app.navigationBars["Room"].waitForExistence(timeout: 10))
    }
    private func openComposeMenu() {
        let menu = element("composeMenu")
        XCTAssertTrue(menu.waitForExistence(timeout: 10))
        // SwiftUI exposes a Menu wrapper with a false hit flag around its visible button.
        // Tap the observed control's centre and require the actual menu to appear.
        XCTAssertTrue(menu.isEnabled && menu.frame.width > 0 && app.frame.contains(menu.frame))
        menu.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        let command = app.buttons.matching(NSPredicate(format: "label ==[c] %@", "New conversation")).firstMatch
        XCTAssertTrue(command.waitForExistence(timeout: 5))
    }
    private func expectConversation(_ name: String) {
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: element("org-room-preview-form"))
        XCTAssertEqual(XCTWaiter.wait(for: [gone], timeout: 12), .completed)
        XCTAssertFalse(element("org-rooms-directory").exists)
        XCTAssertTrue(app.staticTexts[name].firstMatch.waitForExistence(timeout: 10))
    }
    func testEntryRequiresBothOrganizationAndGroupCapabilities() {
        for features in ["groups", "orgs"] {
            launch(staged: false, features: features)
            let menu = element("composeMenu")
            if menu.waitForExistence(timeout: 5) {
                openComposeMenu()
                XCTAssertFalse(element("org-rooms-open").exists)
            } else {
                XCTAssertTrue(app.buttons["New conversation"].firstMatch.waitForExistence(timeout: 5))
                XCTAssertFalse(element("org-rooms-open").exists)
            }
        }
        launch(staged: false)
        openComposeMenu()
        // Through the shared menu helper (CalendarTests.swift): an iPad menu can swallow a tap
        // made while it is still opening; the directory must still appear.
        app.choose(element("org-rooms-open"), until: element("org-room:grp_room_public")) { self.openComposeMenu() }
        XCTAssertTrue(element("org-room:grp_room_public").waitForExistence(timeout: 10))
    }
    func testPublicRoomRequiresAnExplicitJoinBeforeOpening() {
        launch(); openRoom("grp_room_public")
        let action = app.buttons["org-room-join-open"].firstMatch
        XCTAssertTrue(action.waitForExistence(timeout: 10)); XCTAssertEqual(action.label, "Join room")
        XCTAssertTrue(app.navigationBars["Room"].exists)
        tap(action, form: "org-room-preview-form")
        expectConversation("Design room")
    }
    func testPrivateInvitationUsesAcceptanceAndOpensTheRoom() {
        launch(); openRoom("grp_room_private")
        let action = app.buttons["org-room-join-open"].firstMatch
        XCTAssertTrue(action.waitForExistence(timeout: 10)); XCTAssertEqual(action.label, "Accept invitation")
        tap(action, form: "org-room-preview-form")
        expectConversation("Hiring room")
    }
    func testPrivateDirectoryEntryDoesNotGrantMembership() {
        launch(); openRoom("grp_room_inviteonly")
        XCTAssertTrue(app.staticTexts["An invitation is required to join this room."].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["org-room-join-open"].exists)
    }
    func testReadOnlyOrganizationOffersNoCreationOrJoin() {
        launch(["-room-read-only"])
        XCTAssertTrue(element("org-room:grp_room_public").waitForExistence(timeout: 10))
        XCTAssertFalse(element("org-room-new").exists)
        openRoom("grp_room_public")
        XCTAssertTrue(app.staticTexts["This organization is read-only. You can open rooms you already belong to."].waitForExistence(timeout: 10))
        XCTAssertFalse(app.buttons["org-room-join-open"].exists)
    }
    func testOrganizationInboxSwitchRequiresConfirmation() {
        launch(["-room-other-inbox"])
        let choice = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "org-room-switch:")).firstMatch
        XCTAssertTrue(choice.waitForExistence(timeout: 10)); tap(choice)
        let cancel = app.buttons["Cancel"].firstMatch
        if cancel.waitForExistence(timeout: 1) {
            cancel.tap()
        } else {
            // iPad's native confirmation popover cancels by tapping outside it.
            XCTAssertTrue(app.buttons["Use inbox"].firstMatch.exists)
            app.navigationBars["Organization rooms"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
        let closed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: app.buttons["Use inbox"].firstMatch)
        XCTAssertEqual(XCTWaiter.wait(for: [closed], timeout: 5), .completed)
        XCTAssertFalse(element("org-room:grp_room_public").exists)
        tap(choice)
        let use = app.buttons["Use inbox"].firstMatch
        XCTAssertTrue(use.waitForExistence(timeout: 5)); use.tap()
        XCTAssertTrue(element("org-room:grp_room_public").waitForExistence(timeout: 15))
        XCTAssertFalse(choice.exists)
    }
    func testUncertainCreationRetainsItsDraftAndCanBeRetried() {
        launch(["-room-lost-create"])
        tap(element("org-room-new"))
        XCTAssertTrue(app.navigationBars["New room"].waitForExistence(timeout: 10))
        let name = app.textFields["org-room-name"].firstMatch
        tap(name, form: "org-room-create-form"); name.typeText("Planning room")
        tap(app.buttons["org-room-create"].firstMatch, form: "org-room-create-form")
        XCTAssertTrue(element("org-room-create-error").waitForExistence(timeout: 10))
        XCTAssertFalse(name.isEnabled)
        XCTAssertEqual(app.buttons["org-room-create"].firstMatch.label, "Retry creation")
        tap(app.buttons["org-room-create"].firstMatch, form: "org-room-create-form")
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: element("org-room-create-form"))
        XCTAssertEqual(XCTWaiter.wait(for: [gone], timeout: 15), .completed)
        XCTAssertFalse(element("org-rooms-directory").exists)
        XCTAssertTrue(app.staticTexts["Planning room"].firstMatch.waitForExistence(timeout: 10))
    }

    // ---- SDS §5.4: the org inside the ordinary inbox -------------------------------------------

    func testCompanySectionListsOrgRoomsAndBrowsesTheRest() {
        launch(["-org-surface"], staged: false)
        let section = element("company-section")
        XCTAssertTrue(section.waitForExistence(timeout: 15), "org rooms sit under a Company heading")
        XCTAssertTrue(app.staticTexts["General"].firstMatch.waitForExistence(timeout: 5), "the org room is listed")
        XCTAssertTrue(app.staticTexts["Garden club"].firstMatch.exists, "personal groups stay where they were")
        let browse = element("company-browse-rooms")
        XCTAssertTrue(browse.exists && browse.isHittable)
        browse.tap()
        XCTAssertTrue(app.navigationBars["Organization rooms"].waitForExistence(timeout: 10), "Browse rooms opens the directory")
    }

    func testCompanySectionNeedsBothCapabilities() {
        launch(["-org-surface"], staged: false, features: "groups")
        XCTAssertTrue(app.staticTexts["Garden club"].firstMatch.waitForExistence(timeout: 15))
        XCTAssertFalse(element("company-section").exists, "no Company heading while orgs is off")
    }

    func testMemberMailboxCarriesItsOrganization() {
        launch(["-org-surface"], staged: false)
        let line = element("mailboxOrganization")
        if line.waitForExistence(timeout: 8) {
            // The desktop layout (iPad) says it under the mailbox in the sidebar.
            XCTAssertEqual(line.label, "Example Co · Member")
        } else {
            // The phone says it in the mailbox switcher.
            let acting = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Acting as")).firstMatch
            XCTAssertTrue(acting.waitForExistence(timeout: 5))
            XCTAssertTrue(acting.label.contains("Example Co · Member"), acting.label)
            acting.tap()
            XCTAssertTrue(app.staticTexts["Example Co · Member"].firstMatch.waitForExistence(timeout: 10))
        }
    }

    func testComposeSuggestsColleaguesFromTheDirectory() {
        launch(["-org-surface"], staged: false)
        openComposeMenu()
        let new = app.buttons.matching(NSPredicate(format: "label ==[c] %@", "New conversation")).firstMatch
        app.choose(new, until: app.textFields["newConversationPeer"]) { self.openComposeMenu() }
        let field = app.textFields["newConversationPeer"]
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.tap(); field.typeText("ad")
        let ada = element("org-directory:/example/ada")
        XCTAssertTrue(ada.waitForExistence(timeout: 10), "a colleague matching what was typed")
        XCTAssertFalse(element("org-directory:/example/build").exists, "non-matching entries are not offered")
        ada.tap()
        XCTAssertEqual(field.value as? String, "/example/ada", "picking a colleague fills in their handle")
    }

    func testGroupsChipShowsOnlyGroupsOnTheDesktopLayout() throws {
        launch(["-org-surface"], staged: false)
        let chip = element("sideGroups")
        guard chip.waitForExistence(timeout: 8) else { throw XCTSkip("The Groups chip belongs to the desktop (iPad) layout") }
        let conversations = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "conversation:"))
        XCTAssertGreaterThan(conversations.count, 0)
        chip.tap()
        XCTAssertTrue(app.staticTexts["Garden club"].firstMatch.waitForExistence(timeout: 5))
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 0"), object: conversations)
        XCTAssertEqual(XCTWaiter.wait(for: [gone], timeout: 5), .completed, "one-to-one conversations are filtered out")
        element("sideInbox").tap()
        let back = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count > 0"), object: conversations)
        XCTAssertEqual(XCTWaiter.wait(for: [back], timeout: 5), .completed, "Inbox shows everything again")
    }
}
