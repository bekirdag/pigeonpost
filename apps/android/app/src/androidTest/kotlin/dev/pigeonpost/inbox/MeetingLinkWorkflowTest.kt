package dev.pigeonpost.inbox

import android.Manifest
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import org.json.JSONArray
import org.json.JSONObject
import org.json.JSONTokener
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MeetingLinkWorkflowTest {
    @get:Rule val ui = createEmptyComposeRule()
    @get:Rule val permissions: GrantPermissionRule = GrantPermissionRule.grant(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA)
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private fun shown(text: String) { ui.waitUntil(10_000) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() } }
    private fun intent(page: String, link: Boolean = true, mode: String = "calendar") = Intent(context, MainActivity::class.java).apply {
        putExtra("pigeonpost.fixtures", mode); putExtra("pigeonpost.calls.page", page)
        if (link) { action = Intent.ACTION_VIEW; data = Uri.parse("https://inbox.pigeonpost.dev/m/ABCDE-FGHJK") }
    }
    private fun launch(page: String, link: Boolean = true, mode: String = "calendar"): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        val scenario = ActivityScenario.launch<MainActivity>(intent(page, link, mode))
        try {
            // A Dialog owns focus separately from its Activity window.
            if (link) shown("Meeting ABCDE-FGHJK") else {
                ui.waitUntil(10_000) { var focused = false; scenario.onActivity { focused = it.hasWindowFocus() }; focused }
                shown("Inbox")
            }
            return scenario
        } catch (failure: Throwable) { scenario.close(); throw failure }
    }
    private fun web(view: View): WebView? {
        if (view is WebView && view.contentDescription == "Call") return view
        if (view is ViewGroup) for (index in 0 until view.childCount) web(view.getChildAt(index))?.let { return it }
        return null
    }
    private fun joined(scenario: ActivityScenario<MainActivity>): JSONArray {
        var result: JSONArray? = null
        var visible = false
        var lastState = "Call view missing"
        try {
            ui.waitUntil(10_000) {
                scenario.onActivity { activity ->
                    web(activity.window.decorView)?.let { target ->
                        visible = target.alpha > .9f
                        target.evaluateJavascript("JSON.stringify({ready:!!(window.options&&window.options.token),events:(window.events||[]).map(x=>x.type),joins:(window.events||[]).filter(x=>x.type==='join')})") { encoded ->
                            val decoded = runCatching { JSONTokener(encoded).nextValue() }.getOrNull()
                            val state = (decoded as? String)?.let { runCatching { JSONObject(it) }.getOrNull() }
                            result = state?.optJSONArray("joins")
                            lastState = "visible=$visible, fixture=$state"
                        }
                    }
                }
                visible && result?.length() == 1
            }
        } catch (failure: Throwable) {
            // These screens contain only the explicit synthetic fixture. Capture the disposable
            // emulator's actual failure state before ActivityScenario closes it; never a real device.
            if (InstrumentationRegistry.getArguments().getString("pigeonpost_test_emulator") == "true") runCatching {
                android.os.ParcelFileDescriptor.AutoCloseInputStream(InstrumentationRegistry.getInstrumentation().uiAutomation
                    .executeShellCommand("screencap -p /data/local/tmp/pp-meeting-link-failure.png")).use { it.readBytes() }
            }
            val layout = runCatching { ui.onAllNodes(isRoot(), useUnmergedTree = true).printToString().take(16_000) }.getOrDefault("Fixture layout unavailable")
            throw AssertionError("$lastState; ${windowInputStatus()}; fixtureLayout=$layout", failure)
        }
        return result!!
    }
    @Test fun openingALinkOnlyLooksUpAndExplicitOccurrenceSelectionEnablesJoining() {
        ScheduledCallHostTest.Page().use { page -> launch(page.url).use { scenario ->
            ui.waitUntil(10_000) { ui.onAllNodesWithTag("meeting:occurrence:occ_second").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithTag("meeting:voice").performScrollTo().assertIsNotEnabled()
            ui.onNodeWithTag("meeting:occurrence:occ_second").performScrollTo().performClick()
            ui.onNodeWithTag("meeting:voice").performScrollTo().assertIsEnabled().performClick()
            val event = joined(scenario).getJSONObject(0)
            assertEquals("ABCDEFGHJK", event.getString("code"))
            val options = event.getJSONObject("options")
            assertEquals("/k/demomain", options.getString("as")); assertEquals("occ_second", options.getString("occurrenceId")); assertEquals("audio", options.getString("kind"))
        } }
    }
    @Test fun changingTheInboxClearsOldSelectionAndRechecksAvailability() {
        ScheduledCallHostTest.Page().use { page -> launch(page.url).use {
            ui.waitUntil(10_000) { ui.onAllNodesWithTag("meeting:occurrence:occ_first").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithTag("meeting:occurrence:occ_first").performScrollTo().performClick()
            ui.onNode(hasText("Demo") and hasAnyAncestor(isDialog())).performScrollTo().performClick()
            ui.onNodeWithText("Demo Agent").performClick()
            shown("This meeting is unavailable to the selected inbox.")
            ui.onNodeWithTag("meeting:voice").performScrollTo().assertIsNotEnabled()
            ui.onNode(hasText("Demo Agent") and hasAnyAncestor(isDialog())).performScrollTo().performClick()
            ui.onNodeWithText("Demo").performClick()
            ui.waitUntil(10_000) { ui.onAllNodesWithTag("meeting:occurrence:occ_first").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithTag("meeting:voice").performScrollTo().assertIsNotEnabled()
        } }
    }
    @Test fun repeatingTheSamePublicLinkReloadsRatherThanLeavingAStuckPicker() {
        ScheduledCallHostTest.Page().use { page -> launch(page.url).use { scenario ->
            ui.waitUntil(10_000) { ui.onAllNodesWithTag("meeting:occurrence:occ_first").fetchSemanticsNodes().isNotEmpty() }
            scenario.onActivity { it.startActivity(intent(page.url).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)) }
            ui.waitUntil(10_000) { ui.onAllNodesWithTag("meeting:occurrence:occ_second").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithTag("meeting:video").performScrollTo().assertIsNotEnabled()
        } }
    }
    @Test fun savedZoneUpdatesTheAlreadyOpenMeetingPicker() {
        val original = java.util.TimeZone.getDefault()
        try {
            java.util.TimeZone.setDefault(java.util.TimeZone.getTimeZone("UTC"))
            ScheduledCallHostTest.Page().use { page -> launch(page.url, mode = "calendar-zone-delay").use {
                shown("UTC")
                shown("Europe/Istanbul")
            } }
        } finally { java.util.TimeZone.setDefault(original) }
    }
    @Test fun aMeetingLinkWaitsUntilAnUnsentCalendarDraftIsClosed() {
        ScheduledCallHostTest.Page().use { page -> launch(page.url).use { scenario ->
            ui.onNodeWithContentDescription("Close Meeting ABCDE-FGHJK").performClick()
            ui.waitUntil(10_000) { ui.onAllNodes(hasTestTag("tab:calendar") or hasTestTag("sidebar:calendar")).fetchSemanticsNodes().isNotEmpty() }
            ui.onNode(hasTestTag("tab:calendar") or hasTestTag("sidebar:calendar")).performClick()
            shown("Planning review"); ui.onNodeWithTag("calendar:new").performClick()
            ui.onNodeWithTag("calendar:title").performTextInput("Keep before the link")
            scenario.onActivity { it.startActivity(intent(page.url).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)) }
            shown("Another calendar item is waiting")
            ui.onNodeWithTag("calendar:title").assertTextContains("Keep before the link")
            ui.onNodeWithContentDescription("Close New event").performClick()
            shown("Discard this draft?"); ui.onNodeWithText("Discard draft").performClick()
            shown("Meeting ABCDE-FGHJK")
        } }
    }
}
