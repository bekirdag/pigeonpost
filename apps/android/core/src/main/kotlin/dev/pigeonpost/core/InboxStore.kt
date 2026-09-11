package dev.pigeonpost.core

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.io.File
import java.io.IOException
import java.util.UUID

data class DraftKey(val mailbox: String, val peer: String, val subject: String?)
data class StagedAttachment(val id: String = UUID.randomUUID().toString(), val file: File, val name: String, val mediaType: String)
data class InboxState(
    val accountLoading: Boolean = false,
    val accountLoaded: Boolean = false,
    val creating: Boolean = false,
    val mailboxes: List<Mailbox> = emptyList(),
    val acting: Mailbox? = null,
    val loading: Boolean = false,
    val loaded: Boolean = false,
    val offline: Boolean = false,
    val error: String? = null,
    val messages: List<Message> = emptyList(),
    val pending: List<PendingMessage> = emptyList(),
    val threads: List<ServerThread> = emptyList(),
    val contacts: List<Contact> = emptyList(),
    val vocabulary: Vocabulary = Vocabulary(),
    val archived: Set<String> = emptySet(),
    val quota: Quota? = null,
    val offer: HandleOffer? = null,
    val conversations: List<Conversation> = emptyList(),
    val selectedPeer: String? = null,
    val selectedSubject: String? = null,
    val filter: String = "",
    val viewingArchive: Boolean = false,
    val drafts: Map<DraftKey, String> = emptyMap(),
    val staged: Map<DraftKey, List<StagedAttachment>> = emptyMap(),
    val sending: Set<DraftKey> = emptySet(),
    val actionBusy: Boolean = false,
) {
    val visible get() = conversations.filter { conversation ->
        (conversation.peer in archived) == viewingArchive &&
            (filter.isBlank() || conversation.name.contains(filter, true) || conversation.peer.contains(filter, true))
    }
    val conversation get() = conversations.firstOrNull { it.peer == selectedPeer }
    val subjects get() = Conversations.subjects(conversation, threads, selectedPeer ?: "", messages)
    val subject get() = subjects.firstOrNull { it.id == selectedSubject } ?: subjects.firstOrNull()
    val draftKey get() = acting?.address?.let { mailbox -> selectedPeer?.let { DraftKey(mailbox, it, subject?.id?.takeIf(String::isNotEmpty)) } }
    val draft get() = drafts[draftKey].orEmpty()
    val attachments get() = staged[draftKey].orEmpty()
    val isSending get() = draftKey in sending
}

