package dev.pigeonpost.inbox

import dev.pigeonpost.core.*
import kotlinx.coroutines.delay
import java.io.File
import java.io.IOException
import java.util.UUID

/** Explicit debug scenarios only; this source set is absent from production builds. */
internal class MessageActionFixture(private val mode: String, private val base: PostboxApi, handles: AccountHandleApi) :
    PostboxApi by base, AccountHandleApi by handles {
    private var mailbox: String? = null
    private val now = System.currentTimeMillis() / 1000
    private val peer = "/demo/assistant"
    private val general = "actions_general"
    private var policyValue = FilterPolicy(readReceipts = true)
    private var lostOnce = false
    private val fileMetadata = mutableMapOf<String, Attachment>()
    private val versions = mutableMapOf<String, MutableList<MessageVersion>>()
    private val receipts = mutableMapOf<String, Pair<Attempt, SendResponse>>()
    private data class Attempt(val identity: String, val to: String, val body: String, val thread: String?, val action: MessageSendAction, val files: List<String>)
    private fun ref(letter: Char) = "mref_" + letter.toString().repeat(26)
    /** Like the postbox: a namespace resolves to its main mailbox, and the sent copy is filed under that mailbox's key and handle. */
    private fun filed(to: String) = if (to == "/demo") "/k/fixture-demo-main" to "/demo/main" else to to to
    private val messages = mutableListOf(
        Message("actions_parent", "The release plan is ready.\nPlease review the details.", from = peer, peer = peer, peerHandle = peer,
            direction = "in", receivedAt = now - 400, read = false, msgRef = ref('a'), threadId = "actions_earlier"),
        Message("actions_sent", workEnvelope("Sent message example"), to = peer, peer = peer, peerHandle = peer, direction = "out",
            sentAt = now - 250, msgRef = ref('b'), threadId = general, receipt = MessageReceipt("sent")),
        Message("actions_delivered", workEnvelope("Delivered message example"), to = peer, peer = peer, peerHandle = peer, direction = "out",
            sentAt = now - 210, msgRef = ref('c'), threadId = general, receipt = MessageReceipt("delivered", now - 200)),
        Message("actions_seen", workEnvelope("Seen message example"), to = peer, peer = peer, peerHandle = peer, direction = "out",
            sentAt = now - 170, msgRef = ref('d'), threadId = general, receipt = MessageReceipt("seen", now - 160, now - 150),
            replyTo = MessageReply(ref('a'), "actions_parent", peer, "in", quote = "The release plan is ready.")),
        Message("actions_working", workEnvelope("Please prepare the meeting notes."), to = peer, peer = peer, peerHandle = peer, direction = "out",
            sentAt = now - 100, msgRef = ref('e'), threadId = general, receipt = MessageReceipt("delivered", now - 90), agentStatus = MessageAgentStatus("working", now - 80)),
        Message("actions_failed", workEnvelope("Please check the attached proposal."), to = peer, peer = peer, peerHandle = peer, direction = "out",
            sentAt = now - 40, msgRef = ref('f'), threadId = general, receipt = MessageReceipt("delivered", now - 35), agentStatus = MessageAgentStatus("failed", now - 30)),
    )
    init { messages.forEach { versions[it.id] = mutableListOf(MessageVersion(it.id, it.body, it.at)) } }

    override suspend fun identities() = base.identities().also { mailbox = it.firstOrNull()?.address }
    private fun owned(identity: String) { if (identity != mailbox) throw ApiException(404, "not_found") }
    override suspend fun capabilities() = Capabilities(if (mode == "actions-off") emptyList() else Features.MESSAGE_ACTIONS.toList())
    override suspend fun contacts(identity: String) = ContactsResponse(if (identity == mailbox) listOf(Contact(peer, "Assistant")) else emptyList())
    override suspend fun archive(identity: String) = emptySet<String>()
    override suspend fun threads(identity: String) = if (identity == mailbox) listOf(
        ServerThread(general, peer, isDefault = true, lastAt = now), ServerThread("actions_earlier", peer, "Earlier discussion", lastAt = now - 400)) else emptyList()
    private fun projected(message: Message): Message = if (mode == "actions-off")
        message.copy(msgRef = null, replyTo = null, receipt = null, agentStatus = null, editedAt = null, editCount = null)
    else if (policyValue.readReceipts == false && message.receipt?.state == "seen")
        message.copy(receipt = message.receipt?.copy(state = "delivered", seenAt = null)) else message
    override suspend fun inbox(identity: String, wait: Int?): InboxResponse {
        if (wait != null) delay(1000)
        return InboxResponse(if (identity == mailbox) messages.map(::projected) else emptyList())
    }
    override suspend fun inboxWithActions(identity: String, wait: Int?): InboxResponse {
        if (mode == "actions-off") throw ApiException(403, "feature_disabled")
        return inbox(identity, wait)
    }
    override suspend fun filteredInboxWithActions(identity: String) = emptyList<Message>()
    override suspend fun policy(identity: String): FilterPolicy { owned(identity); return policyValue }
    override suspend fun setPolicy(identity: String, field: String, value: String): FilterPolicy {
        owned(identity); policyValue = policyValue.with(field, value); return policyValue
    }
    override suspend fun upload(identity: String, file: File, filename: String, mediaType: String): Attachment =
        base.upload(identity, file, filename, mediaType).also { fileMetadata[it.id] = it }
    override suspend fun ack(identity: String, messageId: String) {
        owned(identity); val index = messages.indexOfFirst { it.id == messageId }
        if (index >= 0) messages[index] = messages[index].copy(read = true)
    }
    override suspend fun ackWithEdits(identity: String, messageId: String) = ack(identity, messageId)
    override suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String>): SendResponse {
        owned(identity)
        val id = "actions_new_" + UUID.randomUUID()
        val (key, handle) = filed(to)
        messages += Message(id, body, from = identity, to = key, peer = key, peerHandle = handle, direction = "out", sentAt = System.currentTimeMillis() / 1000,
            threadId = threadId ?: general, attachments = attachments.mapNotNull(fileMetadata::get))
        versions[id] = mutableListOf(MessageVersion(id, body, messages.last().at))
        return SendResponse("received_" + id, id)
    }
    override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
        owned(identity)
        if (mode == "actions-off") throw ApiException(403, "feature_disabled")
        val attempt = Attempt(identity, to, body, threadId, action, attachments.toList())
        receipts[action.msgRef]?.let { prior ->
            if (prior.first != attempt) throw ApiException(409, "idempotency_conflict")
            return prior.second.copy(duplicate = true)
        }
        val at = System.currentTimeMillis() / 1000
        val id = "actions_new_" + UUID.randomUUID()
        if (action.editOf != null) {
            val index = messages.indexOfFirst { it.id == action.editOf || it.msgRef == action.editOf }
            if (index < 0 || !messages[index].outgoing) throw ApiException(404, "reference_not_found")
            val original = messages[index]
            if (at - original.at > 86400 || (original.expiresAt ?: Long.MAX_VALUE) <= at) throw ApiException(403, "edit_window_closed")
            if (attachments.isNotEmpty()) throw ApiException(400, "edit_has_attachments")
            messages[index] = original.copy(body = body, editedAt = at, editCount = (original.editCount ?: 0) + 1,
                receipt = MessageReceipt("sent"), agentStatus = null)
            versions.getOrPut(original.id) { mutableListOf(MessageVersion(original.id, original.body, original.at)) } += MessageVersion(id, body, at)
        } else {
            val parent = action.replyTo?.let { reference -> messages.firstOrNull { it.id == reference || it.msgRef == reference }
                ?: throw ApiException(404, "reference_not_found") }
            val (key, handle) = filed(to)
            messages += Message(id, body, from = identity, to = key, peer = key, peerHandle = handle, direction = "out", sentAt = at,
                threadId = threadId ?: general, msgRef = action.msgRef, attachments = attachments.mapNotNull(fileMetadata::get), receipt = MessageReceipt("sent"),
                replyTo = parent?.let { MessageReply(it.msgRef, it.id, it.from, it.direction, quote = action.quote) })
            versions[id] = mutableListOf(MessageVersion(id, body, at))
        }
        val result = SendResponse("received_" + id, id, action.msgRef)
        receipts[action.msgRef] = attempt to result
        if (mode == "actions-lost" && !lostOnce) { lostOnce = true; throw IOException("The fixture lost the committed response.") }
        return result
    }
    override suspend fun messageInfo(identity: String, messageId: String): MessageInfo {
        owned(identity)
        val message = messages.firstOrNull { it.id == messageId }?.let(::projected) ?: throw ApiException(404, "not_found")
        return MessageInfo(message.id, message.msgRef, message.direction, message.at, versions[messageId].orEmpty().toList(), message.replyTo, message.receipt, message.agentStatus)
    }
    override suspend fun deleteMessage(identity: String, id: String) { owned(identity); messages.removeAll { it.id == id }; versions.remove(id) }
}
