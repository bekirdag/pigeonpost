@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package dev.pigeonpost.core

import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test
import java.io.File
import java.io.IOException
import java.io.OutputStream

class InboxStoreTest {
    @Test fun aSlowPreviousMailboxCannotReplaceTheNewInbox() = runTest {
        val delayed = CompletableDeferred<InboxResponse>()
        val api = object : FakePostbox() {
            override suspend fun inbox(identity: String, wait: Int?) = if (identity == "/a") withContext(NonCancellable) { delayed.await() } else InboxResponse(listOf(Message("b_message", peer = "/peer-b")))
        }
        val store = InboxStore(api, this)
        store.loadAccount("/a"); runCurrent()
        store.switchMailbox(store.state.value.mailboxes.first { it.address == "/b" }); runCurrent()
        assertEquals("b_message", store.state.value.messages.single().id)
        delayed.complete(InboxResponse(listOf(Message("old", peer = "/peer-a")))); advanceUntilIdle()
        assertEquals("/b", store.state.value.acting!!.address)
        assertEquals("b_message", store.state.value.messages.single().id)
        store.reset()
    }
    @Test fun draftsAreIsolatedByMailboxPeerAndSubject() = runTest {
        val store = InboxStore(FakePostbox(), this)
        store.loadAccount("/a"); advanceUntilIdle()
        store.selectPeer("/peer-a"); store.draft("A draft")
        store.selectPeer("/peer-b"); assertEquals("", store.state.value.draft); store.draft("Other peer")
        store.selectPeer("/peer-a"); assertEquals("A draft", store.state.value.draft)
        store.openThread("Planning"); advanceUntilIdle(); store.draft("Subject draft")
        store.selectSubject(""); assertEquals("A draft", store.state.value.draft)
        store.switchMailbox(store.state.value.mailboxes.last()); advanceUntilIdle(); store.selectPeer("/peer-a")
        assertEquals("", store.state.value.draft)
        store.switchMailbox(store.state.value.mailboxes.first()); advanceUntilIdle(); store.selectPeer("/peer-a")
        assertEquals("A draft", store.state.value.draft)
        store.reset()
    }
    @Test fun acknowledgeOnlyTheVisibleSubjectAndRollbackUnattemptedMarks() = runTest {
        val api = object : FakePostbox() {
            override suspend fun ack(identity: String, messageId: String) { acked += identity to messageId; if (messageId == "two") throw IOException("offline") }
        }
        api.rows = listOf(Message("one", peer = "/peer", threadId = "visible", read = false, receivedAt = 1), Message("two", peer = "/peer", threadId = "visible", read = false, receivedAt = 2), Message("three", peer = "/peer", threadId = "visible", read = false, receivedAt = 3), Message("hidden", peer = "/peer", threadId = "hidden", read = false))
        val store = InboxStore(api, this); store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer"); store.selectSubject("visible")
        store.acknowledgeVisible(); advanceUntilIdle()
        assertEquals(listOf("/a" to "one", "/a" to "two"), api.acked)
        assertEquals(setOf("one"), store.state.value.messages.filter { it.read == true }.map { it.id }.toSet())
        store.reset()
    }
    @Test fun acknowledgementKeepsCapturedIdentityAfterSwitch() = runTest {
        val wait = CompletableDeferred<Unit>()
        val api = object : FakePostbox() {
            override suspend fun ack(identity: String, messageId: String) { wait.await(); acked += identity to messageId }
        }
        api.rows = listOf(Message("one", peer = "/peer", read = false))
        val store = InboxStore(api, this); store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer")
        store.acknowledgeVisible(); runCurrent(); store.switchMailbox(store.state.value.mailboxes.last()); runCurrent()
        wait.complete(Unit); advanceUntilIdle()
        assertEquals(listOf("/a" to "one"), api.acked)
        assertFalse(store.state.value.messages.single().read == true)
        store.reset()
    }
    @Test fun anUncertainSendPreservesDraftAndDoesNotRetry() = runTest {
        val api = object : FakePostbox() {
            override suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String>): SendResponse { sends++; throw IOException("lost response") }
        }
        val store = InboxStore(api, this); store.loadAccount(); advanceUntilIdle(); store.selectPeer("/peer"); store.draft("Keep this")
        store.send(); advanceUntilIdle()
        assertEquals(1, api.sends)
        assertEquals("Keep this", store.state.value.draft)
        assertEquals(Delivery.FAILED, store.state.value.pending.single().status)
        assertTrue(store.state.value.error!!.contains("before sending again"))
        assertFalse(store.state.value.isSending)
        store.reset()
    }
    @Test fun sendIsSingleFlightAndReconcilesWithTheServerCopy() = runTest {
        val wait = CompletableDeferred<Unit>()
        val api = object : FakePostbox() {
            override suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String>): SendResponse {
                sends++; wait.await(); rows = listOf(Message("copy", body, peer = to, direction = "out")); return SendResponse("recipient", "copy")
            }
        }
        val store = InboxStore(api, this); store.loadAccount(); advanceUntilIdle(); store.selectPeer("/peer"); store.draft("Hello")
        store.send(); store.send(); runCurrent(); assertEquals(1, api.sends)
        wait.complete(Unit); advanceUntilIdle()
        assertEquals("", store.state.value.draft)
        assertTrue(store.state.value.pending.isEmpty())
        assertEquals(listOf("copy"), store.state.value.conversation!!.messages.map { it.id })
        store.reset()
    }
    @Test fun signOutRejectsLateMutationCallbacksEvenIfTransportIgnoresCancellation() = runTest {
        val wait = CompletableDeferred<Unit>()
        val api = object : FakePostbox() {
            override suspend fun saveContact(identity: String, contact: Contact) { withContext(NonCancellable) { wait.await() } }
        }
        var callback = false
        val store = InboxStore(api, this); store.loadAccount(); advanceUntilIdle()
        store.saveContact(Contact("/peer")) { callback = true }; runCurrent()
        store.reset(); wait.complete(Unit); advanceUntilIdle()
        assertFalse(callback); assertEquals(InboxState(), store.state.value)
    }
    @Test fun fullPermissionsNeverIncludeForbiddenVerbsAndKnownPreservesReview() = runTest {
        val api = FakePostbox(); val store = InboxStore(api, this)
        store.loadAccount(); advanceUntilIdle(); store.markKnown("/peer"); advanceUntilIdle()
        assertEquals("review", api.saved.last().autonomy)
        assertTrue(api.saved.last().allowedVerbs.orEmpty().isEmpty())
        store.fullPermissions("/peer", true); advanceUntilIdle()
        assertEquals(listOf("run_tests"), api.saved.last().allowedVerbs)
        store.block("/peer"); advanceUntilIdle()
        assertEquals("review", api.saved.last().autonomy); assertEquals(emptyList<String>(), api.saved.last().allowedVerbs)
        store.reset()
    }
    @Test fun backgroundStopsPollingAndErrorsBackOffWithoutDiscardingHistory() = runTest {
        var polls = 0
        val api = object : FakePostbox() { override suspend fun inbox(identity: String, wait: Int?): InboxResponse {
            if (wait != null) { polls++; throw IOException("offline") }; return InboxResponse(rows)
        } }
        api.rows = listOf(Message("a", peer = "/peer"))
        val store = InboxStore(api, this); store.loadAccount(); advanceUntilIdle(); store.setActive(true); runCurrent()
        assertEquals(1, polls); assertTrue(store.state.value.offline); assertEquals(1, store.state.value.messages.size)
        advanceTimeBy(999); runCurrent(); assertEquals(1, polls)
        advanceTimeBy(1); runCurrent(); assertEquals(2, polls)
        store.setActive(false); advanceTimeBy(60000); runCurrent(); assertEquals(2, polls)
        store.reset()
    }
}

