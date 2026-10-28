package dev.pigeonpost.inbox

import android.app.Application
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.inbox.ui.CropDialog
import dev.pigeonpost.inbox.ui.PigeonpostApp
import dev.pigeonpost.inbox.ui.PigeonpostTheme
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Phase 1b on a phone or a tablet: Calls tab or sidebar entry, missed-call lines, Filtered, blocking, privacy settings, pictures and timers. */
@RunWith(AndroidJUnit4::class)
class SafetyUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private fun launch(): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        return ActivityScenario.launch(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "safety"))
    }
    private fun dumped(work: () -> Unit) = try { work() } catch (e: Throwable) { ui.onAllNodes(isRoot()).fetchSemanticsNodes().indices.forEach { ui.onAllNodes(isRoot())[it].printToLog("PPDUMP", Int.MAX_VALUE) }; throw e }
    private fun tag(tag: String) = dumped { ui.waitUntil(30_000) { ui.onAllNodesWithTag(tag, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() } }
    private fun calls() = dumped { ui.waitUntil(30_000) { ui.onAllNodes(callsEntry).fetchSemanticsNodes().isNotEmpty() } }
    private fun text(text: String) = dumped { ui.waitUntil(30_000) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() } }

    @Test fun callsTabShowsTheMissedBadgeFiltersAndClearsItWhenViewed() {
        launch().use {
            calls()
            ui.waitUntil(15_000) { ui.onAllNodesWithContentDescription("Calls, 1 missed").fetchSemanticsNodes().isNotEmpty() }
            ui.onNode(callsEntry).performClick()
            tag("callsPane")
            tag("call:call_missed"); tag("call:call_out")
            ui.waitUntil(15_000) { ui.onAllNodesWithContentDescription("Calls, 1 missed").fetchSemanticsNodes().isEmpty() }
            if (wideLayout()) {
                // On a tablet the Calls list replaces its sidebar entry; go back and check the badge really cleared.
                ui.onNodeWithContentDescription("Back to conversations").performClick()
                calls()
                ui.onNode(callsEntry).assertContentDescriptionEquals("Calls")
                ui.onNode(callsEntry).performClick()
                tag("call:call_out")
            }
            ui.onNodeWithText("Missed").performClick()
            ui.waitUntil(15_000) { ui.onAllNodesWithTag("call:call_out").fetchSemanticsNodes().isEmpty() }
            tag("call:call_missed")
            ui.onNodeWithTag("filteredCalls").performClick()
            tag("call:call_filtered")
            ui.onNodeWithText("All").performClick()
            ui.onAllNodesWithContentDescription("More actions for this call")[0].performClick()
            ui.onNodeWithText("Delete from history").performClick()
            text("Removed from call history.")
        }
    }

    @Test fun blockingFromACallRowAsksFirstAndReportsTheCall() {
        launch().use {
            calls(); ui.onNode(callsEntry).performClick()
            tag("call:call_missed")
            ui.onAllNodesWithContentDescription("More actions for this call")[0].performClick()
            ui.onNodeWithText("Block").performClick()
            tag("blockSheet")
            ui.onNodeWithText("They won’t be able to message or call you, and won’t be told.").assertExists()
            ui.onNodeWithText("Block everyone at /demo").assertExists()
            ui.onNodeWithText("Also report as spam").performClick()
            ui.onNode(hasText("Block") and hasClickAction() and hasAnyAncestor(isDialog())).performClick()
            text("Blocked and reported as spam.")
        }
    }

    @Test fun aMissedCallIsALineInTheConversationAndThePictureReplacesTheInitials() {
        launch().use {
            tag("conversation:/demo/builder")
            ui.waitUntil(30_000) { ui.onAllNodesWithTag("avatar:/demo/builder:photo", useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithTag("conversation:/demo/builder").performClick()
            tag("missedCall")
            ui.onNodeWithText("Missed video call", substring = true).assertExists()
        }
    }

    @Test fun filteredSendersAreAcceptedIntoTheInbox() {
        launch().use {
            tag("filteredEntry")
            ui.onNodeWithTag("filteredEntry").performScrollTo().performClick()
            tag("filteredPane")
            ui.onAllNodes(hasTestTag("filtered:/k/stranger7ab3c9d0e1f2a3b4c5"))[0].performClick()
            text("Following up on my earlier message.")
            ui.onNodeWithTag("acceptFiltered").performScrollTo().performClick()
            text("Accepted. Their messages are in your inbox.")
            text("Hi! Are you hiring agents?")
            ui.onAllNodesWithTag("filteredEntry").assertCountEquals(0)
        }
    }

    @Test fun blockingFromTheHeaderShowsTheBannerAndUnblockBringsTheComposerBack() {
        launch().use {
            // /demo/builder is one of this account's own mailboxes, which cannot be blocked; the reviewer can.
            tag("conversation:/demo/reviewer"); ui.onNodeWithTag("conversation:/demo/reviewer").performClick()
            ui.onNodeWithContentDescription("More conversation actions").performClick()
            ui.onNodeWithText("Block").performClick()
            tag("blockSheet")
            ui.onNode(hasText("Block") and hasClickAction() and hasAnyAncestor(isDialog())).performClick()
            tag("blockedBanner")
            ui.onAllNodesWithText("Message").assertCountEquals(0)
            // The "Blocked" snackbar can sit over the banner; press the button itself.
            ui.onNode(hasText("Unblock") and hasClickAction() and hasAnyAncestor(hasTestTag("blockedBanner"))).performSemanticsAction(androidx.compose.ui.semantics.SemanticsActions.OnClick)
            dumped { ui.waitUntil(15_000) { ui.onAllNodesWithTag("blockedBanner").fetchSemanticsNodes().isEmpty() } }
            text("Message")
        }
    }

    @Test fun privacySettingsSaveAndTheBlockedListUnblocks() {
        launch().use {
            calls()
            ui.onNodeWithContentDescription("Settings").performClick()
            ui.onNodeWithText("Privacy & filtering").performScrollTo().performClick()
            text("Calls from people you don’t know")
            ui.onNode(hasText("Silence") and hasAnyAncestor(hasTestTag("policy:unknown_calls"))).performClick()
            text("Saved.")
            ui.onNodeWithText("Blocked").performScrollTo().performClick()
            tag("blockedEmpty")
            ui.onNodeWithText("Block an address").performTextInput("/spamco/*")
            ui.onNode(hasText("Block") and hasClickAction() and !hasAnyAncestor(hasTestTagStart("blocked:"))).performScrollTo().performClick()
            tag("blocked:/spamco/*")
            ui.onNodeWithContentDescription("Unblock /spamco/*").performClick()
            ui.onNode(hasText("Unblock") and hasClickAction() and !hasContentDescription("Unblock /spamco/*")).performClick()
            ui.waitUntil(15_000) { ui.onAllNodesWithTag("blocked:/spamco/*").fetchSemanticsNodes().isEmpty() }
        }
    }

    @Test fun profilePicturePageChoosesWhoMaySeeIt() {
        launch().use {
            calls()
            ui.onNodeWithContentDescription("Settings").performClick()
            ui.onNodeWithText("Profile picture").performScrollTo().performClick()
            text("No picture yet. People see your initials.")
            tag("choosePicture")
            ui.onNodeWithTag("visibility:everyone").performScrollTo().performClick()
            ui.onNodeWithTag("visibility:everyone").assertIsSelected()
            ui.onNodeWithText("Use for all my mailboxes").assertExists()
        }
    }

    @Test fun theDirectMessageTimerIsSetFromConversationInfo() {
        launch().use {
            tag("conversation:/demo/builder"); ui.onNodeWithTag("conversation:/demo/builder").performClick()
            ui.onNodeWithText("Android build").performScrollTo().performClick()
            ui.onNodeWithContentDescription("Conversation info").performClick()
            ui.onNodeWithTag("timerPicker").performScrollTo().performClick()
            ui.onNodeWithText("1 day").performClick()
            text("New messages disappear after 1 day.")
            ui.onNodeWithContentDescription("Close Conversation info").performClick()
            tag("timerBadge")
        }
    }
}

private fun hasTestTagStart(prefix: String) = SemanticsMatcher("tag starts with $prefix") { node ->
    node.config.getOrElseNullable(androidx.compose.ui.semantics.SemanticsProperties.TestTag) { null }?.startsWith(prefix) == true
}

/** The square crop: what Save hands back is a square inside the photo, at any zoom. */
@RunWith(AndroidJUnit4::class)
class CropDialogTest {
    @get:Rule val ui = createAndroidComposeRule<ComponentActivity>()
    @Test fun saveReturnsASquareInsideThePicture() {
        val bitmap = Bitmap.createBitmap(1200, 800, Bitmap.Config.ARGB_8888)
        var saved: Triple<Int, Int, Int>? = null
        ui.setContent { PigeonpostTheme(false) { CropDialog(bitmap, false, null, { l, t, s -> saved = Triple(l, t, s) }, {}) } }
        ui.onNodeWithTag("savePicture").performClick()
        val (l, t, side) = saved!!
        assertEquals("zoom 1 takes the full short side", 800, side, 2)
        assertEquals("centred", 200, l, 2); assertEquals(0, t, 2)
        val (bytes, size) = dev.pigeonpost.inbox.ui.encodeCrop(bitmap, l, t, side)
        assertEquals(800, size)
        assertTrue("re-encoded as JPEG", bytes[0] == 0xFF.toByte() && bytes[1] == 0xD8.toByte())
    }
    private fun assertEquals(message: String, expected: Int, actual: Int, slack: Int) = assertTrue("$message: $actual", kotlin.math.abs(expected - actual) <= slack)
    private fun assertEquals(expected: Int, actual: Int, slack: Int) = assertEquals("value", expected, actual, slack)
}

/** The tablet sidebar: Calls under the mailbox switcher, and Filtered at the end of the list. */
@RunWith(AndroidJUnit4::class)
class SafetyTabletTest {
    @get:Rule val ui = createAndroidComposeRule<ComponentActivity>()
    @OptIn(ExperimentalTestApi::class)
    @Test fun callsAndFilteredLiveInTheSidebar() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        app.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        val model = InboxViewModel(app, Development.graph(app, Intent().putExtra("pigeonpost.fixtures", "safety"))!!)
        ui.setContent {
            DeviceConfigurationOverride(DeviceConfigurationOverride.ForcedSize(DpSize(1280.dp, 800.dp))) {
                PigeonpostApp(model, { _, _ -> }, {}, {}, {}, { _, _ -> }, {})
            }
        }
        ui.waitUntil(15_000) { ui.onAllNodesWithTag("sidebar:calls").fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithContentDescription("Calls, 1 missed").assertExists()
        ui.onNodeWithTag("sidebar:calls").performClick()
        ui.waitUntil(15_000) { ui.onAllNodesWithTag("call:call_missed").fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithTag("call:call_missed").performClick()
        ui.waitUntil(15_000) { ui.onAllNodesWithTag("missedCall", useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithContentDescription("Back to conversations").performClick()
        ui.waitUntil(15_000) { ui.onAllNodesWithTag("filteredEntry").fetchSemanticsNodes().isNotEmpty() }
    }
}
