@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package dev.pigeonpost.core

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException

/** A first message opens its conversation, wherever the server files it, and only in its own mailbox. */
class OpenThreadAfterSendTest {
    /** `/ns` is a namespace; the postbox delivers to its `/ns/main` mailbox and files the sent copy there. */
    private open class Resolving : FakePostbox() {
        var gate: CompletableDeferred<Unit>? = null
        override suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String>): SendResponse {
            sends++; gate?.await()
            rows = rows + Message("copy", body, from = identity, to = "/k/main", peer = "/k/main", peerHandle = if (to == "/ns") "/ns/main" else null,
                direction = "out", sentAt = 10)
            return SendResponse("message", "copy")
        }
    }

    @Test fun startingAConversationSelectsItAndAsksForComposerFocus() = runTest {
        val store = InboxStore(FakePostbox(), this); store.loadAccount("/a"); advanceUntilIdle()
        store.showArchive(true)
        store.startConversation("/ns/main")
        assertEquals("/ns/main", store.state.value.selectedPeer)
        assertFalse(store.state.value.viewingArchive)
        assertTrue(store.state.value.composerFocus)
        store.composerFocused(); assertFalse(store.state.value.composerFocus)
        store.reset()
    }

    @Test fun aFirstMessageToANamespaceFollowsTheSelectionToWhereItWasFiled() = runTest {
        val store = InboxStore(Resolving(), this); store.loadAccount("/a"); advanceUntilIdle()
        store.startConversation("/ns"); store.composerFocused(); store.draft("Hello namespace")
        store.send(); advanceUntilIdle()
        val state = store.state.value
        assertEquals("/ns/main", state.selectedPeer)
        assertEquals(listOf("copy"), state.conversation!!.messages.map { it.id })
        assertEquals("Hello namespace", state.subject!!.messages.single().display.text)
        assertTrue("the field is focused again in the followed conversation", state.composerFocus)
        assertTrue(state.pending.isEmpty())
        // Reopening the typed address later is a choice the user made; it is not followed again.
        store.selectPeer("/ns"); store.refresh(); advanceUntilIdle()
        assertEquals("/ns", store.state.value.selectedPeer)
        store.reset()
    }

    @Test fun anAddressFiledUnderItsOwnNameStaysSelected() = runTest {
        val store = InboxStore(Resolving(), this); store.loadAccount("/a"); advanceUntilIdle()
        store.startConversation("/k/main"); store.draft("Hello key"); store.send(); advanceUntilIdle()
        assertEquals("/k/main", store.state.value.selectedPeer)
        assertEquals(listOf("copy"), store.state.value.conversation!!.messages.map { it.id })
        store.reset()
    }

    @Test fun whileSendingTheSentMessageIsVisibleInTheNewConversation() = runTest {
        val api = Resolving().also { it.gate = CompletableDeferred() }
        val store = InboxStore(api, this); store.loadAccount("/a"); advanceUntilIdle()
        store.startConversation("/ns"); store.draft("Hello"); store.send(); runCurrent()
        assertEquals("/ns", store.state.value.selectedPeer)
        assertEquals(Delivery.SENDING, store.state.value.conversation!!.messages.single().status)
        api.gate!!.complete(Unit); advanceUntilIdle()
        assertEquals("/ns/main", store.state.value.selectedPeer)
        store.reset()
    }

    @Test fun anUncertainFirstSendKeepsTheNewConversationOpenWithItsMessage() = runTest {
        val api = object : FakePostbox() {
            override suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String>): SendResponse { sends++; throw IOException("lost response") }
        }
        val store = InboxStore(api, this); store.loadAccount("/a"); advanceUntilIdle()
        store.startConversation("/ns"); store.draft("Did it arrive?"); store.send(); advanceUntilIdle()
        val state = store.state.value
        assertEquals("/ns", state.selectedPeer)
        val shown = state.conversation!!.messages.single()
        assertEquals(Delivery.FAILED, shown.status)
        assertEquals("Did it arrive?", shown.display.text)
        assertEquals(1, api.sends)
        store.reset()
    }

    @Test fun anUnconfirmedProtectedFirstSendOffersRetryInTheNewConversationAndFollowsWithIt() = runTest {
        val api = object : FakePostbox() {
            val targets = mutableListOf<String>()
            val refs = mutableListOf<String>()
            val subjects = mutableListOf<String?>()
            override suspend fun capabilities() = Capabilities(Features.MESSAGE_ACTIONS.toList())
            override suspend fun inboxWithActions(identity: String, wait: Int?) = InboxResponse(if (identity == "/a") rows else emptyList())
            override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
                targets += to; refs += action.msgRef; subjects += threadId
                // Filed in a thread the client has not been told about yet.
                rows = listOf(Message("copy", body, from = identity, to = "/k/main", peer = "/k/main", peerHandle = "/ns/main", direction = "out", sentAt = 10, msgRef = action.msgRef, threadId = "server_thread"))
                if (targets.size == 1) throw IOException("response lost after commit")
                return SendResponse("message", "copy", action.msgRef, duplicate = true)
            }
        }
        val store = InboxStore(api, this); store.loadAccount("/a"); advanceUntilIdle()
        store.startConversation("/ns"); store.draft("Once only"); store.send(); advanceUntilIdle()
        val id = store.state.value.unconfirmedSend
        assertNotNull("Retry and Stop retrying are offered in the new conversation", id)
        assertEquals("/ns", store.state.value.selectedPeer)
        assertEquals(Delivery.FAILED, store.state.value.conversation!!.messages.single().status)
        // The committed copy arrives with the next load: the conversation, its Retry and the frozen send move together.
        store.refresh(); advanceUntilIdle()
        assertEquals("/ns/main", store.state.value.selectedPeer)
        assertEquals(id, store.state.value.unconfirmedSend)
        assertEquals("Once only", store.state.value.draft)
        assertEquals(listOf("copy"), store.state.value.conversation!!.messages.map { it.id })
        assertTrue(store.canRetrySend(id!!)); store.retrySend(id); advanceUntilIdle()
        assertEquals("the retry repeats the original request exactly", listOf("/ns", "/ns"), api.targets)
        assertEquals(1, api.refs.toSet().size)
        assertEquals(listOf<String?>(null, null), api.subjects)
        assertEquals("/ns/main", store.state.value.selectedPeer)
        assertNull(store.state.value.unconfirmedSend)
        assertEquals("", store.state.value.draft)
        assertEquals(listOf("copy"), store.state.value.conversation!!.messages.map { it.id })
        store.reset()
    }

    @Test fun anUnconfirmedSendIsNeverMovedIntoAnotherMailbox() = runTest {
        val api = object : FakePostbox() {
            override suspend fun capabilities() = Capabilities(Features.MESSAGE_ACTIONS.toList())
            override suspend fun inboxWithActions(identity: String, wait: Int?) = InboxResponse(rows)
            override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
                rows = listOf(Message("copy", body, from = identity, to = "/k/main", peer = "/k/main", peerHandle = "/ns/main", direction = "out", sentAt = 10, msgRef = action.msgRef))
                throw IOException("response lost after commit")
            }
        }
        val store = InboxStore(api, this); store.loadAccount("/a"); advanceUntilIdle()
        store.startConversation("/ns"); store.draft("Mine"); store.send(); advanceUntilIdle()
        val id = store.state.value.unconfirmedSend!!
        // The other mailbox's inbox happens to list a matching copy; nothing moves there.
        store.switchMailbox(store.state.value.mailboxes.first { it.address == "/b" }); advanceUntilIdle()
        store.selectPeer("/ns"); store.refresh(); advanceUntilIdle()
        assertEquals("/ns", store.state.value.selectedPeer)
        assertNull(store.state.value.unconfirmedSend)
        assertFalse(store.canRetrySend(id))
        // Back in its own mailbox, the Retry is found with the conversation the copy was filed under.
        store.switchMailbox(store.state.value.mailboxes.first { it.address == "/a" }); advanceUntilIdle()
        assertNull(store.state.value.conversations.firstOrNull { it.peer == "/ns" })
        store.selectPeer("/ns/main")
        assertEquals(id, store.state.value.unconfirmedSend)
        assertTrue(store.canRetrySend(id))
        store.reset()
    }

    @Test fun aSendFinishingAfterAMailboxSwitchNeverOpensAConversationInTheOtherMailbox() = runTest {
        val api = Resolving().also { it.gate = CompletableDeferred() }
        val store = InboxStore(api, this); store.loadAccount("/a"); advanceUntilIdle()
        store.startConversation("/ns"); store.draft("Hello"); store.send(); runCurrent()
        store.switchMailbox(store.state.value.mailboxes.first { it.address == "/b" }); runCurrent()
        api.gate!!.complete(Unit); advanceUntilIdle()
        assertEquals("/b", store.state.value.acting!!.address)
        assertNull(store.state.value.selectedPeer)
        store.switchMailbox(store.state.value.mailboxes.first { it.address == "/a" }); advanceUntilIdle()
        assertNull(store.state.value.selectedPeer)
        store.reset()
    }

    @Test fun aSendFinishingAfterTheUserMovedOnDoesNotPullThemBack() = runTest {
        val api = Resolving().also { it.gate = CompletableDeferred() }
        val store = InboxStore(api, this); store.loadAccount("/a"); advanceUntilIdle()
        store.startConversation("/ns"); store.draft("Hello"); store.send(); runCurrent()
        store.selectPeer("/someone-else")
        api.gate!!.complete(Unit); advanceUntilIdle()
        assertEquals("/someone-else", store.state.value.selectedPeer)
        store.reset()
    }
}
