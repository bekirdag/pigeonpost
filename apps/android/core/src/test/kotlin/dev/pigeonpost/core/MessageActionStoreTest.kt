@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package dev.pigeonpost.core

import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.*
import org.junit.Test
import java.io.File
import java.io.IOException

private const val ACTION_NOW = 100_000L
private const val ORIGINAL_REF = "mref_abcdefghijklmnopqrstuvwxyz"
private fun actionMessage(outgoing: Boolean = true, body: String = "Original") = Message(
    "root", body, from = if (outgoing) "/a" else "/peer", peer = "/peer", direction = if (outgoing) "out" else "in",
    sentAt = ACTION_NOW - 20, receivedAt = ACTION_NOW - 20, read = false, msgRef = ORIGINAL_REF)

private data class SendAttempt(val identity: String, val peer: String, val body: String, val thread: String?, val action: MessageSendAction, val attachments: List<String>)
private open class ActionPostbox : FakePostbox() {
    var enabled = Features.MESSAGE_ACTIONS.toList()
    var foldedReads = 0
    var legacyReads = 0
    var foldedAcks = 0
    var uploads = 0
    var threadRows = emptyList<ServerThread>()
    val attempts = mutableListOf<SendAttempt>()
    override suspend fun capabilities() = Capabilities(enabled)
    override suspend fun inbox(identity: String, wait: Int?): InboxResponse { legacyReads++; return super.inbox(identity, wait) }
    override suspend fun inboxWithActions(identity: String, wait: Int?): InboxResponse { foldedReads++; return InboxResponse(if (identity == "/a") rows else emptyList()) }
    override suspend fun threads(identity: String) = if (identity == "/a") threadRows else emptyList()
    override suspend fun ackWithEdits(identity: String, messageId: String) { foldedAcks++; acked += identity to messageId }
    override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
        attempts += SendAttempt(identity, to, body, threadId, action, attachments)
        return SendResponse("received", "sent", action.msgRef)
    }
    override suspend fun upload(identity: String, file: File, filename: String, mediaType: String): Attachment {
        uploads++; return super.upload(identity, file, filename, mediaType)
    }
    override suspend fun messageInfo(identity: String, messageId: String) = MessageInfo(messageId, versions = listOf(MessageVersion(messageId, "Fresh", ACTION_NOW)))
}

