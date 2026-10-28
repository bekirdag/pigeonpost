package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.meetings.MeetingBrief
import dev.pigeonpost.core.meetings.MeetingDetail
import dev.pigeonpost.core.meetings.MeetingNotesCrypto
import dev.pigeonpost.core.meetings.MeetingNotesText
import dev.pigeonpost.inbox.meetings.KeystoreNotesKeys
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Meeting notes (Phase 7) on the device: the app registers its Android Keystore notes key, and the
 * fixture (standing in for meeting-intel) seals a brief for the registered key on request, so a brief
 * that opens proves the key agreement ran in the Keystore here.
 */
@RunWith(AndroidJUnit4::class)
class MeetingNotesUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()

    @Before fun reset() {
        LiveFixtureControl.reset()
        KeystoreNotesKeys(context, "fixture-meeting-notes").destroy()
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
    }
    @After fun after() { LiveFixtureControl.reset() }

    private fun dumped(work: () -> Unit) = try { work() } catch (e: Throwable) { ui.onAllNodes(isRoot()).fetchSemanticsNodes().indices.forEach { ui.onAllNodes(isRoot())[it].printToLog("PPDUMP", Int.MAX_VALUE) }; throw e }
    private fun text(text: String, ms: Long = 60_000) = dumped { ui.waitUntil(ms) { ui.onAllNodesWithText(text, substring = true, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() } }
    private fun tag(tag: String, ms: Long = 30_000) = dumped { ui.waitUntil(ms) { ui.onAllNodesWithTag(tag, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() } }

    private fun openNotes() {
        text("Garden club")
        // Phones reach it from Settings; tablets also from the sidebar.
        val sidebar = ui.onAllNodesWithTag("sidebar:meetingNotes").fetchSemanticsNodes().isNotEmpty()
        if (sidebar) ui.onNodeWithTag("sidebar:meetingNotes").performClick()
        else {
            ui.onAllNodesWithContentDescription("Settings")[0].performClick()
            ui.onNodeWithText("Meeting notes").performScrollTo().performClick()
        }
        tag("notes:list")
    }

    @Test fun briefsOpenWithThisDevicesKeystoreKeyAndCanBeDeleted() {
        ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "live")).use {
            openNotes()
            ui.waitUntil(30_000) { LiveFixtureControl.registered.size == 1 }
            val (keyId, publicKey) = LiveFixtureControl.registered.entries.single()
            assertTrue(MeetingNotesCrypto.validKeyId(keyId)); assertEquals(65, MeetingNotesCrypto.unb64(publicKey).size)
            ui.onNodeWithTag("notes:row:" + LiveFixtureControl.READY).performClick()
            tag("notes:brief")
            text("Agreed what to plant this spring; Bob brings the seeds.")
            text("/bob/main: Bring the seed packets (due Saturday)")
            text("02:10  /bob/main: I'll bring the seeds on Saturday.")
            text("Garden planning")
            ui.onNodeWithContentDescription("Back").performClick()
            ui.onNodeWithTag("notes:row:" + LiveFixtureControl.ELSEWHERE).performClick()
            text(MeetingNotesText.NO_KEY)
            ui.onNodeWithContentDescription("Back").performClick()
            ui.onNodeWithTag("notes:row:" + LiveFixtureControl.FAILED).performClick()
            text("No speech was captured.")
            ui.onNodeWithContentDescription("Back").performClick()
            ui.onNodeWithTag("notes:row:" + LiveFixtureControl.READY).performClick()
            tag("notes:brief")
            ui.onNodeWithTag("notes:delete").performClick()
            ui.onNodeWithText("Delete").performClick()
            ui.waitUntil(30_000) { LiveFixtureControl.READY in LiveFixtureControl.deleted }
            dumped { ui.waitUntil(30_000) { ui.onAllNodesWithTag("notes:row:" + LiveFixtureControl.READY).fetchSemanticsNodes().isEmpty() } }
            // The same key is kept for the next launch; it is not registered under a new id.
            assertEquals(keyId, KeystoreNotesKeys(context, "fixture-meeting-notes").current().keyId)
        }
    }

    @Test fun offForTheAccountThereIsNoMeetingNotesEntry() {
        LiveFixtureControl.notesOn = false
        ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "live")).use {
            text("Garden club")
            ui.onAllNodesWithTag("sidebar:meetingNotes").assertCountEquals(0)
            ui.onAllNodesWithContentDescription("Settings")[0].performClick()
            text("Help and about")
            ui.onAllNodesWithText("Meeting notes").assertCountEquals(0)
            assertTrue(LiveFixtureControl.registered.isEmpty())
        }
    }
}
