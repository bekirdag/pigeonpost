package dev.pigeonpost.inbox

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.unit.dp
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.ui.ConversationHistory
import dev.pigeonpost.inbox.ui.LoadedMessageCount
import dev.pigeonpost.inbox.ui.MailboxDialog
import dev.pigeonpost.inbox.ui.PigeonpostTheme
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

class HistoryUiTest {
    @get:Rule val ui = createAndroidComposeRule<ComponentActivity>()
    private fun messages(count: Int) = (0 until count).map { ThreadMessage("m" + it, "Message " + it, false, it.toLong()) }

    private fun show(
        source: State<List<ThreadMessage>>,
        tall: State<Boolean> = mutableStateOf(false),
        target: State<String?> = mutableStateOf(null),
        request: State<Int> = mutableIntStateOf(0),
        compact: State<Boolean> = mutableStateOf(false),
    ) {
        ui.setContent {
            PigeonpostTheme {
                Column(Modifier.fillMaxSize()) {
                    ConversationHistory(source.value, Modifier.weight(1f), target.value, request.value) { message ->
                        Column(Modifier.fillMaxWidth().heightIn(min = 100.dp)) {
                            Text(message.body)
                            if (tall.value && message.id == source.value.last().id) Spacer(Modifier.height(1800.dp))
                            Text("End " + message.id)
                        }
                    }
                    Text("Composer", Modifier.fillMaxWidth().height(if (compact.value) 340.dp else 60.dp).testTag("composer"))
                }
            }
        }
    }

    private fun assertNewestBottom(id: String) {
        ui.waitForIdle()
        ui.onNodeWithText("End " + id).assertIsDisplayed()
        val history = ui.onNodeWithTag("conversation-history").fetchSemanticsNode().boundsInRoot
        val newest = ui.onNodeWithTag("message:" + id).fetchSemanticsNode().boundsInRoot
        val padding = 16 * ui.activity.resources.displayMetrics.density
        assertEquals("Newest message must sit at the bottom inset", history.bottom - padding, newest.bottom, 2f)
        ui.onNodeWithTag("conversation-history").assert(SemanticsMatcher.expectValue(LoadedMessageCount, 10))
    }

    @Test fun thousandMessagesStartAtTheEndOfAnOversizedNewestMessage() {
        show(mutableStateOf(messages(1000)), tall = mutableStateOf(true))
        assertNewestBottom("m999")
        ui.onNodeWithContentDescription("Latest messages").assertDoesNotExist()
    }

    @Test fun lateHeightGrowthAndComposerResizeKeepTheBottomAnchored() {
        val tall = mutableStateOf(false)
        val compact = mutableStateOf(false)
        show(mutableStateOf(messages(90)), tall, compact = compact)
        assertNewestBottom("m89")
        ui.runOnIdle { tall.value = true }
        assertNewestBottom("m89")
        ui.runOnIdle { compact.value = true }
        assertNewestBottom("m89")
        ui.runOnIdle { compact.value = false }
        assertNewestBottom("m89")
    }

    @Test fun olderPagingAndAnArrivalPreserveTheReadingPosition() {
        val source = mutableStateOf(messages(90))
        show(source)
        repeat(3) { ui.onNodeWithTag("conversation-history").performTouchInput { swipeDown() } }
        ui.waitForIdle()
        val list = ui.onNodeWithTag("conversation-history").fetchSemanticsNode()
        assertTrue(list.config[LoadedMessageCount] > 10)
        val rows = ui.onAllNodes(SemanticsMatcher("message row") { it.config.getOrNull(SemanticsProperties.TestTag)?.startsWith("message:") == true }).fetchSemanticsNodes()
        val anchor = rows.first { it.boundsInRoot.top >= list.boundsInRoot.top + 20 && it.boundsInRoot.bottom <= list.boundsInRoot.bottom - 20 && it.boundsInRoot.height > 0 }
        val tag = anchor.config[SemanticsProperties.TestTag]
        val top = anchor.boundsInRoot.top
        ui.runOnIdle { source.value = source.value + ThreadMessage("arrival", "Arrived while reading", false, 1000) }
        ui.waitForIdle()
        assertEquals(top, ui.onNodeWithTag(tag).fetchSemanticsNode().boundsInRoot.top, 2f)
        ui.onNodeWithText("End arrival").assertDoesNotExist()
        ui.onNodeWithContentDescription("Latest messages").performClick()
        assertNewestBottom("arrival")
    }

