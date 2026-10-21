import XCTest

/// Groups on the phone (and iPad), driven against the fixture build: the list, invitations, the
/// encrypted view with its security lines, announcement mode, keying, group info, New group and
/// joining from a link. No network; `GroupStore.installFixtures` stages every state.
final class GroupTests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
    private let garden = "grp_fixturegarden000000000000"

    override func setUpWithError() throws { continueAfterFailure = false }

    override func tearDownWithError() throws {
        if let run = testRun, run.failureCount > 0 {
            let shot = XCTAttachment(screenshot: app.screenshot())
            shot.lifetime = .keepAlways
            add(shot)
        }
    }

    private func launch(_ extra: [String]) {
        app.terminate()
        app.launchArguments = ["-fixtures"] + extra
        app.launch()
    }

    private func element(_ id: String) -> XCUIElement { app.descendants(matching: .any)[id] }

    func testInvitationsFoldOpenWithJoinAndDecline() {
        launch([])
        let invitations = element("groupInvitations")
        XCTAssertTrue(invitations.waitForExistence(timeout: 10))
        XCTAssertTrue(element("group:Garden club").exists, "groups sit in the conversation list")
        invitations.tap()
        XCTAssertTrue(app.buttons["Join Book swap"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Decline Book swap"].exists)
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "someone you don't know")).firstMatch.exists,
                      "an invitation from a stranger says so")
    }

    func testGroupViewShowsEncryptionSecurityLinesAndSends() {
        launch(["-open=" + garden])
        XCTAssertTrue(element("groupE2EENote").waitForExistence(timeout: 10))
        XCTAssertTrue(element("groupSecurityNotice").exists, "a member's new device is shown as a security line")
        XCTAssertTrue(app.staticTexts["Saturday at ten?"].exists)
        // A narrow iPad column folds the video button into the bar's overflow; voice is always there.
        XCTAssertTrue(element("groupVoiceCall").exists)
        let composer = element("groupComposer")
        XCTAssertTrue(composer.exists)
        composer.tap()
        composer.typeText("Bringing gloves")
        element("groupSend").tap()
        XCTAssertTrue(app.staticTexts["Bringing gloves"].waitForExistence(timeout: 5))
    }

    func testTappingAGroupOpensIt() {
        launch([])
        let row = element("group:Garden club")
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.tap()
        XCTAssertTrue(element("groupComposer").waitForExistence(timeout: 8))
        XCTAssertTrue(element("groupTitle").exists)
    }

    func testAnnouncementGroupHasNoComposerForMembers() {
        launch(["-open=grp_fixtureannounce0000000000"])
        XCTAssertTrue(element("groupAnnounce").waitForExistence(timeout: 10))
        XCTAssertFalse(element("groupComposer").exists)
    }

    func testKeyingGroupSaysItIsWaitingToBeAdded() {
        launch(["-open=grp_fixturekeying00000000000"])
        XCTAssertTrue(element("groupStatus").waitForExistence(timeout: 10))
        XCTAssertFalse(element("groupComposer").exists)
    }

    func testGroupInfoShowsMembersSafetyNumberAndMembership() {
        launch(["-open=" + garden, "-sheet=group-info"])
        XCTAssertTrue(app.navigationBars["Group info"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["OWNER"].waitForExistence(timeout: 5) && app.staticTexts["ADMIN"].exists)
        let list = app.collectionViews.firstMatch
        for _ in 0..<8 where !element("groupSafetyNumber").isHittable { list.swipeUp() }
        XCTAssertTrue(element("groupSafetyNumber").exists)
        XCTAssertTrue(element("deviceFingerprint").exists, "this device's fingerprint is shown")
        for _ in 0..<6 where !element("deleteGroup").isHittable { list.swipeUp() }
        XCTAssertTrue(element("leaveGroup").exists && element("deleteGroup").exists, "the owner can leave or delete")
    }

    func testNewGroupNeedsANameAndTakesPeople() {
        launch(["-sheet=new-group"])
        XCTAssertTrue(app.navigationBars["New group"].waitForExistence(timeout: 10))
        let create = app.buttons["createGroup"].firstMatch
        XCTAssertFalse(create.isEnabled, "a group needs a name")
        let who = app.textFields["newGroupWho"].firstMatch
        who.tap()
        who.typeText("alp/main\n")
        XCTAssertTrue(app.buttons["Remove /alp/main"].waitForExistence(timeout: 5), "an address becomes a chip")
        let name = app.textFields["newGroupName"].firstMatch
        name.tap()
        name.typeText("Allotment")
        XCTAssertTrue(create.isEnabled)
    }

    func testJoinLinkShowsAPreviewFirst() {
        launch(["-join=grp_fixturejoin00000000000000"])
        XCTAssertTrue(app.staticTexts["Neighbourhood swap"].waitForExistence(timeout: 10))
        XCTAssertTrue(element("joinGroup").exists)
    }
}
