package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * Store marketing screenshots from the `marketing` fixture (the invented world of
 * marketing/store/seed/demo-world.json; no account, person or message is real). Skipped unless run with
 * `-Pandroid.testInstrumentationRunnerArguments.marketingShots=<folder>`, optionally `...scenes=inbox,group`.
 * Files land in the app's external files directory under marketing/<folder>; marketing/store/capture/android/capture.sh
 * pulls them.
 */
@RunWith(AndroidJUnit4::class)
class MarketingScreenshots {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private val arguments get() = InstrumentationRegistry.getArguments()
    private val folder = arguments.getString("marketingShots")
    private val wide get() = wideLayout()

    private fun launch(look: String, caps: String): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        val shell = InstrumentationRegistry.getInstrumentation().uiAutomation
        shell.executeShellCommand("cmd uimode night " + if (look == "dark") "yes" else "no").close()
        return ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "marketing")
            .putExtra("pigeonpost.appearance", look).putExtra("pigeonpost.marketing.caps", caps)).also { scenario ->
            ui.waitUntil(15_000) { var focused = false; scenario.onActivity { focused = it.hasWindowFocus() }; focused }
        }
    }
    private fun shown(text: String, ms: Long = 60_000) = ui.waitUntil(ms) { ui.onAllNodesWithText(text, substring = true, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
    private fun tagged(tag: String, ms: Long = 60_000) = ui.waitUntil(ms) { ui.onAllNodesWithTag(tag, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
    private fun open(text: String, exact: Boolean = false) {
        shown(text); ui.onAllNodes(hasText(text, substring = !exact) and hasClickAction(), useUnmergedTree = false)[0].performClick()
    }
    private fun shot(name: String, settle: Long = 2500) {
        ui.waitForIdle(); Thread.sleep(settle)
        val bitmap = InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot()
        val out = File(context.getExternalFilesDir(null), "marketing/$folder").apply { mkdirs() }
        File(out, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }
    private fun hideKeyboard(scenario: ActivityScenario<MainActivity>) = scenario.onActivity {
        it.currentFocus?.clearFocus()
        WindowCompat.getInsetsController(it.window, it.window.decorView).hide(WindowInsetsCompat.Type.ime())
    }

    private fun back() {
        InstrumentationRegistry.getInstrumentation().uiAutomation.performGlobalAction(android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK)
        ui.waitForIdle(); Thread.sleep(800)
    }

    private val actions = "message_edit,message_reply,receipts,agent_status"
    private fun wanted(scene: String) = arguments.getString("scenes")?.split(',')?.contains(scene) ?: true

    @Test fun capture() {
        assumeTrue(folder != null)
        if (wanted("inbox")) launch("light", "groups,disappearing,avatars").use {
            shown("Weekend hikers"); shown("Design team"); shown("Leo: Can"); shown("Priya: New")
            if (wide) { open("Leo", exact = true); shown("book a table") }
            shot("inbox")
        }
        if (wanted("agent-thread")) launch("dark", "avatars,$actions").use {
            open("Research agent"); shown("Venue shortlist"); tagged("receipt:working")
            shot("agent-thread")
        }
        if (wanted("group")) launch("light", "groups,disappearing,avatars").use { scenario ->
            open("Weekend hikers"); shown("ridge loop")
            tagged("groupComposer")
            ui.onNodeWithTag("groupComposer").performClick().performTextInput("Ridge loop it is. I'll drive — room for three 🚗")
            ui.waitUntil(10_000) { androidx.compose.ui.semantics.SemanticsProperties.Disabled !in ui.onNodeWithContentDescription("Send to the group").fetchSemanticsNode().config }
            ui.onNodeWithContentDescription("Send to the group").performSemanticsAction(androidx.compose.ui.semantics.SemanticsActions.OnClick)
            shown("Packed lunch")
            hideKeyboard(scenario)
            shot("group", 4000)
        }
        if (wanted("live")) launch("dark", "groups,disappearing,avatars,streaming").use {
            open("Design team"); shown("onboarding screens"); tagged("groupLiveBanner")
            shot("live")
        }
        if (wanted("meeting-notes")) launch("dark", "groups,meeting_summaries,avatars").use {
            shown("Priya: New")
            if (ui.onAllNodesWithTag("sidebar:meetingNotes").fetchSemanticsNodes().isNotEmpty()) ui.onNodeWithTag("sidebar:meetingNotes").performClick()
            else { ui.onAllNodesWithContentDescription("Settings")[0].performClick(); ui.onNodeWithText("Meeting notes").performScrollTo().performClick() }
            tagged("notes:row:mtg_launchsync0000000000000001")
            ui.onNodeWithTag("notes:row:mtg_launchsync0000000000000001").performClick()
            tagged("notes:brief")
            shot("meeting-notes")
        }
        if (wanted("calendar")) launch("light", "calendar,avatars").use {
            ui.waitUntil(15_000) { ui.onAllNodes(hasTestTag("tab:calendar") or hasTestTag("sidebar:calendar")).fetchSemanticsNodes().isNotEmpty() }
            ui.onNode(hasTestTag("tab:calendar") or hasTestTag("sidebar:calendar")).performClick()
            shown("Launch sync")
            if (wide) ui.onAllNodes(hasScrollAction() and hasAnyDescendant(hasText("Launch sync", substring = true)))[0]
                .performScrollToNode(hasText("Ramen with Leo", substring = true))
            shot("calendar")
        }
        if (wanted("teams")) launch("light", "groups,orgs,avatars").use {
            shown("Priya: New")
            if (ui.onAllNodesWithTag("sidebar:organizationRooms").fetchSemanticsNodes().isNotEmpty()) ui.onNodeWithTag("sidebar:organizationRooms").performClick()
            else { ui.onNodeWithContentDescription("New conversation").performClick(); ui.onNodeWithTag("menu:organizationRooms").performClick() }
            shown("Launch room"); shown("Support desk")
            shot("teams")
        }
    }
}
