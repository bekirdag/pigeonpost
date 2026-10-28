package dev.pigeonpost.inbox

import android.app.Application
import android.content.Context
import android.content.Intent
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.inbox.ui.PigeonpostApp
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Today's web/iOS parity: theme choice, composer focus, partial selection and the in-app viewer. */
@RunWith(AndroidJUnit4::class)
class ParityUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private fun launch(mode: String = "inbox"): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        return ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", mode)).also { scenario ->
            ui.waitUntil(10_000) { var focused = false; scenario.onActivity { focused = it.hasWindowFocus() }; focused }
        }
    }
    private fun shown(text: String) = ui.waitUntil(10_000) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() }

    @Test fun appearanceChoiceIsKeptAcrossRecreation() {
        val looks = context.getSharedPreferences("appearance", Context.MODE_PRIVATE)
        looks.edit().clear().commit()
        try {
            launch().use { scenario ->
                shown("Inbox")
                ui.onNodeWithContentDescription("Settings").performClick()
                ui.onNodeWithText("System").performScrollTo().assertIsSelected()
                ui.onNodeWithText("Dark").performScrollTo().performClick()
                ui.onNodeWithText("Dark").assertIsSelected()
                assertEquals("dark", looks.getString("choice", null))
                scenario.recreate()
                shown("Settings")
                ui.onNodeWithText("Dark").performScrollTo().assertIsSelected()
                ui.onNodeWithText("Light").performScrollTo().performClick()
                assertEquals("light", looks.getString("choice", null))
            }
        } finally { looks.edit().clear().commit() }
    }

    @Test fun composerKeepsFocusAfterSend() {
        launch().use {
            shown("Inbox")
            ui.onNodeWithText("Demo Builder").performClick()
            shown("The Android build is ready for review.")
            ui.onNodeWithText("Message").performClick().performTextInput("First of two")
            ui.onNode(hasSetTextAction()).assertIsFocused()
            ui.onNodeWithContentDescription("Send message").performClick()
            shown("First of two")
            ui.waitUntil(10_000) { ui.onAllNodesWithText("Sending…").fetchSemanticsNodes().isEmpty() }
            ui.onNode(hasSetTextAction()).assertIsFocused()
            ui.onNode(hasSetTextAction()).performTextInput("Second without tapping")
            ui.onNodeWithText("Second without tapping").assertExists()
        }
    }

    @Test fun longPressOnTextSelectsAndTheMenuStaysOnItsButton() {
        launch().use {
            shown("Inbox")
            ui.onNodeWithText("Demo Builder").performClick()
            shown("The Android build is ready for review.")
            // The bubble in the history, not a tablet's preview of the same message in the conversations column.
            ui.onNode(hasText("The Android build is ready for review.") and hasAnyAncestor(hasTestTag("conversation-history"))).performTouchInput { longClick(centerLeft + androidx.compose.ui.geometry.Offset(40f, 0f)) }
            ui.waitForIdle()
            ui.onNodeWithText("Original message").assertDoesNotExist()
            ui.onAllNodesWithContentDescription("Message actions").onLast().performClick()
            ui.onNodeWithText("Original message").assertIsDisplayed()
        }
    }

    @Test fun textAttachmentOpensInTheAppWithADownloadButton() {
        launch().use {
            shown("Inbox")
            ui.onNodeWithText("Docdex").performClick()
            shown("build-notes.txt")
            ui.onAllNodes(hasClickLabel("View build-notes.txt")).onFirst().performClick()
            ui.waitUntil(10_000) { ui.onAllNodesWithText("Pigeonpost Android development build").fetchSemanticsNodes().isNotEmpty() }
            ui.onNode(hasContentDescription("Download build-notes.txt") and hasAnyAncestor(isDialog())).assertExists()
            ui.onNodeWithContentDescription("Close viewer").performClick()
            ui.onNodeWithTag("attachmentViewer").assertDoesNotExist()
        }
    }

    private fun hasClickLabel(label: String) = SemanticsMatcher("click label $label") { node ->
        node.config.getOrElseNullable(androidx.compose.ui.semantics.SemanticsActions.OnClick) { null }?.label == label
    }
}

