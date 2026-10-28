package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Phase 3b groups on the device, end to end through the real group-crypto AAR: the "groups"
 * fixture is the app's mailbox plus Bob and Carol, each a real MLS device, on an in-memory postbox
 * that enforces docs/groups.md. Bob answers every message the app sends, so a reply proves the
 * message was encrypted here, decrypted by Bob, and his answer decrypted here again.
 */
@RunWith(AndroidJUnit4::class)
class GroupsUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()

    private fun launch(): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        return ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "groups"))
    }
    private fun dumped(work: () -> Unit) = try { work() } catch (e: Throwable) { ui.onAllNodes(isRoot()).fetchSemanticsNodes().indices.forEach { ui.onAllNodes(isRoot())[it].printToLog("PPDUMP", Int.MAX_VALUE) }; throw e }
    private fun tag(tag: String, ms: Long = 30_000) = dumped { ui.waitUntil(ms) { ui.onAllNodesWithTag(tag, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() } }
    private fun text(text: String, ms: Long = 30_000) = dumped { ui.waitUntil(ms) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() } }
    private fun groupRow(name: String) = ui.onNode(hasText(name, substring = true) and hasTestTagPrefix("group:"))
    private fun hasTestTagPrefix(prefix: String) = SemanticsMatcher("tag starts with $prefix") { node ->
        node.config.getOrElseNullable(androidx.compose.ui.semantics.SemanticsProperties.TestTag) { null }?.startsWith(prefix) == true
    }
    private fun send(message: String) {
        tag("groupComposer", 60_000)
        ui.onNodeWithTag("groupComposer").performClick().performTextInput(message)
        // The editor reports its text asynchronously; Send is enabled once it has.
        ui.waitUntil(10_000) { androidx.compose.ui.semantics.SemanticsProperties.Disabled !in ui.onNodeWithContentDescription("Send to the group").fetchSemanticsNode().config }
        // A notice ("Saved.") may be showing over the composer; press the button itself.
        ui.onNodeWithContentDescription("Send to the group").performSemanticsAction(androidx.compose.ui.semantics.SemanticsActions.OnClick)
    }

    @Test fun anAcceptedMemberIsAddedAndChatsEndToEnd() {
        launch().use {
            text("Garden club")
            groupRow("Garden club").performClick()
            tag("groupPane")
            text("end-to-end encrypted")
            // Bob's device adds this one; until then the composer waits and the status says why.
            send("Hello garden")
            text("Got it: Hello garden", 60_000)
            text("You joined this group. Earlier messages are not available on this device.")
            // Messages from before this device joined are not readable here, by design.
            ui.onAllNodesWithText("Tomatoes are in!").assertCountEquals(0)
        }
    }

    /** The composer keeps focus after a send (as in direct conversations), so the next message is typed at once. */
    @Test fun theGroupComposerKeepsFocusAfterSend() {
        launch().use {
            text("Garden club")
            groupRow("Garden club").performClick()
            tag("groupPane")
            send("First of two")
            text("Got it: First of two", 60_000)
            ui.onNodeWithTag("groupComposer").assertIsFocused()
            ui.onNodeWithTag("groupComposer").performTextInput("Second without tapping")
            ui.onNodeWithText("Second without tapping").assertExists()
        }
    }

    @Test fun anInvitationIsAcceptedAndAnnouncementModeReplacesTheComposer() {
        launch().use {
            tag("groupInvitations")
            text("Book club")
            ui.onNode(hasText("Join") and hasClickAction() and hasAnyAncestor(hasTestTagPrefix("invitation:"))).performClick()
            ui.waitUntil(30_000) { ui.onAllNodes(hasTestTagPrefix("invitation:")).fetchSemanticsNodes().isEmpty() }
            groupRow("Book club").performClick()
            tag("announcementBar", 60_000)
            ui.onNodeWithText("Only admins can send messages").assertExists()
            ui.onAllNodesWithTag("groupComposer").assertCountEquals(0)
        }
    }

    @Test fun aNewGroupIsCreatedHereWithTheMemberAddedByThisDevice() {
        launch().use {
            text("Garden club")
            ui.onNodeWithContentDescription("New conversation").performClick()
            ui.onNodeWithTag("menu:newGroup").performClick()
            ui.onNodeWithTag("groupWho").performTextInput("/bob/main")
            ui.onNodeWithText("Add").performClick()
            ui.onNodeWithTag("groupName").performTextInput("Release crew")
            ui.onNodeWithTag("createGroup").performScrollTo().performClick()
            tag("groupPane")
            ui.onNodeWithTag("groupTitle", useUnmergedTree = true).assertTextEquals("Release crew")
            // Bob accepts; this device claims his key package and commits the add. Only then can he read.
            text(" joined", 60_000)
            send("Shipping today")
            text("Got it: Shipping today", 60_000)
        }
    }

    @Test fun groupInfoShowsTheSafetyNumberFingerprintsAndSetsTheTimer() {
        launch().use {
            text("Garden club")
            groupRow("Garden club").performClick()
            tag("groupComposer", 60_000)
            ui.onNodeWithContentDescription("Group info").performClick()
            text("Members (")
            dumped { ui.waitUntil(30_000) { ui.onNodeWithTag("safetyNumber").fetchSemanticsNode().config.getOrElse(androidx.compose.ui.semantics.SemanticsProperties.Text) { emptyList() }
                .joinToString().count(Char::isDigit) == 60 } }
            ui.onAllNodesWithContentDescription("Actions for ", substring = true)[0].performScrollTo().performClick()
            ui.onNodeWithText("Devices and fingerprints").performClick()
            ui.waitUntil(30_000) { ui.onAllNodes(hasTestTagPrefix("fingerprint:")).fetchSemanticsNodes().isNotEmpty() }
            // Only the owner (Bob) may change the timer here; the control is shown but disabled.
            ui.onNodeWithTag("timerPicker").performScrollTo().assertIsNotEnabled()
            ui.onNodeWithContentDescription("Close Group info").performClick()
        }
    }

    @Test fun theOwnerSetsTheDisappearingTimerAndTheNoticeShows() {
        launch().use {
            text("Garden club")
            ui.onNodeWithContentDescription("New conversation").performClick()
            ui.onNodeWithTag("menu:newGroup").performClick()
            ui.onNodeWithTag("groupName").performTextInput("Timers")
            ui.onNodeWithTag("createGroup").performScrollTo().performClick()
            tag("groupPane")
            ui.onNodeWithContentDescription("Group info").performClick()
            ui.onNodeWithTag("timerPicker").performScrollTo().performClick()
            ui.onNodeWithText("1 hour").performClick()
            ui.onNodeWithContentDescription("Close Group info").performClick()
            text("You set messages to disappear after 1 hour")
            text("⏱ 1 hour")
            send("Gone in an hour")
            dumped { ui.waitUntil(30_000) { ui.onAllNodesWithContentDescription("Disappears", substring = true, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() } }
            assertTrue(ui.onAllNodesWithText("Gone in an hour").fetchSemanticsNodes().isNotEmpty())
        }
    }
}