class MessageActionStoreTest {
    @Test fun reloadingOwnedMailboxesDoesNotStrandASendAfterAHandlePurchase() = runTest {
        val held = CompletableDeferred<Unit>()
        val api = object : ActionPostbox() {
            override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
                held.await()
                return super.sendWithActions(identity, to, body, threadId, action, attachments)
            }
        }
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer"); store.draft("In flight")
        store.send(); runCurrent(); assertTrue(store.state.value.isSending)
        store.loadAccount("/a"); runCurrent(); store.selectPeer("/peer")
        held.complete(Unit); advanceUntilIdle()
        assertEquals(1, api.attempts.size)
        assertFalse(store.state.value.isSending)
        assertEquals("", store.state.value.draft)
        store.reset()
    }

    @Test fun navigatingToAnotherConversationRetiresItsOldMessageInfo() = runTest {
        val held = CompletableDeferred<Unit>()
        val api = object : ActionPostbox() {
            override suspend fun messageInfo(identity: String, messageId: String): MessageInfo {
                withContext(NonCancellable) { held.await() }
                return super.messageInfo(identity, messageId)
            }
        }.also { it.rows = listOf(actionMessage()) }
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer")
        store.showMessageInfo("root"); runCurrent()
        store.selectPeer("/another")
        held.complete(Unit); advanceUntilIdle()
        assertNull(store.state.value.messageInfo)
        store.reset()
    }

    @Test fun capabilitiesAreKnownBeforeChoosingTheInboxWireView() = runTest {
        val ready = CompletableDeferred<Unit>()
        val api = object : ActionPostbox() {
            override suspend fun capabilities(): Capabilities { ready.await(); return super.capabilities() }
        }
        api.enabled = emptyList()
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); runCurrent()
        assertEquals(0, api.foldedReads); assertEquals(0, api.legacyReads)
        ready.complete(Unit); advanceUntilIdle()
        assertEquals(1, api.legacyReads)
        api.enabled = Features.MESSAGE_ACTIONS.toList()
        store.refresh(); advanceUntilIdle()
        assertEquals(1, api.foldedReads)
        store.reset()
    }

    @Test fun editingRestoresTheOrdinaryDraftAndKeepsItsAttachments() = runTest {
        val api = ActionPostbox().also { it.rows = listOf(actionMessage()) }
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer"); store.draft("Ordinary draft")
        val file = File.createTempFile("pp-action-original-", ".txt").also { it.writeText("attachment") }
        store.stage(store.state.value.draftKey!!, listOf(StagedAttachment(file = file, name = "notes.txt", mediaType = "text/plain")))
        store.beginEdit("root")
        assertEquals("Original", store.state.value.draft)
        assertTrue(store.state.value.composerAction!!.editing)
        assertTrue(store.state.value.attachments.isEmpty())
        store.draft("Changed")
        store.selectPeer("/other"); assertNull(store.state.value.composerAction)
        store.selectPeer("/peer"); assertEquals("Changed", store.state.value.draft)
        store.cancelMessageAction()
        assertEquals("Ordinary draft", store.state.value.draft)
        assertEquals(file, store.state.value.attachments.single().file)
        assertTrue(file.exists())
        store.reset(); assertFalse(file.exists())
    }

    @Test fun anEditPreservesTheRequestVerbAndArgumentsAndFoldsIntoItsRoot() = runTest {
        val original = """{"v":1,"verb":"ask","args":{"question":"Original","budget":5},"note":"Keep this context"}"""
        val api = object : ActionPostbox() {
            override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
                val response = super.sendWithActions(identity, to, body, threadId, action, attachments)
                rows = rows.map { it.copy(body = body, editedAt = ACTION_NOW, editCount = 1) }
                return response
            }
        }.also { it.rows = listOf(actionMessage(body = original)) }
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer"); store.draft("Unrelated draft")
        store.beginEdit("root"); store.draft("Changed"); store.send(); advanceUntilIdle()
        val attempt = api.attempts.single()
        val body = WireJson.parseToJsonElement(attempt.body).jsonObject
        assertEquals(ORIGINAL_REF, attempt.action.editOf)
        assertNotEquals(ORIGINAL_REF, attempt.action.msgRef)
        assertEquals("ask", body["verb"]!!.jsonPrimitive.content)
        assertEquals("Changed", body["args"]!!.jsonObject["question"]!!.jsonPrimitive.content)
        assertEquals("5", body["args"]!!.jsonObject["budget"].toString())
        assertEquals("Keep this context", body["note"]!!.jsonPrimitive.content)
        assertTrue(attempt.attachments.isEmpty())
        assertTrue(store.state.value.pending.isEmpty())
        assertEquals(listOf("root"), store.state.value.conversation!!.messages.map { it.id })
        assertEquals("Changed", store.state.value.conversation!!.messages.single().display.text)
        assertEquals("Unrelated draft", store.state.value.draft)
        assertNull(store.state.value.composerAction)
        store.reset()
    }

    @Test fun anUnconfirmedReplyFreezesItsBodyFilesReferenceAndGeneralSubject() = runTest {
        val api = object : ActionPostbox() {
            override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
                val result = super.sendWithActions(identity, to, body, threadId, action, attachments)
                if (attempts.size == 1) {
                    threadRows = listOf(ServerThread("canonical_general", "/peer", isDefault = true))
                    rows = rows + Message("sent", body, from = "/a", peer = "/peer", direction = "out", sentAt = ACTION_NOW,
                        threadId = "canonical_general", msgRef = action.msgRef)
                    throw IOException("response lost after commit")
                }
                return result.copy(duplicate = true)
            }
        }.also { it.rows = listOf(actionMessage(outgoing = false, body = "🙂".repeat(240) + "\nSecond line")) }
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer")
        val file = File.createTempFile("pp-action-reply-", ".txt").also { it.writeText("reply file") }
        store.stage(store.state.value.draftKey!!, listOf(StagedAttachment(file = file, name = "reply.txt", mediaType = "text/plain")))
        store.beginReply("root"); store.draft("Answer"); store.send(); advanceUntilIdle()
        val id = store.state.value.unconfirmedSend!!
        store.draft("Must not change"); store.cancelMessageAction(); store.send(); advanceUntilIdle()
        assertEquals("Answer", store.state.value.draft)
        assertEquals(1, api.attempts.size)
        assertFalse(store.canSubmitMessage())
        store.refresh(); advanceUntilIdle()
        assertEquals(id, store.state.value.unconfirmedSend)
        assertNull(store.state.value.draftKey!!.subject)
        assertEquals(2, store.state.value.conversation!!.messages.size)
        store.switchMailbox(store.state.value.mailboxes.last()); advanceUntilIdle()
        assertFalse(store.canRetrySend(id)); store.retrySend(id); advanceUntilIdle(); assertEquals(1, api.attempts.size)
        store.switchMailbox(store.state.value.mailboxes.first()); advanceUntilIdle(); store.selectPeer("/peer")
        assertTrue(store.canRetrySend(id)); store.retrySend(id); advanceUntilIdle()
        assertEquals(2, api.attempts.size)
        assertEquals(api.attempts.first(), api.attempts.last())
        val quote = api.attempts.first().action.quote!!
        assertEquals(200, quote.codePointCount(0, quote.length))
        assertEquals(1, api.uploads)
        assertFalse(file.exists())
        assertNull(store.state.value.unconfirmedSend)
        assertEquals("", store.state.value.draft)
        assertEquals(2, store.state.value.conversation!!.messages.size)
        store.reset()
    }

    @Test fun readMarksNeverHideANewUnreadEditAndAcknowledgmentsIncludeVersions() = runTest {
        val api = ActionPostbox().also { it.rows = listOf(actionMessage(outgoing = false)) }
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer")
        store.acknowledgeVisible(); advanceUntilIdle()
        assertTrue(store.state.value.messages.single().read == true)
        api.rows = api.rows.map { it.copy(body = "New edit", read = false, editedAt = ACTION_NOW, editCount = 1) }
        store.refresh(); advanceUntilIdle()
        assertFalse(store.state.value.messages.single().read == true)
        assertEquals(1, store.state.value.subject!!.unread)
        store.acknowledgeVisible(); advanceUntilIdle()
        assertEquals(2, api.foldedAcks)
        assertEquals(listOf("/a" to "root", "/a" to "root"), api.acked)
        assertTrue(store.state.value.messages.single().read == true)
        store.reset()
    }

    @Test fun messageInfoIgnoresACancelledOldDialogEvenAfterReturningToTheMailbox() = runTest {
        val held = CompletableDeferred<Unit>()
        val api = object : ActionPostbox() {
            override suspend fun messageInfo(identity: String, messageId: String): MessageInfo {
                if (messageId == "root") withContext(NonCancellable) { held.await() }
                return super.messageInfo(identity, messageId)
            }
        }.also { it.rows = listOf(actionMessage(), actionMessage().copy(id = "other", msgRef = "other_ref")) }
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer")
        store.showMessageInfo("root"); runCurrent()
        store.switchMailbox(store.state.value.mailboxes.last()); runCurrent(); assertNull(store.state.value.messageInfo)
        store.switchMailbox(store.state.value.mailboxes.first()); runCurrent(); store.selectPeer("/peer")
        store.showMessageInfo("other"); runCurrent()
        assertEquals("other", store.state.value.messageInfo!!.info!!.messageId)
        held.complete(Unit); advanceUntilIdle()
        assertEquals("other", store.state.value.messageInfo!!.info!!.messageId)
        store.reset()
    }

    @Test fun sendCapturesTheOriginalMailboxAndDraftBeforeItsCoroutineStarts() = runTest {
        val api = ActionPostbox()
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/first"); store.draft("First draft")
        store.send()
        store.switchMailbox(store.state.value.mailboxes.last()); store.selectPeer("/second"); store.draft("Second draft")
        advanceUntilIdle()
        val attempt = api.attempts.single()
        assertEquals("/a", attempt.identity); assertEquals("/first", attempt.peer)
        assertEquals("First draft", displayBody(attempt.body, true).text)
        assertEquals("/b", store.state.value.acting!!.address)
        assertEquals("Second draft", store.state.value.draft)
        store.reset()
    }

    @Test fun anExpiredOriginalCannotTurnAnEditIntoANewSend() = runTest {
        var now = ACTION_NOW
        val api = ActionPostbox().also { it.rows = listOf(actionMessage().copy(expiresAt = now + 30)) }
        val store = InboxStore(api, this, clock = { now })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer"); store.draft("Ordinary")
        store.beginEdit("root"); store.draft("Edited but not sent")
        now += 31; store.pruneExpired(); store.send(); advanceUntilIdle()
        assertTrue(api.attempts.isEmpty()); assertEquals(0, api.sends)
        assertFalse(store.canSubmitMessage())
        assertNotNull(store.messageComposerProblem())
        store.cancelMessageAction(); assertEquals("Ordinary", store.state.value.draft)
        store.reset()
    }

    @Test fun resetRetiresTheOriginalFrozenRequestInsteadOfReusingANewSession() = runTest {
        val api = object : ActionPostbox() {
            override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
                super.sendWithActions(identity, to, body, threadId, action, attachments)
                throw IOException("unconfirmed")
            }
        }
        val store = InboxStore(api, this, clock = { ACTION_NOW })
        store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer"); store.draft("Private draft")
        store.send(); advanceUntilIdle(); val old = store.state.value.unconfirmedSend!!
        store.reset(); store.loadAccount("/a"); advanceUntilIdle(); store.selectPeer("/peer")
        assertFalse(store.canRetrySend(old)); store.retrySend(old); advanceUntilIdle()
        assertEquals(1, api.attempts.size)
        assertTrue(store.state.value.pending.isEmpty()); assertEquals("", store.state.value.draft)
        store.reset()
    }
}
