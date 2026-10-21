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
/** What the conversation column shows: conversations, the Calls page, or the Filtered list. */
enum class LeftView { INBOX, CALLS, FILTERED }
data class CallsState(
    val entries: List<CallLogEntry> = emptyList(),
    /** Calls silenced by the filtering settings; loaded when that section is first opened. */
    val filtered: List<CallLogEntry>? = null,
    val unseen: Int = 0,
    val filter: String = "all",
    val next: Long? = null,
    val loaded: Boolean = false,
    val loading: Boolean = false,
    val error: String? = null,
) {
    val shown get() = entries.filter { it.outcome != "filtered" }
}
/** Messages the server filed as Filtered, one row per sender. */
data class FilteredGroup(val peer: String, val messages: List<Message>) {
    val last get() = messages.maxOfOrNull { it.at } ?: 0
    val from get() = messages.lastOrNull()?.from ?: peer
}
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
    val features: Set<String> = emptySet(),
    val leftView: LeftView = LeftView.INBOX,
    val filtered: List<Message> = emptyList(),
    val calls: CallsState = CallsState(),
    val policy: FilterPolicy? = null,
    val policyError: String? = null,
    val blocked: List<Contact>? = null,
    val blockedError: String? = null,
) {
    fun has(feature: String) = feature in features
    val filteredGroups: List<FilteredGroup> get() {
        if (!has(Features.MESSAGE_FILTERING)) return emptyList()
        val aliases = Conversations.aliases(filtered)
        return filtered.groupBy { aliases[it.peerKey] ?: it.peerKey }
            .filter { (peer, _) -> Conversations.contact(peer, contacts)?.admission != "block" }
            .map { (peer, rows) -> FilteredGroup(peer, rows.sortedBy { it.at }) }.sortedByDescending { it.last }
    }
    /** Missed-call lines join the general subject of a one-to-one conversation. */
    private val callLines get() = if (has(Features.CALL_HISTORY)) Conversations.missedCallLines(conversation, calls.entries, ::clockTime) else emptyList()
    val visible get() = conversations.filter { conversation ->
        (conversation.peer in archived) == viewingArchive &&
            (filter.isBlank() || conversation.name.contains(filter, true) || conversation.peer.contains(filter, true))
    }
    val conversation get() = conversations.firstOrNull { it.peer == selectedPeer }
    val subjects get() = Conversations.subjects(conversation, threads, selectedPeer ?: "", messages, callLines)
    val subject get() = subjects.firstOrNull { it.id == selectedSubject } ?: subjects.firstOrNull()
    val draftKey get() = acting?.address?.let { mailbox -> selectedPeer?.let { DraftKey(mailbox, it, subject?.id?.takeIf(String::isNotEmpty)) } }
    val draft get() = drafts[draftKey].orEmpty()
    val attachments get() = staged[draftKey].orEmpty()
    val isSending get() = draftKey in sending
    /** The conversation is with someone this mailbox blocked: a banner replaces the composer. */
    val blockedPeer get() = conversation?.blocked == true
}

fun clockTime(epoch: Long): String = java.time.Instant.ofEpochSecond(epoch).atZone(java.time.ZoneId.systemDefault())
    .format(java.time.format.DateTimeFormatter.ofLocalizedTime(java.time.format.FormatStyle.SHORT))