/** The iPad/desktop three columns on a wide window, and the way back after hiding conversations. */
@RunWith(AndroidJUnit4::class)
class TabletLayoutTest {
    @get:Rule val ui = createAndroidComposeRule<ComponentActivity>()

    private fun model(): InboxViewModel {
        val app = ApplicationProvider.getApplicationContext<Application>()
        app.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        return InboxViewModel(app, Development.graph(app, Intent().putExtra("pigeonpost.fixtures", "inbox"))!!)
    }

    @OptIn(ExperimentalTestApi::class)
    @Test fun conversationsColumnHidesAndComesBack() {
        val model = model()
        ui.setContent {
            DeviceConfigurationOverride(DeviceConfigurationOverride.ForcedSize(DpSize(1280.dp, 800.dp))) {
                PigeonpostApp(model, { _, _ -> }, {}, {}, {}, { _, _ -> }, {})
            }
        }
        ui.waitUntil(10_000) { ui.onAllNodesWithText("Demo Builder").fetchSemanticsNodes().isNotEmpty() }
        // Nothing open yet: the conversations column, the mailbox switcher and an empty thread pane.
        ui.onNodeWithTag("mailboxSwitcher").assertExists()
        ui.onNodeWithText("Choose a conversation or start a new one.").assertExists()
        ui.onNodeWithText("Demo Builder").performClick()
        ui.waitUntil(10_000) { ui.onAllNodesWithText("THREADS").fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithTag("subject:Android build").assertExists()
        ui.onNodeWithContentDescription("Voice call").assertExists()
        // Hide, then the thread's own bar brings the column back.
        ui.onNodeWithContentDescription("Hide conversations").performClick()
        ui.waitUntil(5_000) { ui.onAllNodesWithTag("mailboxSwitcher").fetchSemanticsNodes().isEmpty() }
        ui.onNodeWithText("THREADS").assertDoesNotExist()
        ui.onNodeWithContentDescription("Show conversations").assertIsDisplayed().performClick()
        ui.waitUntil(5_000) { ui.onAllNodesWithTag("mailboxSwitcher").fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithContentDescription("Hide conversations").assertIsDisplayed()
        ui.onNodeWithText("THREADS").assertExists()
        // Hidden with no conversation open still leaves a way back.
        ui.onNodeWithContentDescription("Hide conversations").performClick()
        ui.waitUntil(5_000) { ui.onAllNodesWithTag("mailboxSwitcher").fetchSemanticsNodes().isEmpty() }
        ui.runOnIdle { model.inbox.selectPeer(null) }
        ui.onNodeWithContentDescription("Show conversations").assertIsDisplayed().performClick()
        ui.waitUntil(5_000) { ui.onAllNodesWithTag("mailboxSwitcher").fetchSemanticsNodes().isNotEmpty() }
    }

    @OptIn(ExperimentalTestApi::class)
    @Test fun mailboxSwitcherListsInboxesInsideTheColumn() {
        val model = model()
        ui.setContent {
            DeviceConfigurationOverride(DeviceConfigurationOverride.ForcedSize(DpSize(1280.dp, 800.dp))) {
                PigeonpostApp(model, { _, _ -> }, {}, {}, {}, { _, _ -> }, {})
            }
        }
        ui.waitUntil(10_000) { ui.onAllNodesWithTag("mailboxSwitcher").fetchSemanticsNodes().isNotEmpty() }
        ui.onNodeWithTag("mailboxSwitcher").performClick()
        ui.onNodeWithTag("mailbox:/k/demo-agent").performClick()
        ui.waitUntil(10_000) { ui.onAllNodesWithText("No conversations yet").fetchSemanticsNodes().isNotEmpty() }
    }
}