private open class FakePostbox : PostboxApi {
    var rows = emptyList<Message>()
    var sends = 0
    val acked = mutableListOf<Pair<String, String>>()
    val saved = mutableListOf<Contact>()
    override suspend fun identities() = listOf(IdentityRow("/a"), IdentityRow("/b"))
    override suspend fun whoami(identity: String) = WhoAmI(identity, if (identity == "/a") "/demo/main" else "/demo/agent")
    override suspend fun createIdentity(handle: String?) = "/a"
    override suspend fun inbox(identity: String, wait: Int?) = InboxResponse(rows)
    override suspend fun threads(identity: String) = emptyList<ServerThread>()
    override suspend fun contacts(identity: String) = ContactsResponse(emptyList(), Vocabulary(listOf("run_tests", "run_shell"), listOf("run_shell")))
    override suspend fun archive(identity: String) = emptySet<String>()
    override suspend fun quota(identity: String) = Quota(0, 1024, 900, "free")
    override suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String>): SendResponse { sends++; return SendResponse("recipient", "copy") }
    override suspend fun ack(identity: String, messageId: String) { acked += identity to messageId }
    override suspend fun openThread(identity: String, peer: String, title: String) = "created"
    override suspend fun deleteThread(identity: String, id: String) {}
    override suspend fun deleteMessage(identity: String, id: String) {}
    override suspend fun reportSpam(identity: String, id: String) {}
    override suspend fun setArchived(identity: String, peer: String, archived: Boolean) {}
    override suspend fun saveContact(identity: String, contact: Contact) { saved += contact }
    override suspend fun removeContact(identity: String, peer: String) {}
    override suspend fun upload(identity: String, file: File, filename: String, mediaType: String) = Attachment("attachment", filename, mediaType, file.length())
    override suspend fun download(identity: String, id: String, output: OutputStream, maximumBytes: Long) {}
    override suspend fun handleOffer() = HandleOffer()
}