    @Test fun searchRevealsOldHistoryThenLatestReturnsToTen() {
        val target = mutableStateOf<String?>(null)
        show(mutableStateOf(messages(1000)), target = target)
        ui.runOnIdle { target.value = "m123" }
        ui.onNodeWithText("End m123").assertIsDisplayed()
        assertTrue(ui.onNodeWithTag("conversation-history").fetchSemanticsNode().config[LoadedMessageCount] > 10)
        ui.runOnIdle { target.value = null }
        ui.onNodeWithContentDescription("Latest messages").performClick()
        assertNewestBottom("m999")
    }

    @Test fun explicitComposerRequestAndOwnMessageReturnToNewest() {
        val source = mutableStateOf(messages(90))
        val request = mutableIntStateOf(0)
        show(source, request = request)
        repeat(2) { ui.onNodeWithTag("conversation-history").performTouchInput { swipeDown() } }
        ui.onNodeWithContentDescription("Latest messages").assertExists()
        ui.runOnIdle { request.intValue++ }
        assertNewestBottom("m89")
        ui.runOnIdle { source.value = source.value + ThreadMessage("own", "My sent message", true, 1000, status = Delivery.SENDING) }
        assertNewestBottom("own")
    }

    @Test fun restoredHistoryKeepsItsWindowAndReadingAnchorTogether() {
        val restoration = StateRestorationTester(ui)
        restoration.setContent {
            PigeonpostTheme {
                ConversationHistory(messages(1000), Modifier.fillMaxSize()) { message ->
                    Text(message.body, Modifier.fillMaxWidth().height(160.dp))
                }
            }
        }
        repeat(3) { ui.onNodeWithTag("conversation-history").performTouchInput { swipeDown() } }
        val list = ui.onNodeWithTag("conversation-history").fetchSemanticsNode()
        val count = list.config[LoadedMessageCount]
        assertTrue(count > 10)
        val anchor = ui.onAllNodes(SemanticsMatcher("message row") {
            it.config.getOrNull(SemanticsProperties.TestTag)?.startsWith("message:") == true
        }).fetchSemanticsNodes().first { it.boundsInRoot.top > list.boundsInRoot.top + 20 && it.boundsInRoot.height > 0 }
        val top = anchor.boundsInRoot.top
        val tag = anchor.config[SemanticsProperties.TestTag]
        restoration.emulateSavedInstanceStateRestore()
        ui.onNodeWithTag("conversation-history").assert(SemanticsMatcher.expectValue(LoadedMessageCount, count))
        assertEquals(top, ui.onNodeWithTag(tag).fetchSemanticsNode().boundsInRoot.top, 2f)
    }

    @Test fun mailboxSelectionDoesNotReorderTheMainRootAndPurchasedHandles() {
        val boxes = listOf(Mailbox("raw", null), Mailbox("studio", "/studio/main"), Mailbox("child", "/bekir/agent"), Mailbox("alp", "/alp"), Mailbox("main", "/bekir/main"))
        val state = mutableStateOf(InboxState(mailboxes = boxes, acting = boxes[1]))
        ui.setContent { PigeonpostTheme { MailboxDialog(state.value, { state.value = state.value.copy(acting = it) }, {}, "bekir") } }
        fun assertOrder() {
            val positions = listOf("main", "alp", "studio", "child", "raw").map { ui.onNodeWithTag("mailbox:" + it).fetchSemanticsNode().boundsInRoot.top }
            assertEquals(positions.sorted(), positions)
        }
        assertOrder()
        ui.onNodeWithTag("mailbox:studio").assertIsSelected()
        ui.onNodeWithTag("mailbox:alp").performClick()
        ui.onNodeWithTag("mailbox:alp").assertIsSelected()
        assertOrder()
    }
}