/** Owns account and inbox state independently of Android, so races can be tested on the JVM. */
class InboxStore(
    private val api: PostboxApi,
    private val scope: CoroutineScope,
    private val rememberMailbox: (String?) -> Unit = {},
    private val onSessionExpired: () -> Unit = {},
) {
    private val mutable = MutableStateFlow(InboxState())
    val state = mutable.asStateFlow()
    private var sessionVersion = 0L
    private var mailboxVersion = 0L
    private var active = false
    private var accountJob: Job? = null
    private var loadJob: Job? = null
    private var pollJob: Job? = null
    private val actions = mutableSetOf<Job>()
    private val acknowledged = mutableSetOf<Pair<String, String>>()

    fun loadAccount(remembered: String? = null): Job {
        accountJob?.cancel()
        loadJob?.cancel(); pollJob?.cancel()
        val epoch = ++sessionVersion
        mutable.update { it.copy(accountLoading = true, error = null) }
        return scope.launch {
            try {
                val rows = api.identities()
                val mailboxes = coroutineScope {
                    rows.map { row -> async {
                        val handle = try { api.whoami(row.address).handle }
                            catch (cancelled: CancellationException) { throw cancelled }
                            catch (expired: SessionExpired) { throw expired }
                            catch (_: IOException) { null }
                        Mailbox(row.address, handle, row.label)
                    } }.map { it.await() }
                }
                if (epoch != sessionVersion) return@launch
                val wanted = remembered ?: state.value.acting?.address
                val chosen = mailboxes.firstOrNull { it.address == wanted }
                    ?: mailboxes.firstOrNull { it.handle?.endsWith("/main") == true }
                    ?: mailboxes.firstOrNull { it.handle != null } ?: mailboxes.firstOrNull()
                mutable.update { it.copy(mailboxes = mailboxes, accountLoaded = true, accountLoading = false, creating = false) }
                if (chosen != null) switchMailbox(chosen) else mutable.update { it.copy(acting = null) }
            } catch (failure: Exception) { fail(failure, epoch) }
            finally { if (epoch == sessionVersion) mutable.update { it.copy(accountLoading = false, creating = false) } }
        }.also { accountJob = it }
    }

    fun createFirstInbox(): Job = action {
        if (state.value.creating || state.value.acting != null) return@action
        mutable.update { it.copy(creating = true, error = null) }
        try { api.createIdentity(); loadAccount() }
        finally { mutable.update { it.copy(creating = false) } }
    }

    fun switchMailbox(mailbox: Mailbox) {
        require(state.value.mailboxes.any { it.address == mailbox.address })
        ++mailboxVersion; loadJob?.cancel(); pollJob?.cancel()
        val old = state.value
        mutable.value = InboxState(accountLoaded = true, mailboxes = old.mailboxes, acting = mailbox,
            pending = old.pending, drafts = old.drafts, staged = old.staged, sending = old.sending)
        rememberMailbox(mailbox.address)
        refresh()
    }

    fun reset() {
        ++sessionVersion; ++mailboxVersion
        accountJob?.cancel(); loadJob?.cancel(); pollJob?.cancel()
        actions.toList().forEach { it.cancel() }; actions.clear()
        state.value.staged.values.flatten().forEach { it.file.delete() }
        acknowledged.clear(); mutable.value = InboxState(); rememberMailbox(null)
    }

    fun setActive(value: Boolean) {
        active = value
        if (!value) pollJob?.cancel() else if (state.value.loaded) startPoll()
    }
    fun clearError() { mutable.update { it.copy(error = null) } }
    fun showError(message: String) { mutable.update { it.copy(error = message) } }
    fun filter(value: String) { mutable.update { it.copy(filter = value) } }
    fun showArchive(value: Boolean) { mutable.update { it.copy(viewingArchive = value, selectedPeer = null, selectedSubject = null, filter = "") } }
    fun selectPeer(peer: String?) { mutable.update { it.copy(selectedPeer = peer, selectedSubject = null) } }
    fun selectSubject(id: String) { mutable.update { it.copy(selectedSubject = id) } }
    fun draft(value: String) {
        val key = state.value.draftKey ?: return
        mutable.update { it.copy(drafts = it.drafts + (key to value)) }
    }
    fun stage(key: DraftKey, files: List<StagedAttachment>) {
        if (key in state.value.sending) { files.forEach { it.file.delete() }; showError("Wait for the current message to finish sending before adding attachments."); return }
        if (key.mailbox !in state.value.mailboxes.map { it.address }) { files.forEach { it.file.delete() }; return }
        val previous = state.value.staged[key].orEmpty()
        if (previous.size + files.size > MAX_ATTACHMENTS) {
            files.forEach { it.file.delete() }; showError("Choose at most $MAX_ATTACHMENTS attachments."); return
        }
        mutable.update { it.copy(staged = it.staged + (key to (previous + files))) }
    }
    fun removeAttachment(id: String) {
        val key = state.value.draftKey ?: return
        if (key in state.value.sending) return
        state.value.staged[key].orEmpty().firstOrNull { it.id == id }?.file?.delete()
        mutable.update { it.copy(staged = it.staged + (key to it.staged[key].orEmpty().filterNot { file -> file.id == id })) }
    }

    fun refresh(): Job? {
        val identity = state.value.acting?.address ?: return null
        val epoch = sessionVersion
        val version = ++mailboxVersion
        loadJob?.cancel(); pollJob?.cancel()
        mutable.update { it.copy(loading = true, error = null) }
        return scope.launch {
            try {
                val snapshot = coroutineScope {
                    val inbox = async { api.inbox(identity) }
                    val contacts = async { api.contacts(identity) }
                    val threads = async { optional { api.threads(identity) }.orEmpty() }
                    val archive = async { api.archive(identity) }
                    val quota = async { optional { api.quota(identity) } }
                    val offer = async { optional { api.handleOffer() } }
                    Snapshot(inbox.await(), contacts.await(), threads.await(), archive.await(), quota.await(), offer.await())
                }
                if (!current(epoch, version, identity)) return@launch
                content { old -> old.copy(messages = readMarks(snapshot.inbox.messages.orEmpty(), identity),
                    contacts = snapshot.contacts.contacts.orEmpty(), vocabulary = snapshot.contacts.vocabulary ?: Vocabulary(),
                    threads = snapshot.threads, archived = normalizeArchive(snapshot.archived, snapshot.inbox.messages.orEmpty()),
                    quota = snapshot.quota, offer = snapshot.offer, loaded = true, loading = false, offline = false, error = null) }
            } catch (failure: Exception) { if (current(epoch, version, identity)) fail(failure, epoch) }
            finally {
                if (current(epoch, version, identity)) { mutable.update { it.copy(loading = false) }; if (active) startPoll() }
            }
        }.also { loadJob = it }
    }

    private fun startPoll() {
        if (!active || state.value.acting == null) return
        pollJob?.cancel()
        val identity = state.value.acting!!.address
        val epoch = sessionVersion
        val version = mailboxVersion
        pollJob = scope.launch {
            var retry = 1000L
            while (isActive && active && current(epoch, version, identity)) {
                try {
                    val inbox = api.inbox(identity, wait = 25)
                    val threads = optional { api.threads(identity) }.orEmpty()
                    if (!current(epoch, version, identity)) break
                    content { it.copy(messages = readMarks(inbox.messages.orEmpty(), identity), threads = threads, offline = false, loaded = true) }
                    retry = 1000
                    // Some servers answer an empty poll immediately. Never spin a hot loop.
                    delay(250)
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (failure: Exception) {
                    if (!current(epoch, version, identity)) break
                    if (failure is SessionExpired) { fail(failure, epoch); break }
                    mutable.update { it.copy(offline = true) }
                    delay(retry); retry = (retry * 2).coerceAtMost(30000)
                }
            }
        }
    }

    fun acknowledgeVisible(): Job = action {
        val epoch = sessionVersion
        val identity = state.value.acting?.address ?: return@action
        val ids = state.value.subject?.messages.orEmpty().filter { !it.outgoing && !it.read }.map { it.id }
        if (ids.isEmpty()) return@action
        ids.forEach { acknowledged += identity to it }
        content { it.copy(messages = readMarks(it.messages, identity)) }
        val unconfirmed = ids.toMutableSet()
        try {
            for (id in ids) {
                api.ack(identity, id)
                unconfirmed -= id
            }
        } finally {
            if (epoch == sessionVersion && unconfirmed.isNotEmpty()) {
                unconfirmed.forEach { acknowledged -= identity to it }
                if (state.value.acting?.address == identity) content { old -> old.copy(messages = old.messages.map { if (it.id in unconfirmed) it.copy(read = false) else it }) }
            }
        }
    }

    fun send(): Job = action {
        val snapshot = state.value
        val key = snapshot.draftKey ?: return@action
        val text = snapshot.draft.trim()
        val files = snapshot.attachments
        if ((text.isEmpty() && files.isEmpty()) || key in snapshot.sending) return@action
        require(text.toByteArray().size <= 256 * 1024) { "This message is too long." }
        val epoch = sessionVersion
        val localId = "local_" + UUID.randomUUID()
        val wire = workEnvelope(text)
        content { it.copy(pending = it.pending + PendingMessage(localId, key.mailbox, key.peer, wire, System.currentTimeMillis() / 1000, key.subject), sending = it.sending + key, error = null) }
        try {
            val uploaded = files.map { api.upload(key.mailbox, it.file, it.name, it.mediaType) }
            val sent = api.send(key.mailbox, key.peer, wire, key.subject, uploaded.map { it.id })
            if (epoch != sessionVersion) return@action
            content { old -> old.copy(pending = old.pending.map { if (it.id == localId) it.copy(status = Delivery.SENT, sentCopyId = sent.sentCopyId, attachments = uploaded) else it }.filterNot { it.id == localId && sent.sentCopyId == null },
                drafts = if (old.drafts[key]?.trim() == text) old.drafts - key else old.drafts, staged = old.staged - key) }
            files.forEach { it.file.delete() }
            if (state.value.acting?.address == key.mailbox) refresh()
        } catch (failure: Exception) {
            if (epoch == sessionVersion) content { old -> old.copy(pending = old.pending.map { if (it.id == localId) it.copy(status = Delivery.FAILED) else it }) }
            if (failure is IOException && failure !is ApiException && failure !is SessionExpired)
                throw IOException("Delivery could not be confirmed. Check the conversation before sending again.", failure)
            throw failure
        } finally { if (epoch == sessionVersion) mutable.update { it.copy(sending = it.sending - key) } }
    }

    fun openThread(title: String, done: () -> Unit = {}): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        val peer = state.value.selectedPeer ?: return@command
        require(title.isNotBlank() && title.length <= 160) { "Enter a subject up to 160 characters." }
        val id = api.openThread(identity, peer, title.trim())
        if (epoch == sessionVersion && state.value.acting?.address == identity && state.value.selectedPeer == peer) {
            content { it.copy(threads = it.threads + ServerThread(id, peer, title.trim()), selectedSubject = id) }; done()
        }
    }
    fun deleteThread(id: String, done: () -> Unit = {}): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        api.deleteThread(identity, id)
        if (epoch == sessionVersion && state.value.acting?.address == identity) {
            content { it.copy(messages = it.messages.filterNot { message -> message.threadId == id }, threads = it.threads.filterNot { thread -> thread.id == id }, selectedSubject = null) }
            done(); refresh()
        }
    }
    fun deleteMessage(id: String): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        if (state.value.pending.any { it.id == id }) {
            if (state.value.pending.any { it.id == id && it.status == Delivery.SENDING }) return@command
            content { it.copy(pending = it.pending.filterNot { message -> message.id == id }) }; return@command
        }
        api.deleteMessage(identity, id)
        if (epoch == sessionVersion && state.value.acting?.address == identity) { content { it.copy(messages = it.messages.filterNot { message -> message.id == id }) }; refresh() }
    }
    fun reportSpam(id: String): Job = command { state.value.acting?.address?.let { api.reportSpam(it, id) } }
    fun archive(peer: String, archived: Boolean): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        api.setArchived(identity, peer, archived)
        if (epoch == sessionVersion && state.value.acting?.address == identity) content { it.copy(archived = if (archived) it.archived + peer else it.archived - peer) }
    }
    fun saveContact(contact: Contact, done: () -> Unit = {}): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        val allowed = state.value.vocabulary.safeGrantable
        val sanitized = contact.copy(allowedVerbs = if (contact.autonomy == "auto" && contact.admission == "allow") contact.allowedVerbs.orEmpty().filter { it in allowed } else emptyList(),
            autonomy = if (contact.admission == "block") "review" else contact.autonomy)
        api.saveContact(identity, sanitized)
        if (epoch == sessionVersion && state.value.acting?.address == identity) { content { it.copy(contacts = it.contacts.filterNot { row -> row.peer == contact.peer } + sanitized) }; done() }
    }
    fun removeContact(peer: String, done: () -> Unit = {}): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        api.removeContact(identity, peer)
        if (epoch == sessionVersion && state.value.acting?.address == identity) { content { it.copy(contacts = it.contacts.filterNot { row -> row.peer == peer }) }; done() }
    }
    fun markKnown(peer: String): Job {
        val previous = Conversations.contact(peer, state.value.contacts)
        return saveContact(Contact(peer, previous?.alias, "allow", previous?.autonomy ?: "review", previous?.allowedVerbs))
    }
    fun fullPermissions(peer: String, allowed: Boolean): Job {
        val previous = Conversations.contact(peer, state.value.contacts)
        return saveContact(Contact(peer, previous?.alias, "allow", if (allowed) "auto" else "review", if (allowed) state.value.vocabulary.safeGrantable else emptyList()))
    }
    fun block(peer: String) = saveContact(Contact(peer, Conversations.contact(peer, state.value.contacts)?.alias, "block", "review", emptyList()))

    private fun command(work: suspend (Long) -> Unit): Job = action {
        if (state.value.actionBusy) return@action
        val epoch = sessionVersion
        mutable.update { it.copy(actionBusy = true, error = null) }
        try { work(epoch) } finally { if (epoch == sessionVersion) mutable.update { it.copy(actionBusy = false) } }
    }
    private fun action(work: suspend () -> Unit): Job {
        val epoch = sessionVersion
        val job = scope.launch {
            try { if (epoch == sessionVersion) work() } catch (failure: Exception) { fail(failure, epoch) }
        }
        actions += job
        job.invokeOnCompletion { actions -= job }
        return job
    }
    private fun fail(failure: Exception, epoch: Long) {
        if (failure is CancellationException || epoch != sessionVersion) return
        if (failure is SessionExpired) onSessionExpired()
        mutable.update { it.copy(error = failure.message ?: "Could not reach the postbox.", offline = failure is IOException && failure !is ApiException) }
    }
    private fun current(epoch: Long, version: Long, identity: String) = epoch == sessionVersion && version == mailboxVersion && state.value.acting?.address == identity
    private fun readMarks(messages: List<Message>, identity: String) = messages.map { if (identity to it.id in acknowledged) it.copy(read = true) else it }
    private fun normalizeArchive(peers: Set<String>, messages: List<Message>): Set<String> { val aliases = Conversations.aliases(messages); return peers.map { aliases[it] ?: it }.toSet() }
    private fun content(change: (InboxState) -> InboxState) {
        mutable.update { old ->
            val next = change(old)
            val ids = next.messages.mapTo(mutableSetOf()) { it.id }
            val pending = next.pending.filterNot { it.mailbox == next.acting?.address && (it.sentCopyId in ids || it.id in ids) }
            next.copy(pending = pending, conversations = Conversations.build(next.messages, pending, next.contacts, next.mailboxes, next.acting?.address))
        }
    }
    private suspend fun <T> optional(work: suspend () -> T): T? = try { work() } catch (failure: ApiException) { if (failure.status == 404) null else throw failure }
    private data class Snapshot(val inbox: InboxResponse, val contacts: ContactsResponse, val threads: List<ServerThread>, val archived: Set<String>, val quota: Quota?, val offer: HandleOffer?)
}
