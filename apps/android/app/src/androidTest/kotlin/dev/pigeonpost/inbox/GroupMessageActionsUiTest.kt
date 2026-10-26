package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.text.AnnotatedString
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Native controls run over the real MLS debug fixture, never a signed-in account. */
@RunWith(AndroidJUnit4::class)
class GroupMessageActionsUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private val seed = "The group release plan is ready."
    private fun tag(value: String) = ui.waitUntil(40_000) { ui.onAllNodesWithTag(value, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
    private fun back() = InstrumentationRegistry.getInstrumentation().sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_BACK)
    private fun scenario(mode: String = "group-actions", work: () -> Unit) {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", mode).putExtra("pigeonpost.appearance", "light")).use {
            try { work() } catch (failure: Throwable) {
                runCatching {
                    ui.onAllNodes(isRoot(), useUnmergedTree = true).fetchSemanticsNodes().indices.forEach { index ->
                        ui.onAllNodes(isRoot(), useUnmergedTree = true)[index].printToLog("PP_GROUP_ACTIONS", Int.MAX_VALUE)
                    }
                }
                runCatching {
                    val directory = java.io.File(context.getExternalFilesDir(null), "group-action-failures").also { it.mkdirs() }
                    InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot()?.let { picture ->
                        java.io.FileOutputStream(java.io.File(directory, "failure-" + System.currentTimeMillis() + ".png")).use { out ->
                            picture.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out)
                        }
                    }
                }
                throw failure
            }
        }
    }
    private fun open() {
        val row = hasText("Garden club") and SemanticsMatcher("group row") { it.config.getOrNull(SemanticsProperties.TestTag)?.startsWith("group:") == true }
        ui.waitUntil(40_000) { ui.onAllNodes(row).fetchSemanticsNodes().isNotEmpty() }
        ui.onNode(row).performClick()
        tag("groupComposer")
        ui.waitUntil(30_000) { ui.onAllNodes(hasText(seed) and hasAnyAncestor(hasTestTag("groupHistory"))).fetchSemanticsNodes().isNotEmpty() }
    }
    private fun message(text: String): SemanticsMatcher = SemanticsMatcher("confirmed group bubble") { node ->
        node.config.getOrNull(SemanticsProperties.TestTag)?.removePrefix("groupMessage:")?.toLongOrNull() != null
    } and (hasText(text) or hasAnyDescendant(hasText(text)))
    private fun sequence(text: String): String {
        ui.waitUntil(30_000) { ui.onAllNodes(message(text), useUnmergedTree = true).fetchSemanticsNodes().size == 1 }
        return ui.onNode(message(text), useUnmergedTree = true).fetchSemanticsNode().config[SemanticsProperties.TestTag].removePrefix("groupMessage:")
    }
    private fun send(text: String): String {
        ui.onNodeWithTag("groupComposer").performTextInput(text)
        ui.waitUntil(10_000) { SemanticsProperties.Disabled !in ui.onNodeWithContentDescription("Send to the group").fetchSemanticsNode().config }
        ui.onNodeWithContentDescription("Send to the group").performSemanticsAction(androidx.compose.ui.semantics.SemanticsActions.OnClick)
        val seq = sequence(text)
        ui.onNodeWithTag("groupComposer").assert(SemanticsMatcher.expectValue(SemanticsProperties.EditableText, AnnotatedString("")))
        return seq
    }

    @Test fun cancellingAnEditRestoresTheOrdinaryGroupDraft() = scenario {
        open(); val seq = send("Original group proposal")
        ui.onNodeWithTag("groupComposer").performTextInput("Ordinary group draft")
        ui.onNodeWithTag("groupEdit:$seq").performClick(); tag("groupComposerMode")
        ui.onNodeWithTag("groupComposer").assertTextContains("Original group proposal")
        ui.onNodeWithTag("groupComposer").performTextReplacement("Unsaved group edit")
        ui.onNodeWithContentDescription("Cancel edit").performClick()
        ui.onNodeWithTag("groupComposer").assertTextContains("Ordinary group draft")
        ui.onAllNodesWithTag("groupComposerMode").assertCountEquals(0)
    }

    @Test fun savingAnEditFoldsOneBubbleAndKeepsLocalVersions() = scenario {
        open(); val seq = send("Original group proposal")
        ui.onNodeWithTag("groupEdit:$seq").performClick(); tag("groupComposerMode")
        ui.onNodeWithTag("groupComposer").performTextReplacement("Updated group proposal")
        ui.onNodeWithContentDescription("Save group edit").performClick()
        val changed = sequence("Updated group proposal")
        ui.onAllNodes(hasText("Original group proposal") and hasAnyAncestor(hasTestTag("groupHistory"))).assertCountEquals(0)
        ui.onNode(hasContentDescription("Message actions") and hasAnyAncestor(hasTestTag("groupMessage:$changed"))).performClick()
        ui.onNodeWithText("Message info").performClick(); tag("groupHistory")
        ui.waitUntil(20_000) { ui.onAllNodesWithText("Versions").fetchSemanticsNodes().isNotEmpty() }
        ui.onNode(hasText("Original group proposal") and hasAnyAncestor(isDialog())).assertExists()
        ui.onNode(hasText("Updated group proposal") and hasAnyAncestor(isDialog())).assertExists()
    }

    @Test fun aReplyQuotesTheVerifiedGroupParent() = scenario {
        open(); val parent = sequence(seed)
        ui.onNodeWithTag("groupReply:$parent").performClick(); tag("groupComposerMode")
        ui.onNodeWithText("Replying to Bob").assertExists()
        send("My group answer")
        ui.onNodeWithTag("replyQuote:$parent").assertExists()
        ui.onAllNodesWithTag("groupComposerMode").assertCountEquals(0)
    }

    @Test fun aQuoteNavigatesToAnEarlierGroupMessage() = scenario {
        open(); val parent = sequence(seed)
        ui.onNodeWithTag("groupReply:$parent").performClick(); send("Reply with a parent")
        repeat(7) { send("Later group note $it") }
        back()
        ui.onNodeWithTag("groupHistory").performScrollToNode(hasTestTag("replyQuote:$parent"))
        ui.onNodeWithTag("replyQuote:$parent").performClick()
        ui.onNodeWithTag("groupMessage:$parent").assertIsDisplayed()
    }

    @Test fun groupSeenTicksAndMemberInfoUseTheReceiptPreference() = scenario {
        open(); val seq = send("Group receipt example")
        tag("groupReceipt:seen")
        ui.onNode(hasContentDescription("Message actions") and hasAnyAncestor(hasTestTag("groupMessage:$seq"))).performClick()
        ui.onNodeWithText("Message info").performClick()
        ui.waitUntil(20_000) { ui.onAllNodesWithText("Delivery").fetchSemanticsNodes().isNotEmpty() }
        ui.onNode(hasText("Bob") and hasAnyAncestor(isDialog())).assertExists()
        ui.onAllNodes(hasText("Seen") and hasAnyAncestor(isDialog())).assertCountEquals(2)
        back()
        if (ui.onAllNodesWithContentDescription("Back to conversations").fetchSemanticsNodes().isNotEmpty())
            ui.onNodeWithContentDescription("Back to conversations").performClick()
        ui.onNodeWithContentDescription("Settings").performClick()
        ui.onNodeWithText("Privacy & filtering").performClick(); tag("readReceiptsSwitch")
        ui.onNodeWithTag("readReceiptsSwitch").assertIsOn().performClick().assertIsOff()
        back(); open()
        ui.onAllNodesWithTag("groupReceipt:seen", useUnmergedTree = true).assertCountEquals(0)
        ui.onAllNodesWithTag("groupReceipt:delivered", useUnmergedTree = true).fetchSemanticsNodes().also { org.junit.Assert.assertTrue(it.isNotEmpty()) }
    }

    @Test fun anUnconfirmedGroupSendRetriesTheAcceptedMessageOnce() = scenario("group-actions-lost") {
        open(); ui.onNodeWithTag("groupComposer").performTextInput("One accepted group message")
        ui.onNodeWithContentDescription("Send to the group").performClick(); tag("groupUnconfirmedSend")
        ui.onNodeWithText("Retry", useUnmergedTree = true).performClick()
        sequence("One accepted group message")
        ui.waitUntil(20_000) { ui.onAllNodesWithTag("groupUnconfirmedSend").fetchSemanticsNodes().isEmpty() }
        ui.onNodeWithTag("groupComposer").assert(SemanticsMatcher.expectValue(SemanticsProperties.EditableText, AnnotatedString("")))
        ui.onAllNodes(hasText("One accepted group message") and hasAnyAncestor(hasTestTag("groupHistory"))).assertCountEquals(1)
    }

    @Test fun disabledCapabilitiesKeepCopyWithoutGroupActionsOrReceipts() = scenario("group-actions-off") {
        open(); val seq = send("A legacy group message")
        ui.onNodeWithTag("groupCopy:$seq").assertExists()
        ui.onAllNodesWithTag("groupEdit:$seq").assertCountEquals(0)
        ui.onAllNodesWithTag("groupReply:" + sequence(seed)).assertCountEquals(0)
        for (state in listOf("sent", "delivered", "seen")) ui.onAllNodesWithTag("groupReceipt:$state", useUnmergedTree = true).assertCountEquals(0)
    }

    @Test fun deletingAnEditedGroupMessageErasesEveryLocalVersion() = scenario {
        open(); val seq = send("Group text to delete")
        ui.onNodeWithTag("groupEdit:$seq").performClick()
        ui.onNodeWithTag("groupComposer").performTextReplacement("Edited text to delete")
        ui.onNodeWithContentDescription("Save group edit").performClick(); val changed = sequence("Edited text to delete")
        ui.onNode(hasContentDescription("Message actions") and hasAnyAncestor(hasTestTag("groupMessage:$changed"))).performClick()
        ui.onNodeWithText("Delete for everyone").performClick()
        ui.onNodeWithText("Delete", useUnmergedTree = true).performClick()
        ui.waitUntil(20_000) { ui.onAllNodes(hasText("Edited text to delete") and hasAnyAncestor(hasTestTag("groupHistory"))).fetchSemanticsNodes().isEmpty() }
        ui.onAllNodes(hasText("Group text to delete") and hasAnyAncestor(hasTestTag("groupHistory"))).assertCountEquals(0)
    }
}
