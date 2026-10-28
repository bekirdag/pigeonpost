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

    private func typeInComposer(_ text: String) {
        let field = element("groupComposer")
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        for _ in 0..<3 where !app.keyboards.firstMatch.exists {
            let frame = field.frame
            app.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: frame.midX, dy: frame.midY)).tap()
            if app.keyboards.firstMatch.waitForExistence(timeout: 2) { break }
        }
        XCTAssertTrue(app.keyboards.firstMatch.exists)
        field.typeText(text)
    }

    private func expectDraft(_ text: String, file: StaticString = #filePath, line: UInt = #line) {
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "value == %@", text), object: element("groupComposer"))
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 5), .completed, file: file, line: line)
    }

    func testGroupReplyPreservesDraftAndShowsAWorkingQuote() {
        launch(["-open=" + garden, "-group-actions"])
        XCTAssertTrue(element("group-reply:10").waitForExistence(timeout: 10))
        typeInComposer("A group reply")
        element("group-reply:10").tap()
        XCTAssertEqual(element("groupComposer").value as? String, "A group reply")
        element("cancel-group-action").tap()
        XCTAssertEqual(element("groupComposer").value as? String, "A group reply")
        element("group-reply:10").tap()
        element("groupSend").tap()
        XCTAssertTrue(app.staticTexts["A group reply"].waitForExistence(timeout: 5))
        let quote = element("group-quote:11")
        XCTAssertTrue(quote.waitForExistence(timeout: 5), "the sent reply retains its authenticated parent")
        quote.tap()
        XCTAssertTrue(app.staticTexts["Edited group message"].exists, "a quote navigates to its retained parent")
    }

    func testGroupEditCancelSaveRetainsDraftAndVersionInfo() {
        launch(["-open=" + garden, "-group-actions"])
        XCTAssertTrue(element("group-edit:10").waitForExistence(timeout: 10))
        element("group-info:10").tap()
        XCTAssertTrue(app.navigationBars["Message info"].waitForExistence(timeout: 5))
        XCTAssertEqual(element("group-receipt:/k/aaaa1111bbbb2222cccc3333dd").value as? String, "Seen")
        XCTAssertEqual(element("group-receipt:/k/eeee5555ffff6666gggg7777hh").value as? String, "Delivered",
                       "the member hiding a read cursor makes no Seen claim")
        app.buttons["Done"].tap()
        typeInComposer("Ordinary draft")
        element("group-edit:10").tap()
        expectDraft("Edited group message")
        element("cancel-group-action").tap()
        expectDraft("Ordinary draft")
        element("group-edit:10").tap()
        expectDraft("Edited group message")
        typeInComposer(" revised")
        element("groupSend").tap()
        XCTAssertTrue(app.staticTexts["Edited group message revised"].waitForExistence(timeout: 5))
        XCTAssertEqual(element("groupComposer").value as? String, "Ordinary draft")
        element("group-info:11").tap()
        XCTAssertTrue(app.navigationBars["Message info"].waitForExistence(timeout: 5))
        XCTAssertTrue(element("group-version:9").exists && element("group-version:10").exists && element("group-version:11").exists,
                      "all locally received versions remain available")
        XCTAssertEqual(element("group-receipt:/k/aaaa1111bbbb2222cccc3333dd").value as? String, "Sent",
                       "a cursor behind the new edit makes no delivery/read claim")
    }

    func testGroupDeletionDoesNotRevealAnEarlierVersion() {
        launch(["-open=" + garden, "-group-actions"])
        let more = element("group-more:10")
        XCTAssertTrue(more.waitForExistence(timeout: 10))
        more.tap()
        let delete = app.buttons["Delete for everyone"].firstMatch
        XCTAssertTrue(delete.waitForExistence(timeout: 5))
        delete.tap()
        app.buttons["Delete"].firstMatch.tap()
        XCTAssertTrue(app.staticTexts["This message was deleted"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.staticTexts["Original group message"].exists)
        XCTAssertFalse(app.staticTexts["Edited group message"].exists)
        XCTAssertFalse(element("group-edit:10").exists)
    }

    func testGroupMemberMenuCannotDeleteAnotherMembersMessage() {
        launch(["-open=grp_fixtureannounce0000000000"])
        let more = element("group-more:5")
        XCTAssertTrue(more.waitForExistence(timeout: 10))
        more.tap()
        XCTAssertTrue(app.buttons["Block sender…"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["Delete for everyone"].exists, "a member has no admin deletion control")
    }

    func testGroupMessageControlsRespectCapabilities() {
        launch(["-open=" + garden, "-group-actions", "-features=groups"])
        XCTAssertTrue(element("groupComposer").waitForExistence(timeout: 10))
        XCTAssertFalse(element("group-reply:10").exists)
        XCTAssertFalse(element("group-edit:10").exists)
        XCTAssertFalse(element("group-info:10").exists)
    }

    func testGroupUnconfirmedSendCanRetryWithoutClearingAnotherDraft() {
        launch(["-open=" + garden, "-group-failed-send"])
        let retry = element("group-retry:cmid_fixture_retry")
        XCTAssertTrue(retry.waitForExistence(timeout: 10))
        typeInComposer("Independent draft")
        retry.tap()
        XCTAssertTrue(app.staticTexts["Waiting for retry"].waitForExistence(timeout: 5))
        XCTAssertFalse(element("group-retry:cmid_fixture_retry").exists)
        XCTAssertEqual(element("groupComposer").value as? String, "Independent draft")
        XCTAssertEqual(app.staticTexts.matching(NSPredicate(format: "label == %@", "Waiting for retry")).count, 1)
    }

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
        XCTAssertTrue(app.navigationBars["Group info"].waitForExistence(timeout: 20))
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

    // ---- Phase 6: live streams --------------------------------------------------------------------

    private let announce = "grp_fixtureannounce0000000000"

    private func keyboardFocus(_ id: String) -> Bool {
        (element(id).value(forKey: "hasKeyboardFocus") as? Bool) == true
    }

    func testLiveBannerShowsWhoIsLiveAndWatchGoesToTheCallView() {
        launch(["-open=" + announce])
        let banner = element("groupLiveBanner")
        XCTAssertTrue(banner.waitForExistence(timeout: 10), "a live stream shows the LIVE banner")
        XCTAssertTrue(banner.label.contains("is live: 0.9.0 walkthrough") && banner.label.hasSuffix("Watch"), banner.label)
        XCTAssertTrue(element("groupTitle").exists, "the banner leaves the group bar as it was")
        XCTAssertFalse(element("groupGoLive").exists, "a member may not go live where only admins can")
        banner.tap()
        // The fixture build has no call page, so the call view refuses; the refusal is said, not lost.
        XCTAssertTrue(app.staticTexts["Live streams aren't ready yet. Try again in a moment, or close and reopen the app."].waitForExistence(timeout: 8))
    }

    func testGoLiveNeedsATitleBeforeItStarts() {
        launch(["-open=" + garden])
        // On a phone Go live is in group info: the bar keeps its three buttons and the title.
        XCTAssertTrue(element("groupTitle").waitForExistence(timeout: 10), "the group bar keeps its title")
        XCTAssertFalse(element("groupGoLive").exists, "no fourth button in a phone's group bar")
        XCTAssertFalse(element("groupLiveBanner").exists, "nobody is live in this group")
        element("groupInfo").firstMatch.tap()
        let goLive = element("groupInfoGoLive").firstMatch
        XCTAssertTrue(goLive.waitForExistence(timeout: 10), "the owner may go live")
        goLive.tap()
        XCTAssertTrue(app.navigationBars["Go live"].waitForExistence(timeout: 5))
        let start = app.buttons["goLiveStart"].firstMatch
        XCTAssertFalse(start.isEnabled, "a stream needs a title")
        let title = app.textFields["goLiveTitle"].firstMatch
        if !app.keyboards.firstMatch.waitForExistence(timeout: 3) { title.tap() }
        title.typeText("Seed swap")
        XCTAssertTrue(start.isEnabled)
        XCTAssertTrue(element("goLiveCamera").exists)
        XCTAssertFalse(element("goLiveScreen").exists, "an iPhone or iPad web view cannot share its screen")
        let camera = app.switches["goLiveCamera"].firstMatch
        camera.switches.firstMatch.exists ? camera.switches.firstMatch.tap() : camera.tap()
        XCTAssertFalse(start.isEnabled, "something has to be shared")
        app.buttons["Cancel"].firstMatch.tap()
        XCTAssertTrue(element("groupTitle").waitForExistence(timeout: 5))
    }

    func testGroupComposerKeepsFocusAfterSend() {
        launch(["-open=" + garden])
        typeInComposer("First line out")
        element("groupSend").tap()
        XCTAssertTrue(app.staticTexts["First line out"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 3), "the keyboard stays up after Send")
        XCTAssertTrue(keyboardFocus("groupComposer"), "the group composer keeps focus after Send")
        element("groupComposer").typeText("Second without tapping")
        expectDraft("Second without tapping")
    }

    // ---- Phase 7: meeting notes -------------------------------------------------------------------

    func testMeetingNotesListOpensBriefsAndExplainsOthers() {
        launch(["-sheet=meeting-notes"])
        let ready = element("meeting-mtg_fixtureready00000000000000")
        XCTAssertTrue(ready.waitForExistence(timeout: 10), "the reader lists meetings")
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS 'Being prepared'")).firstMatch.exists)
        ready.tap()
        XCTAssertTrue(element("meetingNoteSummary").waitForExistence(timeout: 5), "a ready brief opens")
        XCTAssertTrue(app.staticTexts["Ship 0.9.1 on Thursday"].exists && app.staticTexts["Do we need a migration note for Windows?"].exists)
        XCTAssertTrue(app.staticTexts["/alp/main: Land the calendar fix (due Wednesday)"].exists, "action items read owner: task (due)")
        element("meetingNoteCopy").tap()
        XCTAssertTrue(app.staticTexts["Copied."].waitForExistence(timeout: 3))
        let back = app.navigationBars.buttons["Meeting notes"].firstMatch
        XCTAssertTrue(back.waitForExistence(timeout: 5))
        back.tap()
        let other = element("meeting-mtg_fixtureother00000000000000")
        XCTAssertTrue(other.waitForExistence(timeout: 5))
        other.tap()
        let message = element("meetingNoteMessage")
        XCTAssertTrue(message.waitForExistence(timeout: 5))
        XCTAssertTrue(message.label.hasPrefix("These notes were encrypted for your other devices."), message.label)
        element("meetingNoteDelete").tap()
        let confirm = app.buttons.matching(NSPredicate(format: "label == 'Delete my copy' AND identifier != 'meetingNoteDelete'")).firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5), "deleting asks first")
        confirm.tap()
        XCTAssertTrue(element("meeting-mtg_fixtureready00000000000000").waitForExistence(timeout: 5))
        XCTAssertFalse(element("meeting-mtg_fixtureother00000000000000").exists, "the deleted copy leaves the list")
    }

    func testMeetingNotesEntryOnlyWhenTheFeatureIsOn() {
        launch([])
        XCTAssertTrue(element("meetingNotesButton").waitForExistence(timeout: 10))
        launch(["-features=groups"])
        XCTAssertTrue(element("composeMenu").waitForExistence(timeout: 10) || app.buttons["New conversation"].exists)
        XCTAssertFalse(element("meetingNotesButton").exists, "no reader on a postbox without meeting notes")
    }
}