/** Owns account and inbox state independently of Android, so races can be tested on the JVM. */
class InboxStore(
    private val api: PostboxApi,
    private val scope: CoroutineScope,
    private val rememberMailbox: (String?) -> Unit = {},
    private val onSessionExpired: () -> Unit = {},
    private val clock: () -> Long = { System.currentTimeMillis() / 1000 },
) {
    private val mutable = MutableStateFlow(InboxState())
    val state = mutable.asStateFlow()
    private var sessionVersion = 0L
    private var mailboxVersion = 0L
    private var active = false
    private var accountJob: Job? = null
    private var loadJob: Job? = null
    private var pollJob: Job? = null
    private var expiryJob: Job? = null
    private var expiryTarget: Long? = null
    private var expiryFired: Long? = null
    private var lastSideLoad = 0L
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
            pending = old.pending, drafts = old.drafts, staged = old.staged, sending = old.sending, features = old.features)
        rememberMailbox(mailbox.address)
        refresh()
    }

    fun reset() {
        ++sessionVersion; ++mailboxVersion
        accountJob?.cancel(); loadJob?.cancel(); pollJob?.cancel(); expiryJob?.cancel()
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
    fun showView(view: LeftView) {
        val allowed = when (view) { LeftView.CALLS -> state.value.has(Features.CALL_HISTORY); LeftView.FILTERED -> state.value.has(Features.MESSAGE_FILTERING); LeftView.INBOX -> true }
        mutable.update { it.copy(leftView = if (allowed) view else LeftView.INBOX, viewingArchive = if (view != LeftView.INBOX) false else it.viewingArchive) }
        if (allowed && view == LeftView.CALLS) action { if (!state.value.calls.loaded) loadCallsNow(); markCallsSeenNow() }
    }
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
                    val features = async { features() }
                    Snapshot(inbox.await(), contacts.await(), threads.await(), archive.await(), quota.await(), offer.await(), features.await())
                }
                if (!current(epoch, version, identity)) return@launch
                mutable.update { it.copy(features = snapshot.features, leftView = when {
                    it.leftView == LeftView.CALLS && Features.CALL_HISTORY !in snapshot.features -> LeftView.INBOX
                    it.leftView == LeftView.FILTERED && Features.MESSAGE_FILTERING !in snapshot.features -> LeftView.INBOX
                    else -> it.leftView }) }
                content { old -> old.copy(messages = readMarks(snapshot.inbox.messages.orEmpty(), identity),
                    contacts = snapshot.contacts.contacts.orEmpty(), vocabulary = snapshot.contacts.vocabulary ?: Vocabulary(),
                    threads = snapshot.threads, archived = normalizeArchive(snapshot.archived, snapshot.inbox.messages.orEmpty()),
                    quota = snapshot.quota, offer = snapshot.offer, loaded = true, loading = false, offline = false, error = null) }
                sideLoads(epoch, version, identity)
            } catch (failure: Exception) { if (current(epoch, version, identity)) fail(failure, epoch) }
            finally {
                if (current(epoch, version, identity)) { mutable.update { it.copy(loading = false) }; if (active) startPoll() }
            }
        }.also { loadJob = it }
    }

    private suspend fun features(): Set<String> = try { api.capabilities().enabled }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (expired: SessionExpired) { throw expired }
        catch (_: Exception) { emptySet() }

    /** Filtered mail and the call log: best effort, never blocking the inbox itself. */
    private suspend fun sideLoads(epoch: Long, version: Long, identity: String) {
        lastSideLoad = clock()
        val features = state.value.features
        coroutineScope {
            if (Features.MESSAGE_FILTERING in features) launch {
                val rows = quietly { api.filteredInbox(identity) }
                if (rows != null && current(epoch, version, identity)) content { it.copy(filtered = rows) }
            }
            if (Features.CALL_HISTORY in features) launch { loadCallsNow(identity = identity, epoch = epoch, version = version) }
        }
    }
    private suspend fun <T> quietly(work: suspend () -> T): T? = try { work() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (expired: SessionExpired) { throw expired }
        catch (_: Exception) { null }

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
                    if (clock() - lastSideLoad >= 30) sideLoads(epoch, version, identity)
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

    // ---- Phase 1b: blocking ------------------------------------------------------------------

    /**
     * Block `peer` (or everyone at its namespace) and, when asked, report the message or call it
     * came from. "They won't be able to message or call you, and won't be told."
     */
    fun blockSender(peer: String, wide: Boolean = false, report: Boolean = false, messageId: String? = null, callId: String? = null, done: () -> Unit = {}): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        val target = if (wide) wildcardFor(peer) ?: peer else peer
        require(target != identity && target != state.value.acting?.handle) { "You cannot block this mailbox itself." }
        val existing = state.value.contacts.firstOrNull { it.peer == target }
        val row = Contact(target, existing?.alias, "block", "review", emptyList())
        api.saveContact(identity, row)
        if (epoch != sessionVersion || state.value.acting?.address != identity) return@command
        content { old -> old.copy(contacts = old.contacts.filterNot { it.peer == target } + row, blocked = old.blocked?.let { list -> list.filterNot { it.peer == target } + row }) }
        var note = "Blocked. They won’t be able to message or call you."
        if (report) {
            val message = messageId ?: (state.value.messages + state.value.filtered).lastOrNull { !it.outgoing && (Conversations.aliases(state.value.messages)[it.peerKey] ?: it.peerKey) == peer }?.id
            note = try {
                when { callId != null -> api.reportCall(identity, callId); message != null -> api.reportSpam(identity, message) }
                "Blocked and reported as spam."
            } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { "Blocked, but the spam report could not be sent." }
        }
        mutable.update { it.copy(error = note) }
        done()
    }

    fun loadBlocked(): Job = action {
        val epoch = sessionVersion
        val identity = state.value.acting?.address ?: return@action
        mutable.update { it.copy(blocked = null, blockedError = null) }
        try {
            val rows = api.blockedContacts(identity)
            if (epoch == sessionVersion && state.value.acting?.address == identity) mutable.update { it.copy(blocked = rows) }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (expired: SessionExpired) { throw expired }
        catch (_: Exception) {
            if (epoch == sessionVersion && state.value.acting?.address == identity)
                mutable.update { it.copy(blocked = it.contacts.filter { row -> row.admission == "block" }, blockedError = "Could not refresh the list. Showing what was last loaded.") }
        }
    }

    /** Unblocking is a person's decision; the server refuses it from agents. */
    fun unblock(peer: String): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        api.unblock(identity, peer)
        if (epoch == sessionVersion && state.value.acting?.address == identity) {
            content { old -> old.copy(contacts = old.contacts.filterNot { it.peer == peer }, blocked = old.blocked?.filterNot { it.peer == peer }, error = "Unblocked $peer.") }
        }
    }

    // ---- Phase 1b: filtering -----------------------------------------------------------------

    fun acceptFiltered(group: FilteredGroup, done: () -> Unit = {}): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        api.acceptFiltered(identity, group.from)
        if (epoch != sessionVersion || state.value.acting?.address != identity) return@command
        content { old -> old.copy(filtered = old.filtered.filterNot { (Conversations.aliases(old.filtered)[it.peerKey] ?: it.peerKey) == group.peer }, leftView = LeftView.INBOX,
            selectedPeer = group.peer, selectedSubject = null) }
        done()
        refresh()?.join()
        mutable.update { it.copy(error = "Accepted. Their messages are in your inbox.") }
    }

    fun reportAvatar(address: String): Job = action {
        val identity = state.value.acting?.address ?: return@action
        try { api.reportAvatar(identity, address); showError("Picture reported.") }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { showError("Could not send the report.") }
    }

    fun reportMessage(id: String, done: () -> Unit = {}): Job = action {
        val identity = state.value.acting?.address ?: return@action
        try { api.reportSpam(identity, id); done() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { showError("Could not send the report.") }
    }

    fun loadPolicy(): Job = action {
        val epoch = sessionVersion
        val identity = state.value.acting?.address ?: return@action
        mutable.update { it.copy(policy = null, policyError = null) }
        try {
            val policy = api.policy(identity)
            if (epoch == sessionVersion && state.value.acting?.address == identity) mutable.update { it.copy(policy = policy) }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (expired: SessionExpired) { throw expired }
        catch (_: Exception) { if (epoch == sessionVersion) mutable.update { it.copy(policyError = "Could not load these settings. Close and try again.") } }
    }

    fun setPolicy(field: String, value: String): Job = action {
        val epoch = sessionVersion
        val identity = state.value.acting?.address ?: return@action
        val previous = state.value.policy ?: return@action
        mutable.update { it.copy(policy = previous.with(field, value), policyError = null) }
        try {
            val saved = api.setPolicy(identity, field, value)
            if (epoch == sessionVersion && state.value.acting?.address == identity) mutable.update { it.copy(policy = saved, error = "Saved.") }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (expired: SessionExpired) { throw expired }
        catch (_: Exception) {
            if (epoch == sessionVersion && state.value.acting?.address == identity) mutable.update { it.copy(policy = previous, policyError = "Could not save that setting. Please try again.") }
        }
    }

    // ---- Phase 1b: call history --------------------------------------------------------------

    private suspend fun loadCallsNow(more: Boolean = false, identity: String? = null, epoch: Long = sessionVersion, version: Long = mailboxVersion) {
        val mailbox = identity ?: state.value.acting?.address ?: return
        if (!state.value.has(Features.CALL_HISTORY)) return
        val calls = state.value.calls
        val filter = calls.filter
        mutable.update { it.copy(calls = it.calls.copy(loading = true)) }
        try {
            val page = api.callLog(mailbox, filter, if (more) calls.next else null)
            if (!current(epoch, version, mailbox) || state.value.calls.filter != filter) return
            val rows = page.entries.orEmpty()
            mutable.update { old -> old.copy(calls = old.calls.copy(
                entries = if (more) old.calls.entries + rows.filter { row -> old.calls.entries.none { it.id == row.id } } else rows,
                next = page.nextBefore, unseen = page.unseenMissed.coerceAtLeast(0), loaded = true, loading = false, error = null)) }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (expired: SessionExpired) { throw expired }
        catch (_: Exception) {
            if (current(epoch, version, mailbox)) mutable.update { it.copy(calls = it.calls.copy(loading = false, error = "Could not load your calls.")) }
        }
    }

    fun loadCalls(more: Boolean = false): Job = action { loadCallsNow(more) }
    fun callFilter(filter: String): Job = action {
        require(filter in setOf("all", "missed"))
        mutable.update { it.copy(calls = it.calls.copy(filter = filter, entries = emptyList(), next = null, loaded = false)) }
        loadCallsNow()
    }
    fun loadFilteredCalls(): Job = action {
        val identity = state.value.acting?.address ?: return@action
        val rows = quietly { api.callLog(identity, "filtered").entries.orEmpty() } ?: emptyList()
        if (state.value.acting?.address == identity) mutable.update { it.copy(calls = it.calls.copy(filtered = rows)) }
    }

    /** Looking at the Calls page is seeing the missed calls on it. */
    fun markCallsSeen(): Job = action { markCallsSeenNow() }
    private suspend fun markCallsSeenNow() {
        val identity = state.value.acting?.address ?: return
        val calls = state.value.calls
        val newest = calls.entries.maxOfOrNull { it.startedAt } ?: return
        if (calls.unseen == 0 || newest <= 0) return
        mutable.update { it.copy(calls = it.calls.copy(unseen = 0)) }
        val left = quietly { api.markCallsSeen(identity, newest) } ?: return
        if (state.value.acting?.address == identity) mutable.update { old -> old.copy(calls = old.calls.copy(unseen = left,
            entries = old.calls.entries.map { if (it.startedAt <= newest) it.copy(seen = true) else it })) }
    }

    fun deleteCall(entry: CallLogEntry): Job = action {
        val identity = state.value.acting?.address ?: return@action
        try {
            api.deleteCall(identity, entry.id)
            if (state.value.acting?.address == identity) mutable.update { old -> old.copy(error = "Removed from call history.",
                calls = old.calls.copy(entries = old.calls.entries.filterNot { it.id == entry.id }, filtered = old.calls.filtered?.filterNot { it.id == entry.id })) }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { showError("Could not remove that call.") }
    }

    fun clearCalls(): Job = action {
        val identity = state.value.acting?.address ?: return@action
        try {
            api.clearCalls(identity)
            if (state.value.acting?.address == identity) mutable.update { old -> old.copy(error = "Call history cleared.",
                calls = old.calls.copy(entries = emptyList(), filtered = old.calls.filtered?.let { emptyList() }, next = null, unseen = 0)) }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { showError("Could not clear your call history.") }
    }

    fun reportCall(entry: CallLogEntry): Job = action {
        val identity = state.value.acting?.address ?: return@action
        try { api.reportCall(identity, entry.id); showError("Reported as spam.") }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { showError("Could not send the report.") }
    }

    /** A `call.log` event or a call that just ended: look again soon. */
    fun callsChanged(): Job = action { loadCallsNow() }

    // ---- Phase 3a: disappearing direct messages ----------------------------------------------

    fun setThreadTimer(threadId: String, seconds: Long?): Job = command { epoch ->
        val identity = state.value.acting?.address ?: return@command
        api.setThreadTimer(identity, threadId, seconds)
        if (epoch != sessionVersion || state.value.acting?.address != identity) return@command
        content { old -> old.copy(threads = old.threads.map { if (it.id == threadId) it.copy(ephemeralSecs = seconds) else it },
            error = if (seconds == null) "Disappearing messages are off." else "New messages disappear after ${timerLabel(seconds)}.") }
    }

    /** Expired messages leave the screen on this device's own clock, even when no event said so. */
    fun pruneExpired() { content { it } }

    /** `message.expired` for a direct thread: these ids are gone now. */
    fun dropMessages(ids: Collection<String>) {
        val set = ids.toSet()
        if (set.isNotEmpty()) content { old -> old.copy(messages = old.messages.filterNot { it.id in set }) }
    }

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
        val now = clock()
        mutable.update { old ->
            val changed = change(old)
            val next = if (changed.messages.any { (it.expiresAt ?: Long.MAX_VALUE) <= now } || changed.filtered.any { (it.expiresAt ?: Long.MAX_VALUE) <= now })
                changed.copy(messages = changed.messages.filterNot { (it.expiresAt ?: Long.MAX_VALUE) <= now }, filtered = changed.filtered.filterNot { (it.expiresAt ?: Long.MAX_VALUE) <= now })
                else changed
            val ids = next.messages.mapTo(mutableSetOf()) { it.id }
            val pending = next.pending.filterNot { it.mailbox == next.acting?.address && (it.sentCopyId in ids || it.id in ids) }
            next.copy(pending = pending, conversations = Conversations.build(next.messages, pending, next.contacts, next.mailboxes, next.acting?.address))
        }
        scheduleExpiry()
    }

    /** Wake when the next message runs out, so it leaves the screen on time. */
    private fun scheduleExpiry() {
        val next = state.value.messages.mapNotNull { it.expiresAt }.minOrNull()
        if (next != null && next == expiryTarget && expiryJob?.isActive == true) return
        expiryJob?.cancel()
        // Woken once for this moment already: a clock that has not caught up waits for the next load.
        if (next == null || next == expiryFired) return
        expiryTarget = next
        val epoch = sessionVersion
        expiryJob = scope.launch {
            delay(((next - clock()).coerceAtLeast(0) * 1000 + 250).coerceAtMost(3_600_000))
            expiryFired = next
            if (epoch == sessionVersion) content { it }
        }
    }
    private suspend fun <T> optional(work: suspend () -> T): T? = try { work() } catch (failure: ApiException) { if (failure.status == 404) null else throw failure }
    private data class Snapshot(val inbox: InboxResponse, val contacts: ContactsResponse, val threads: List<ServerThread>, val archived: Set<String>, val quota: Quota?, val offer: HandleOffer?, val features: Set<String>)
}
