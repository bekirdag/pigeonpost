package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.text.AnnotatedString
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Native message controls against explicit, isolated debug transports. No production account. */
@RunWith(AndroidJUnit4::class)
class MessageActionsUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private fun back() = InstrumentationRegistry.getInstrumentation().sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_BACK)
    private fun tag(value: String) {
        ui.waitUntil(30_000) { ui.onAllNodesWithTag(value, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
    }
    private fun launch(mode: String = "actions"): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        val scene = ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java)
            .putExtra("pigeonpost.fixtures", mode).putExtra("pigeonpost.appearance", "light"))
        tag("conversation:/demo/assistant")
        return scene
    }
    private fun open() {
        ui.onNodeWithTag("conversation:/demo/assistant").performClick()
        tag("conversation-history")
    }
    private fun runCase(mode: String = "actions", work: () -> Unit) {
        launch(mode).use {
            try { work() } catch (failure: Throwable) {
                runCatching {
                    ui.onAllNodes(isRoot(), useUnmergedTree = true).fetchSemanticsNodes().indices.forEach { index ->
                        ui.onAllNodes(isRoot(), useUnmergedTree = true)[index].printToLog("PP_ACTIONS", Int.MAX_VALUE)
                    }
                }
                runCatching {
                    val directory = java.io.File(context.getExternalFilesDir(null), "message-action-failures").also { it.mkdirs() }
                    val picture = InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot()
                    if (picture != null) java.io.FileOutputStream(java.io.File(directory, "failure-" + System.currentTimeMillis() + ".png")).use { out ->
                        picture.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out)
                    }
                }
                throw failure
            }
        }
    }
    private fun scrollMessage(id: String) {
        ui.onNodeWithTag("conversation-history").performScrollToNode(hasTestTag("message:" + id))
    }
    // Phone chips identify the subject by ID; the existing tablet list uses its display title.
    private fun subject(id: String, title: String) = ui.onNode(hasTestTag("subject:$id") or hasTestTag("subject:$title"))

    @Test fun editingCancelsBackToTheOrdinaryDraft() {
        runCase {
            open()
            ui.onNodeWithTag("messageEditor").performTextInput("Ordinary draft")
            scrollMessage("actions_failed")
            ui.onNodeWithTag("edit:actions_failed").performClick()
            tag("composerMode")
            ui.onNodeWithText("Editing message").assertExists()
            ui.onNodeWithTag("messageEditor").assertTextContains("Please check the attached proposal.")
            ui.onNodeWithTag("messageEditor").performTextReplacement("Unsaved edit")
            ui.onNodeWithContentDescription("Cancel edit").performClick()
            ui.onNodeWithTag("messageEditor").assertTextContains("Ordinary draft")
            ui.onAllNodesWithTag("composerMode").assertCountEquals(0)
        }
    }

    @Test fun savingAnEditKeepsOneBubbleAndShowsItsVersions() {
        runCase {
            open(); scrollMessage("actions_failed")
            ui.onNodeWithTag("edit:actions_failed").performClick(); tag("composerMode")
            ui.onNodeWithTag("messageEditor").performTextReplacement("Updated proposal")
            ui.onNodeWithContentDescription("Save edit").performClick()
            ui.waitUntil(20_000) { ui.onAllNodesWithTag("composerMode").fetchSemanticsNodes().isEmpty() &&
                ui.onAllNodes(hasText("Updated proposal") and hasAnyAncestor(hasTestTag("conversation-history"))).fetchSemanticsNodes().size == 1 }
            scrollMessage("actions_failed")
            ui.onNodeWithTag("message:actions_failed").assertExists()
            ui.onNode(hasContentDescription("Message actions") and hasAnyAncestor(hasTestTag("message:actions_failed"))).performClick()
            ui.waitUntil(10_000) { ui.onAllNodesWithText("Message info").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithText("Message info").performClick()
            ui.waitUntil(15_000) { ui.onAllNodesWithText("Versions").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithText("Original", substring = true).assertExists()
            ui.onNodeWithText("Edit 1", substring = true).assertExists()
            ui.onNodeWithText("Please check the attached proposal.").assertExists()
            ui.onNode(hasText("Updated proposal") and hasAnyAncestor(isDialog())).assertExists()
        }
    }

    @Test fun replyingShowsTheContextAndSendsTheQuotedAnswer() {
        runCase {
            open()
            subject("actions_earlier", "Earlier discussion").performClick()
            tag("reply:actions_parent"); ui.onNodeWithTag("reply:actions_parent").performClick()
            tag("composerMode"); ui.onNodeWithText("Replying to Assistant").assertExists()
            ui.onNodeWithTag("messageEditor").performTextInput("Answer from the reply")
            ui.onNodeWithContentDescription("Send message").performClick()
            ui.waitUntil(20_000) { ui.onAllNodesWithTag("composerMode").fetchSemanticsNodes().isEmpty() }
            ui.onNode(hasText("Answer from the reply") and hasAnyAncestor(hasTestTag("conversation-history"))).assertExists()
            ui.onNodeWithTag("replyQuote:actions_parent").assertExists()
        }
    }

    @Test fun aQuoteNavigatesToTheParentInAnotherSubject() {
        runCase {
            open(); scrollMessage("actions_seen")
            ui.onNode(hasTestTag("replyQuote:actions_parent") and hasAnyAncestor(hasTestTag("message:actions_seen"))).performClick()
            subject("actions_earlier", "Earlier discussion").assertIsSelected()
            tag("message:actions_parent")
            ui.onNodeWithText("The release plan is ready.", substring = true).assertExists()
        }
    }

    @Test fun sentDeliveredSeenAndAgentStatesHaveDistinctAccessibleMarks() {
        runCase {
            open()
            scrollMessage("actions_failed"); ui.onNodeWithTag("receipt:agent_failed", useUnmergedTree = true).assertContentDescriptionEquals("Agent run failed")
            scrollMessage("actions_working"); ui.onNodeWithTag("receipt:working", useUnmergedTree = true).assertContentDescriptionEquals("Agent working on this")
            scrollMessage("actions_seen"); ui.onNodeWithTag("receipt:seen", useUnmergedTree = true).assertExists()
            scrollMessage("actions_delivered"); ui.onNodeWithTag("receipt:delivered", useUnmergedTree = true).assertContentDescriptionEquals("Delivered")
            scrollMessage("actions_sent"); ui.onNodeWithTag("receipt:sent", useUnmergedTree = true).assertContentDescriptionEquals("Sent")
        }
    }

    @Test fun readReceiptsCanBeDisabledEvenWithoutOtherPrivacyFeatures() {
        runCase {
            ui.onNodeWithContentDescription("Settings").performClick()
            ui.onNodeWithText("Privacy & filtering").performClick()
            tag("readReceiptsSwitch")
            ui.onNodeWithTag("readReceiptsSwitch").assertIsOn().performClick().assertIsOff()
            back()
            tag("conversation:/demo/assistant"); open(); scrollMessage("actions_seen")
            ui.onAllNodesWithTag("receipt:seen", useUnmergedTree = true).assertCountEquals(0)
            ui.onNode(hasTestTag("receipt:delivered") and hasAnyAncestor(hasTestTag("message:actions_seen")), useUnmergedTree = true).assertExists()
        }
    }

    @Test fun anUnconfirmedCommittedSendRetriesWithoutAnotherBubble() {
        runCase("actions-lost") {
            open()
            ui.onNodeWithTag("messageEditor").performTextInput("One committed message")
            ui.onNodeWithContentDescription("Send message").performClick()
            tag("unconfirmedSend")
            ui.onNodeWithText("Retry", useUnmergedTree = true).performClick()
            ui.waitUntil(20_000) { ui.onAllNodesWithTag("unconfirmedSend").fetchSemanticsNodes().isEmpty() }
            ui.onNodeWithTag("messageEditor").assert(SemanticsMatcher.expectValue(SemanticsProperties.EditableText, AnnotatedString("")))
            ui.onAllNodes(hasText("One committed message") and hasAnyAncestor(hasTestTag("conversation-history"))).assertCountEquals(1)
        }
    }

    private fun startConversation(address: String, words: String) {
        ui.onNodeWithContentDescription("New conversation").performClick()
        ui.onNodeWithTag("newConversationAddress").performTextReplacement(address)
        ui.onNodeWithText("Open conversation").performClick()
        tag("messageEditor")
        // The new conversation is open at once, with the message field ready.
        ui.waitUntil(10_000) { runCatching { ui.onNodeWithTag("messageEditor").assertIsFocused() }.isSuccess }
        ui.onNodeWithTag("messageEditor").performTextInput(words)
        ui.onNodeWithContentDescription("Send message").performClick()
    }

    @Test fun aFirstMessageOpensTheConversationTheServerFiledItUnder() {
        runCase {
            startConversation("/demo", "Hello namespace")
            // `/demo` resolved to `/demo/main`: the selection follows it there, showing the message.
            ui.waitUntil(20_000) { ui.onAllNodes(hasText("/demo/main") and hasAnyAncestor(hasTestTag("conversation:/demo/main")).not()).fetchSemanticsNodes().isNotEmpty() }
            ui.waitUntil(20_000) { ui.onAllNodes(hasText("Hello namespace") and hasAnyAncestor(hasTestTag("conversation-history"))).fetchSemanticsNodes().size == 1 }
            ui.onAllNodesWithText("Start a conversation").assertCountEquals(0)
            ui.onAllNodesWithText("Choose a conversation or start a new one.").assertCountEquals(0)
            ui.waitUntil(10_000) { runCatching { ui.onNodeWithTag("messageEditor").assertIsFocused() }.isSuccess }
            ui.onNodeWithTag("messageEditor").assert(SemanticsMatcher.expectValue(SemanticsProperties.EditableText, AnnotatedString("")))
        }
    }

    @Test fun anUnconfirmedFirstMessageStaysOpenWithRetryAndThenFollows() {
        runCase("actions-lost") {
            startConversation("/demo", "Was this delivered")
            tag("unconfirmedSend")
            // The pending message is shown in the new conversation while Retry is offered.
            ui.waitUntil(20_000) { ui.onAllNodes(hasText("Was this delivered") and hasAnyAncestor(hasTestTag("conversation-history"))).fetchSemanticsNodes().size == 1 }
            ui.onNodeWithText("Retry", useUnmergedTree = true).performClick()
            ui.waitUntil(20_000) { ui.onAllNodesWithTag("unconfirmedSend").fetchSemanticsNodes().isEmpty() }
            ui.waitUntil(20_000) { ui.onAllNodes(hasText("/demo/main") and hasAnyAncestor(hasTestTag("conversation:/demo/main")).not()).fetchSemanticsNodes().isNotEmpty() }
            ui.onAllNodes(hasText("Was this delivered") and hasAnyAncestor(hasTestTag("conversation-history"))).assertCountEquals(1)
        }
    }

    @Test fun disabledCapabilitiesKeepCopyWithoutReplyEditOrReceipts() {
        runCase("actions-off") {
            open(); scrollMessage("actions_failed")
            ui.onAllNodesWithTag("edit:actions_failed").assertCountEquals(0)
            ui.onAllNodesWithTag("receipt:agent_failed", useUnmergedTree = true).assertCountEquals(0)
            ui.onNode(hasContentDescription("Copy message") and hasAnyAncestor(hasTestTag("message:actions_failed"))).assertExists()
            subject("actions_earlier", "Earlier discussion").performClick()
            ui.onAllNodesWithTag("reply:actions_parent").assertCountEquals(0)
        }
    }
}
