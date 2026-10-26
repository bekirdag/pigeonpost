package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class CalendarWorkflowTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private fun shown(text: String) { ui.waitUntil(10_000) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() } }
    private fun launch(mode: String = "calendar"): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        return ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", mode)).also { scenario ->
            ui.waitUntil(10_000) { var focused = false; scenario.onActivity { focused = it.hasWindowFocus() }; focused }
            shown("Inbox")
            ui.waitUntil(10_000) { ui.onAllNodes(hasTestTag("tab:calendar") or hasTestTag("sidebar:calendar")).fetchSemanticsNodes().isNotEmpty() }
            ui.onNode(hasTestTag("tab:calendar") or hasTestTag("sidebar:calendar")).performClick()
            shown("Planning review")
        }
    }
    private fun click(text: String) = ui.onNodeWithText(text).performScrollTo().performClick()
    private fun save() = ui.onNodeWithTag("calendar:save").performScrollTo().performClick()
    private fun notice(identity: String = "/k/demomain", event: String? = "evt_review") = Intent(context, MainActivity::class.java)
        .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        .putExtra("pigeonpost.fixtures", "calendar").putExtra("push_type", "calendar").putExtra("push_identity", identity)
        .putExtra("calendar_event_id", event).putExtra("calendar_occurrence_id", if (event == null) null else "occ_$event")

    @Test fun agendaCreateAndEditKeepTheNativeNavigationAndDetails() {
        launch().use {
            ui.onNodeWithTag("calendar").assertExists()
            ui.onNodeWithTag("calendar:new").performClick()
            ui.onNodeWithTag("calendar:title").performTextInput("Native calendar event")
            save(); shown("Edit event")
            click("Edit event")
            ui.onNodeWithTag("calendar:title").assertTextContains("Native calendar event")
            ui.onNodeWithTag("calendar:title").performTextReplacement("Revised native event")
            save(); shown("Revised native event")
            ui.onNodeWithContentDescription("Close Event").performClick()
            shown("Calendar")
            ui.onNodeWithContentDescription("Back to inbox").performClick()
            shown("Inbox")
        }
    }
    @Test fun anAcceptedCreateWithALostResponseLocksAndRetriesTheOriginal() {
        launch("calendar-lost").use {
            ui.onNodeWithTag("calendar:new").performClick()
            ui.onNodeWithTag("calendar:title").performTextInput("One uncertain event")
            save(); shown("Retry original event")
            ui.onNodeWithTag("calendar:title").assertIsNotEnabled()
            click("Retry original event"); shown("Edit event")
            ui.onNodeWithContentDescription("Close Event").performClick()
            ui.waitUntil(10_000) { ui.onAllNodesWithText("Unconfirmed event").fetchSemanticsNodes().isEmpty() }
            ui.onNodeWithTag("calendar:agenda").performScrollToNode(hasText("One uncertain event"))
            ui.onAllNodesWithText("One uncertain event").assertCountEquals(1)
        }
    }
    @Test fun aRevisionConflictKeepsTheDraftWithoutRebasingIt() {
        launch("calendar-conflict").use {
            ui.onNodeWithTag("calendar:event:evt_review").performScrollTo().performClick()
            shown("Edit event"); click("Edit event")
            ui.onNodeWithTag("calendar:title").performTextReplacement("My reviewed change")
            save(); shown("This event changed while you were editing")
            ui.onNodeWithTag("calendar:title").performScrollTo().assertTextContains("My reviewed change")
            ui.onNodeWithContentDescription("Close Edit event").performClick()
            shown("Discard this draft?"); ui.onNodeWithText("Discard draft").performClick()
            click("Reload latest event"); shown("Changed by another client")
        }
    }
    @Test fun dstGapIsRefusedAndAFoldNeedsAnExplicitChoice() {
        launch().use {
            ui.onNodeWithTag("calendar:new").performClick()
            ui.onNodeWithTag("calendar:title").performTextInput("Clock change")
            ui.onNodeWithTag("calendar:start-date").performScrollTo().performTextReplacement("2026-03-08")
            ui.onNodeWithTag("calendar:end-date").performScrollTo().performTextReplacement("2026-03-08")
            ui.onNodeWithTag("calendar:start-time").performScrollTo().performTextReplacement("02:30")
            ui.onNodeWithTag("calendar:end-time").performScrollTo().performTextReplacement("03:30")
            ui.onNodeWithTag("calendar:zone").performScrollTo().performTextReplacement("America/New_York")
            save(); shown("That local time does not exist")
            ui.onNodeWithTag("calendar:start-date").performScrollTo().performTextReplacement("2026-11-01")
            ui.onNodeWithTag("calendar:end-date").performScrollTo().performTextReplacement("2026-11-01")
            ui.onNodeWithTag("calendar:start-time").performScrollTo().performTextReplacement("01:15")
            ui.onNodeWithTag("calendar:end-time").performScrollTo().performTextReplacement("01:45")
            save(); shown("That time happens twice")
            click("Choose offset")
            ui.onNodeWithText("Later offset").performClick()
            save(); shown("Edit event")
        }
    }
    @Test fun attendeesReceiveNoContactDetailsAndRsvpDoesNotAddContacts() {
        launch().use {
            ui.onNodeWithTag("calendar:event:evt_invited").performScrollTo().performClick()
            shown("Your answer: pending")
            ui.onAllNodesWithText("candidate@example.invalid", substring = true).assertCountEquals(0)
            ui.onAllNodesWithText("Get personal invitation link").assertCountEquals(0)
            click("Accept")
            shown("Your answer: accepted")
            ui.onNodeWithContentDescription("Close Event").performClick()
            ui.onNodeWithContentDescription("Back to inbox").performClick()
            ui.onNodeWithContentDescription("Settings").performClick()
            click("Contacts and permissions")
            ui.onAllNodesWithText("Guest").assertCountEquals(0)
        }
    }
    @Test fun hidingAllCalendarsShowsNoAgendaAndCanBeRestored() {
        launch().use {
            ui.onNodeWithContentDescription("Calendar settings").performClick()
            click("Calendars and subscriptions"); click("Hide all calendars")
            ui.onNodeWithContentDescription("Close Your calendars").performClick()
            shown("No calendars selected.")
            ui.onAllNodesWithTag("calendar:event:evt_review").assertCountEquals(0)
            ui.onNodeWithContentDescription("Calendar settings").performClick()
            click("Calendars and subscriptions"); click("Show all calendars")
            ui.onNodeWithContentDescription("Close Your calendars").performClick()
            shown("Planning review")
        }
    }
    @Test fun rotationPreservesAnUnsubmittedDraftWithoutSavingIt() {
        launch().use { scenario ->
            ui.onNodeWithTag("calendar:new").performClick()
            ui.onNodeWithTag("calendar:title").performTextInput("A draft through rotation")
            scenario.recreate()
            ui.onNodeWithTag("calendar:title").assertTextContains("A draft through rotation")
            ui.onNodeWithContentDescription("Close New event").performClick()
            shown("Discard this draft?")
            ui.onNodeWithText("Discard draft").performClick()
            ui.onAllNodesWithText("A draft through rotation").assertCountEquals(0)
        }
    }
    @Test fun coldStartCalendarNoticeOpensTheEventWithoutAnOrganizerConversation() {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        ActivityScenario.launch<MainActivity>(notice()).use {
            shown("A native calendar workflow")
            ui.onNodeWithContentDescription("Close Event").assertExists()
            ui.onAllNodesWithText("Message").assertCountEquals(0)
        }
    }
    @Test fun aNoticeWaitsForAnOpenDraftAndDoesNotReplaceItsText() {
        launch().use { scenario ->
            ui.onNodeWithTag("calendar:new").performClick()
            ui.onNodeWithTag("calendar:title").performTextInput("Keep this draft")
            scenario.onActivity { it.startActivity(notice()) }
            shown("A calendar notification is waiting")
            ui.onNodeWithTag("calendar:title").assertTextContains("Keep this draft")
            ui.onNodeWithContentDescription("Close New event").performClick()
            shown("Discard this draft?"); ui.onNodeWithText("Discard draft").performClick()
            shown("A native calendar workflow")
        }
    }
    @Test fun aNoticeForAnotherOwnedInboxAlsoWaitsBeforeChangingMailboxes() {
        launch().use { scenario ->
            ui.onNodeWithTag("calendar:new").performClick()
            ui.onNodeWithTag("calendar:title").performTextInput("Keep this mailbox draft")
            scenario.onActivity { it.startActivity(notice("/k/demoagent", null)) }
            shown("A calendar notification is waiting")
            ui.onNodeWithTag("calendar:title").assertTextContains("Keep this mailbox draft")
            ui.onNodeWithContentDescription("Close New event").performClick()
            shown("Discard this draft?"); ui.onNodeWithText("Discard draft").performClick()
            shown("/k/demoagent"); shown("No events on this day.")
        }
    }
}
