package dev.pigeonpost.core.groups

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.Features
import dev.pigeonpost.core.SessionExpired
import dev.pigeonpost.core.WireJson
import dev.pigeonpost.core.displayName
import dev.pigeonpost.core.timerLabel
import dev.pigeonpost.core.validAddress
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray

/** A `/g/<id>#token` link waiting for the person to confirm. */
data class JoinRequest(val groupId: String, val token: String?, val preview: GroupPreview? = null, val error: String? = null, val joining: Boolean = false)

data class GroupsState(
    val contextId: String = java.util.UUID.randomUUID().toString(),
    val enabled: Boolean = false,
    /** Groups are switched on but this device cannot load the encryption library: no groups, never a weaker fallback. */
    val unavailable: Boolean = false,
    val disappearing: Boolean = false,
    val mailbox: String? = null,
    val groups: List<Group> = emptyList(),
    val open: String? = null,
    val members: Map<String, List<GroupMember>> = emptyMap(),
    val devices: Map<String, List<DeviceRow>> = emptyMap(),
    val history: Map<String, List<HistoryItem>> = emptyMap(),
    val inGroup: Set<String> = emptySet(),
    val safety: Map<String, String> = emptyMap(),
    val call: GroupCallState? = null,
    val deviceId: String? = null,
    val fingerprint: String? = null,
    val persistent: Boolean = true,
    val sending: Boolean = false,
    val busy: Boolean = false,
    val notice: String? = null,
    val join: JoinRequest? = null,
    val handles: Map<String, String> = emptyMap(),
    val features: Set<String> = emptySet(),
    val drafts: Map<String, String> = emptyMap(),
    val composers: Map<String, GroupComposerAction> = emptyMap(),
    val pendingIds: Map<String, Set<String>> = emptyMap(),
    val receipts: Map<String, GroupReceipts> = emptyMap(),
) {
    val invitations get() = groups.filter { it.invited }
    val joined get() = groups.filter { !it.invited && it.deletedAt == null }
    val current get() = groups.firstOrNull { it.id == open }
    fun unconfirmed(gid: String) = history[gid].orEmpty().any { it.pending }
}

/** How a person sees an error code from the group routes (as the web inbox words them). */
fun explainGroupError(failure: Throwable): String = when ((failure as? ApiException)?.code) {
    "not_admin" -> "Only admins can do that."
    "owner_only" -> "Only the owner can do that."
    "human_required" -> "This needs you to be signed in yourself, not an agent key."
    "posting_restricted" -> "Only admins can send messages here."
    "banned" -> "That mailbox is banned from this group."
    "group_full" -> "The group is full."
    "invalid_link" -> "This invite link is no longer valid."
    "invite_required" -> "This group is invite only."
    "already_member" -> "Already a member."
    "group_deleted" -> "This group was deleted."
    "revision_conflict" -> "Someone else changed this at the same time. Try again."
    "not_member" -> "You are not a member of this group."
    "rate_limited" -> "Too many requests. Wait a moment and try again."
    "quota_exceeded" -> "This group is out of storage."
    "feature_disabled" -> "Groups are not enabled for this account."
    "not_found" -> "Not found."
    "not_editable", "edit_window_closed" -> "This message can no longer be edited."
    "unconfirmed_send" -> "Resolve the earlier send before writing another message."
    "send_unavailable" -> "This device cannot retry that send. You can stop retrying and keep writing."
    "send_retry_expired" -> "That send is too old to retry."
    "outbox_full" -> "Resolve your unconfirmed messages before sending more."
    else -> (failure as? GroupCryptoError)?.let { "This device could not decrypt that (${it.code})." }
        ?: failure.message?.takeIf { failure !is ApiException } ?: "Something went wrong."
}

/**
 * Groups for the mailbox in use: the list, the open group, its members and history, invitations,
 * links and the call banner. Polls while the app is in front (no push wakes a closed app with
 * group content, by design) and drives [GroupClient] for everything cryptographic.
 */
class GroupStore(
    private val transport: GroupTransport,
    private val crypto: () -> GroupCryptoLibrary?,
    private val storage: GroupStorage,
    private val scope: CoroutineScope,
    private val devicePrefix: String = "android",
    private val now: () -> Long = { System.currentTimeMillis() / 1000 },
    private val pollSeconds: Long = 15,
    private val openPollSeconds: Long = 3,
    private val onSessionExpired: () -> Unit = {},
) {
    private val mutable = MutableStateFlow(GroupsState())
    val state: StateFlow<GroupsState> = mutable.asStateFlow()
    private var client: GroupClient? = null
    private var actorAuthorization: AuthorizationLease? = null
    private var generation = 0L
    private var pollJob: Job? = null
    private var openJob: Job? = null
    private var active = false
    private val refreshing = Mutex()
    private var lastTopUp = 0L

    /** The mailbox child is frozen before launch, including work queued behind a dispatcher. */
    private fun launch(work: suspend CoroutineScope.() -> Unit): Job {
        val authorization = actorAuthorization ?: return Job().apply { complete() }
        return scope.launch(AuthorizationContext(authorization)) {
            authorization.requireActive()
            work()
        }
    }

    private fun note(text: String) = mutable.update { it.copy(notice = text) }
    fun clearNotice() = mutable.update { it.copy(notice = null) }
    fun showNotice(text: String) = note(text)

    /** The mailbox or the capabilities changed. */
    fun onMailbox(mailbox: String?, handle: String?, features: Set<String>) {
        val enabled = mailbox != null && "groups" in features
        val current = state.value
        if (enabled && current.mailbox == mailbox && (current.enabled || current.unavailable)) {
            mutable.update { it.copy(disappearing = "disappearing" in features, features = features,
                receipts = if (Features.RECEIPTS in features) it.receipts else emptyMap()) }
            return
        }
        teardown()
        if (!enabled) return
        val authorization = try { scope.coroutineContext[AuthorizationContext]?.lease?.child() ?: AuthorizationLease() }
            catch (_: AuthorizationRetired) { return }
        actorAuthorization = authorization
        val run = ++generation
        mutable.value = GroupsState(mailbox = mailbox, disappearing = "disappearing" in features, features = features,
            handles = if (handle != null) mapOf(mailbox!! to handle) else emptyMap(), join = current.join)
        launch {
            val library = try { crypto() } catch (cancelled: CancellationException) { throw cancelled } catch (_: Throwable) { null }
            if (library == null) { if (run == generation) mutable.update { it.copy(unavailable = true) }; return@launch }
            val c = GroupClient(library, transport, storage, mailbox!!, devicePrefix, now, onEvent = { event -> if (run == generation) onClientEvent(event) }, authorizationLease = authorization)
            try { c.init() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (expired: SessionExpired) { if (run == generation) onSessionExpired(); return@launch }
            catch (failure: Exception) {
                if (run == generation) mutable.update { it.copy(notice = "Groups could not start on this device: " + explainGroupError(failure)) }
                return@launch
            }
            if (run != generation) return@launch
            client = c
            mutable.update { it.copy(enabled = true, deviceId = c.deviceId, fingerprint = c.fingerprint, persistent = c.persistent) }
            quietly { c.topUpKeyPackages(); lastTopUp = now() }
            quietly { c.processWelcomes() }
            refresh()
            state.value.join?.let { if (it.preview == null && it.error == null) resolveJoin(it.groupId, it.token) }
            if (active) startPolling()
        }
    }

    fun reset() {
        teardown()
        mutable.value = GroupsState()
    }

    private fun teardown() {
        actorAuthorization?.retire(); actorAuthorization = null
        client?.close()
        generation++
        pollJob?.cancel(); openJob?.cancel()
        client = null
        mutable.update { it.copy(enabled = false, unavailable = false, open = null, groups = emptyList(), history = emptyMap(),
            call = null, sending = false, drafts = emptyMap(), composers = emptyMap(), pendingIds = emptyMap(), receipts = emptyMap()) }
    }

    fun setActive(value: Boolean) {
        active = value
        if (!value) { pollJob?.cancel(); openJob?.cancel() } else if (client != null) { startPolling(); state.value.open?.let(::watchOpen) }
    }

    private fun startPolling() {
        pollJob?.cancel()
        val run = generation
        pollJob = launch {
            while (isActive && run == generation) {
                delay(pollSeconds * 1000)
                val c = client ?: break
                quietly { c.processWelcomes() }
                refresh()
                if (c.pruneExpired() > 0) snapshot()
                if (now() - lastTopUp > 300) { quietly { c.topUpKeyPackages() }; lastTopUp = now() }
            }
        }
    }

    private suspend fun <T> quietly(work: suspend () -> T): T? = try { work() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (expired: SessionExpired) { onSessionExpired(); null }
        catch (_: Exception) { null }

    private fun onClientEvent(event: GroupEvent) {
        when (event) {
            is GroupEvent.History -> snapshot()
            is GroupEvent.Removed, is GroupEvent.Resync -> { snapshot(); launch { refresh() } }
            is GroupEvent.Enrolled -> mutable.update { it.copy(deviceId = event.deviceId) }
        }
    }

    /** Copy what the client knows into the UI state. */
    private fun snapshot() {
        val c = client ?: return
        val ids = state.value.groups.map { it.id }.toSet() + listOfNotNull(state.value.open)
        mutable.update { old ->
            val history = ids.associateWith { c.history(it) }
            val pending = ids.associateWith { c.pendingIds(it) }
            val drafts = old.drafts.toMutableMap()
            val composers = old.composers.toMutableMap()
            if (!old.sending) for (gid in ids) {
                val resolved = old.history[gid].orEmpty().any { item -> item.pending && item.cmid != null && item.cmid !in pending[gid].orEmpty() &&
                    history[gid].orEmpty().any { it.cmid == item.cmid && it.kind == "message" && it.seq != null && !it.pending } }
                if (resolved) {
                    val action = composers.remove(gid)
                    drafts[gid] = if (action?.kind == GroupComposerKind.EDIT) action.ordinaryDraft.orEmpty() else ""
                }
            }
            old.copy(history = history, inGroup = ids.filter(c::inGroup).toSet(), drafts = drafts, composers = composers,
                pendingIds = pending,
                safety = ids.mapNotNull { id -> c.safetyNumber(id)?.let { id to it } }.toMap(), fingerprint = c.fingerprint)
        }
    }

    // ---- refreshing ----------------------------------------------------------------------------

    suspend fun refresh() {
        val c = client ?: return
        if (!refreshing.tryLock()) return
        try {
            val run = generation
            val groups = try {
                WireJson.decodeFromString<GroupsResponse>(c.transport.groupJson(c.mailbox, "GET", "/v1/groups?state=active,invited,keying")).groups.orEmpty()
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (expired: SessionExpired) { onSessionExpired(); return }
            catch (failure: ApiException) { if (failure.status == 404) mutable.update { it.copy(groups = emptyList()) }; return }
            catch (_: Exception) { return }
            if (run != generation) return
            mutable.update { it.copy(groups = groups.filter { g -> g.id.startsWith("grp_") }, open = it.open?.takeIf { id -> groups.any { g -> g.id == id && g.deletedAt == null } }) }
            snapshot()
            for (g in groups) {
                if (g.deletedAt != null) continue
                when {
                    g.state == "keying" && g.activationState == "provisioning" && g.createdBy == c.mailbox && g.owner -> quietly { c.activate(g.id) }
                    g.state == "active" -> {
                        if (c.inGroup(g.id) && (c.history(g.id).isEmpty() || g.lastSeq > (c.history(g.id).mapNotNull { it.seq }.maxOrNull() ?: 0))) quietly { c.sync(g.id) }
                        quietly { c.processPending(g.id) }
                    }
                }
            }
            snapshot()
        } finally { refreshing.unlock() }
    }

    // ---- the open group ------------------------------------------------------------------------

    /** Reconfirm live room membership before exposing its encrypted conversation cache. */
    suspend fun prepareOrganizationRoom(gid: String, organizationId: String, expectedContext: String): Boolean {
        val c = client ?: return false
        val operationContext = kotlinx.coroutines.currentCoroutineContext()
        val caller = operationContext[AuthorizationContext]?.lease ?: throw AuthorizationRetired()
        caller.requireActive()
        if (!caller.sharesAuthorization(c.authorizationLease)) throw AuthorizationRetired()
        fun current() = operationContext.isActive && client === c && c.authorizationLease.isValid && caller.isValid && state.value.contextId == expectedContext && state.value.mailbox == c.mailbox
        if (!current()) return false
        val api = dev.pigeonpost.core.enterprise.OrganizationRoomClient(c.transport)
        var group = api.group(c.mailbox, gid)
        if (!current() || group.orgId != organizationId) return false
        if (group.state == "keying" && group.activationState == "provisioning" && group.createdBy == c.mailbox && group.owner) {
            c.activate(gid)
            if (!current()) return false
            group = api.group(c.mailbox, gid)
            if (!current() || group.orgId != organizationId) return false
        }
        mutable.update { it.copy(groups = it.groups.filterNot { old -> old.id == gid } + group) }
        if (!current()) return false
        open(gid)
        return true
    }

    fun open(gid: String) {
        mutable.update { it.copy(open = gid, call = null) }
        snapshot()
        launch { loadMembers(gid) }
        watchOpen(gid)
    }

    fun close() {
        openJob?.cancel()
        mutable.update { it.copy(open = null, call = null) }
    }

    private fun watchOpen(gid: String) {
        openJob?.cancel()
        val run = generation
        openJob = launch {
            var tick = 0
            while (isActive && run == generation && state.value.open == gid) {
                val c = client
                if (c != null) {
                    if (c.inGroup(gid)) quietly { c.sync(gid) }
                    if (run != generation || state.value.open != gid) return@launch
                    quietly { c.reconcileDeletions(gid) }
                    if (tick % 5 == 0) {
                        quietly { c.processPending(gid, noJitter = tick == 0) }
                        val call = quietly { c.transport.groupCall(gid, c.mailbox) }
                        if (run == generation && state.value.open == gid) mutable.update { it.copy(call = call) }
                    }
                    markRead(gid)
                    if (run != generation || state.value.open != gid) return@launch
                    loadReceipts(gid)
                    snapshot()
                }
                tick++
                if (!active) break
                delay(openPollSeconds * 1000)
            }
        }
    }

    private suspend fun markRead(gid: String) {
        val c = client ?: return
        val g = state.value.groups.firstOrNull { it.id == gid } ?: return
        val mine = g.my ?: return
        val last = maxOf(c.history(gid).mapNotNull { it.seq }.maxOrNull() ?: 0, g.lastSeq)
        if (last <= mine.readCursor) return
        mutable.update { old -> old.copy(groups = old.groups.map { if (it.id == gid) it.copy(my = mine.copy(readCursor = last, unread = 0)) else it }) }
        quietly { c.transport.groupJson(c.mailbox, "POST", gpath(gid, "/read"), buildJsonObject { put("seq", last) }) }
    }

    suspend fun loadMembers(gid: String): List<GroupMember> {
        val c = client ?: return emptyList()
        val run = generation
        val list = quietly { WireJson.decodeFromString<MembersResponse>(c.transport.groupJson(c.mailbox, "GET", gpath(gid, "/members"))).members.orEmpty() }
            ?: return state.value.members[gid].orEmpty()
        if (run != generation) return emptyList()
        mutable.update { old -> old.copy(members = old.members + (gid to list), handles = old.handles + list.mapNotNull { m -> m.handle?.let { m.address to it } }) }
        return list
    }

    fun loadDevices(mailbox: String) {
        val c = client ?: return
        launch {
            val rows = quietly { WireJson.decodeFromString<DevicesResponse>(c.transport.groupJson(c.mailbox, "GET", "/v1/mls/devices?mailbox=" + enc(mailbox))).devices.orEmpty() } ?: return@launch
            c.authorizationLease.withActive { mutable.update { it.copy(devices = it.devices + (mailbox to rows)) } }
        }
    }

    /** "Jane", "You", or the address shortened. */
    fun nameOf(address: String?, contacts: Map<String, String> = emptyMap()): String {
        if (address == null) return "Someone"
        if (address == state.value.mailbox) return "You"
        val handle = state.value.handles[address]
        return contacts[handle ?: address] ?: contacts[address] ?: displayName(handle ?: address)
    }

    // ---- sending and files ---------------------------------------------------------------------

    fun draft(gid: String, text: String) {
        val current = state.value
        if (current.open != gid || current.sending || current.unconfirmed(gid)) return
        mutable.update { it.copy(drafts = it.drafts + (gid to text.take(64000))) }
    }
    fun beginReply(gid: String, seq: Long) = beginAction(gid, seq, GroupComposerKind.REPLY)
    fun beginEdit(gid: String, seq: Long) = beginAction(gid, seq, GroupComposerKind.EDIT)
    private fun beginAction(gid: String, seq: Long, kind: GroupComposerKind) {
        val current = state.value
        if (current.open != gid || current.sending || current.unconfirmed(gid) || current.current?.canPost != true) return
        if ((if (kind == GroupComposerKind.EDIT) Features.MESSAGE_EDIT else Features.MESSAGE_REPLY) !in current.features) return
        val history = current.history[gid].orEmpty()
        val item = history.firstOrNull { it.kind == "message" && it.seq == seq && !it.pending && !it.deleted && !it.undecryptable && it.text != null }
            ?.takeIf { (it.expiresAt ?: Long.MAX_VALUE) > now() } ?: return
        if (kind == GroupComposerKind.EDIT && !GroupActions.editable(item, history, current.mailbox ?: return, now())) return
        val previous = current.composers[gid]
        val ordinary = if (previous?.kind == GroupComposerKind.EDIT) previous.ordinaryDraft.orEmpty() else current.drafts[gid].orEmpty()
        val action = GroupComposerAction(kind, seq, item.sender, item.text.orEmpty().lineSequence().firstOrNull().orEmpty().take(200),
            ordinaryDraft = ordinary.takeIf { kind == GroupComposerKind.EDIT })
        mutable.update { it.copy(composers = it.composers + (gid to action),
            drafts = it.drafts + (gid to if (kind == GroupComposerKind.EDIT) item.text.orEmpty() else ordinary)) }
    }
    fun cancelAction(gid: String) {
        val current = state.value
        if (current.sending || current.unconfirmed(gid)) return
        val action = current.composers[gid] ?: return
        mutable.update { it.copy(composers = it.composers - gid,
            drafts = if (action.kind == GroupComposerKind.EDIT) it.drafts + (gid to action.ordinaryDraft.orEmpty()) else it.drafts) }
    }
    private fun finishComposer(gid: String, action: GroupComposerAction?) {
        if (state.value.composers[gid]?.id != action?.id) return
        mutable.update { it.copy(composers = it.composers - gid,
            drafts = it.drafts + (gid to if (action?.kind == GroupComposerKind.EDIT) action.ordinaryDraft.orEmpty() else "")) }
    }
    suspend fun loadReceipts(gid: String) {
        val c = client ?: return
        val run = generation
        if (Features.RECEIPTS !in state.value.features) return
        val receipts = quietly { c.receipts(gid) } ?: return
        if (run == generation && Features.RECEIPTS in state.value.features) mutable.update { it.copy(receipts = it.receipts + (gid to receipts)) }
    }

    fun send(gid: String, text: String, files: List<OutgoingFile>, done: (Boolean) -> Unit = {}) {
        sendPrepared(gid, text, { files }, done)
    }
    /** Capture text/action and claim single-flight before attachment IO or coroutine scheduling. */
    fun sendPrepared(gid: String, text: String, files: suspend () -> List<OutgoingFile>, done: (Boolean) -> Unit = {}) {
        val c = client ?: return
        val current = state.value
        val run = generation
        val action = current.composers[gid]
        if (current.sending || current.open != gid || current.current?.canPost != true || current.unconfirmed(gid)) return
        if (action != null && (if (action.kind == GroupComposerKind.EDIT) Features.MESSAGE_EDIT else Features.MESSAGE_REPLY) !in current.features) {
            note("That action is unavailable. Cancel it to keep writing."); return
        }
        mutable.update { it.copy(sending = true) }
        launch {
            val ok = try {
                val attachments = if (action?.kind == GroupComposerKind.EDIT) emptyList() else files()
                if (run != generation) return@launch
                if (text.isBlank() && attachments.isEmpty()) return@launch
                c.send(gid, OutgoingDocument(text.trim(), attachments,
                    replyTo = action?.takeIf { it.kind == GroupComposerKind.REPLY }?.seq,
                    editOf = action?.takeIf { it.kind == GroupComposerKind.EDIT }?.seq))
                if (run == generation) finishComposer(gid, action)
                true
            }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (expired: SessionExpired) { if (run == generation) onSessionExpired(); false }
            catch (failure: Exception) { if (run == generation) note("Could not confirm the send: " + explainGroupError(failure)); false }
            finally { if (run == generation) { mutable.update { it.copy(sending = false) }; snapshot() } }
            if (run == generation) done(ok)
        }
    }

    fun retry(gid: String, cmid: String, done: (Boolean) -> Unit = {}) {
        val c = client ?: return
        val run = generation
        val current = state.value
        if (current.sending || current.open != gid || current.current?.canPost != true || cmid !in current.pendingIds[gid].orEmpty()) return
        val action = current.composers[gid]
        mutable.update { it.copy(sending = true) }
        launch {
            val ok = try { c.retry(gid, cmid); if (run == generation) finishComposer(gid, action); true }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (expired: SessionExpired) { if (run == generation) onSessionExpired(); false }
            catch (failure: Exception) { if (run == generation) note(explainGroupError(failure)); false }
            finally { if (run == generation) { mutable.update { it.copy(sending = false) }; snapshot() } }
            if (run == generation) done(ok)
        }
    }
    fun discard(gid: String, cmid: String) = act { c ->
        c.discardPending(gid, cmid); snapshot(); cancelAction(gid)
    }

    suspend fun openFile(gid: String, file: GroupFile): ByteArray = (client ?: throw ApiException(403, "not_member")).openFile(gid, file)

    fun deleteMessage(gid: String, seq: Long) = act {
        val response = WireJson.parseToJsonElement(it.transport.groupJson(it.mailbox, "DELETE", gpath(gid, "/messages/$seq"))) as? JsonObject
        val seqs = (response?.get("seqs") as? kotlinx.serialization.json.JsonArray).orEmpty().mapNotNull { (it as? JsonPrimitive)?.content?.toLongOrNull() }
        it.dropSeqs(gid, seqs.ifEmpty { listOf(seq) }, tombstones = true)
    }

    // ---- creating, invitations, joining --------------------------------------------------------

    fun create(name: String, description: String?, invite: List<String>, joinPolicy: String, postPolicy: String, timer: Long?, done: (String?) -> Unit) {
        val c = client ?: return
        require(name.isNotBlank() && name.length <= 80) { "Give the group a name." }
        mutable.update { it.copy(busy = true) }
        launch {
            try {
                val settings = buildJsonObject {
                    put("name", name.trim()); description?.trim()?.takeIf { it.isNotEmpty() }?.let { put("description", it.take(500)) }
                    put("join_policy", joinPolicy); put("post_policy", postPolicy); put("visibility", "unlisted")
                    putJsonArray("invite") { invite.distinct().take(200).forEach { add(JsonPrimitive(it)) } }
                    if (timer != null && state.value.disappearing) put("ephemeral_secs", timer)
                }
                val created = c.createGroup(settings)
                val refused = created.invitations.orEmpty().count { it.status != "invited" }
                if (refused > 0) note("$refused ${if (refused == 1) "person" else "people"} could not be invited.")
                refresh()
                open(created.group.id)
                done(created.group.id)
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (expired: SessionExpired) { onSessionExpired(); done(null) }
            catch (failure: Exception) { note(explainGroupError(failure)); done(null) }
            finally { mutable.update { it.copy(busy = false) } }
        }
    }

    fun answerInvitation(g: Group, accept: Boolean) = act {
        it.transport.groupJson(it.mailbox, "POST", gpath(g.id, "/invitation"), buildJsonObject { put("accept", accept) })
        note(if (accept) "Joined ${g.name}. A member’s device will add you in a moment." else "Invitation declined.")
        refresh()
    }

    /** A `/g/<id>#token` link: show who and what before anything is joined. */
    fun resolveJoin(groupId: String, token: String?) {
        if (!Regex("grp_[a-z2-7]{26}").matches(groupId)) return
        mutable.update { it.copy(join = JoinRequest(groupId, token?.takeIf { t -> t.isNotBlank() && t.length <= 200 })) }
        val c = client ?: return // picked up once groups are ready
        launch {
            try {
                val preview = WireJson.decodeFromString<GroupPreview>(c.transport.groupJson(c.mailbox, "POST", "/v1/groups/resolve", buildJsonObject {
                    put("group_id", groupId); token?.let { put("link_token", it) }
                }))
                mutable.update { it.copy(join = it.join?.copy(preview = preview)) }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) {
                val code = (failure as? ApiException)?.code
                mutable.update { it.copy(join = it.join?.copy(error = if (code == "not_found" || code == "invalid_link") "This invite link is not valid any more." else explainGroupError(failure))) }
            }
        }
    }

    fun dismissJoin() = mutable.update { it.copy(join = null) }

    fun confirmJoin() {
        val c = client ?: return
        val request = state.value.join ?: return
        mutable.update { it.copy(join = request.copy(joining = true, error = null)) }
        launch {
            try {
                c.transport.groupJson(c.mailbox, "POST", gpath(request.groupId, "/join"), buildJsonObject { request.token?.let { put("link_token", it) } })
                mutable.update { it.copy(join = null) }
                note("Joined. A member’s device will add you to the encryption in a moment.")
                refresh()
                open(request.groupId)
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { mutable.update { it.copy(join = it.join?.copy(joining = false, error = explainGroupError(failure))) } }
        }
    }

    // ---- group info ----------------------------------------------------------------------------

    /**
     * Settings carry the group's revision, which also moves with every key change. Read it fresh
     * first; if somebody else's change still lands in between, reload once and reapply.
     */
    fun patch(gid: String, fields: Map<String, JsonElement>) = act { c ->
        for (attempt in 0 until 2) {
            try {
                val fresh = WireJson.decodeFromString<Group>(c.transport.groupJson(c.mailbox, "GET", gpath(gid)))
                c.transport.groupJson(c.mailbox, "PATCH", gpath(gid), JsonObject(fields + ("expected_revision" to JsonPrimitive(fresh.revision))))
                note("Saved.")
                break
            } catch (failure: ApiException) {
                if (failure.code == "revision_conflict" && attempt == 0) continue
                throw failure
            }
        }
        refresh()
    }

    fun setTimer(gid: String, seconds: Long?) = patch(gid, mapOf("ephemeral_secs" to (seconds?.let { JsonPrimitive(it) } ?: JsonNull)))

    fun changeMember(gid: String, member: GroupMember, role: String? = null, newState: String? = null) = act { c ->
        c.transport.groupJson(c.mailbox, "PATCH", gpath(gid, "/members/" + enc(member.address)), buildJsonObject {
            put("expected_revision", member.revision); role?.let { put("role", it) }; newState?.let { put("state", it) }
        })
        // A removal waits for a commit; make it now rather than at the next post.
        if (newState == "removed" || newState == "banned") quietly { c.processPending(gid, noJitter = true) }
        loadMembers(gid)
        refresh()
    }

    fun invite(gid: String, raw: String) = act { c ->
        val list = raw.split(Regex("[\\s,]+")).filter { it.isNotBlank() }.map { if (it.startsWith('/')) it else "/$it" }.filter { validAddress(it) }
        if (list.isEmpty()) { note("Enter an address such as /alp/main."); return@act }
        val res = WireJson.decodeFromString<InviteResults>(c.transport.groupJson(c.mailbox, "POST", gpath(gid, "/members"), buildJsonObject {
            put("request_key", "rk_" + java.util.UUID.randomUUID().toString().replace("-", "").take(20))
            putJsonArray("invite") { list.forEach { add(JsonPrimitive(it)) } }
        }))
        val ok = res.results.orEmpty().count { it.status == "invited" }
        note(if (ok == list.size) "Invited." else "$ok of ${list.size} invited.")
        loadMembers(gid)
    }

    /** Returns the link (shown once) or null when it was turned off. */
    fun link(gid: String, enabled: Boolean, done: (String?) -> Unit) = act { c ->
        val res = WireJson.decodeFromString<LinkResponse>(c.transport.groupJson(c.mailbox, "POST", gpath(gid, "/link"), buildJsonObject { put("enabled", enabled) }))
        done(res.url?.takeIf { it.startsWith("https://inbox.pigeonpost.dev/g/") || it.startsWith("http://127.0.0.1") })
        refresh()
    }

    fun transfer(gid: String, to: String) = act { c ->
        val fresh = WireJson.decodeFromString<Group>(c.transport.groupJson(c.mailbox, "GET", gpath(gid)))
        c.transport.groupJson(c.mailbox, "POST", gpath(gid, "/transfer"), buildJsonObject { put("to", to); put("expected_revision", fresh.revision) })
        note("Ownership transferred.")
        refresh()
    }

    fun leave(gid: String) = act { c ->
        c.transport.groupJson(c.mailbox, "POST", gpath(gid, "/leave"))
        c.forget(gid)
        close()
        refresh()
    }

    fun delete(gid: String) = act { c ->
        c.transport.groupJson(c.mailbox, "DELETE", gpath(gid))
        c.forget(gid)
        close()
        refresh()
    }

    fun setNotifications(gid: String, notify: String, muted: Boolean) = act { c ->
        val until = if (muted) 4102444800L else null
        c.transport.groupJson(c.mailbox, "PUT", gpath(gid, "/notifications"), buildJsonObject { put("notify", notify); put("muted_until", until?.let { JsonPrimitive(it) } ?: JsonNull) })
        mutable.update { old -> old.copy(groups = old.groups.map { g -> if (g.id == gid) g.copy(my = g.my?.copy(notify = notify, mutedUntil = until)) else g }) }
    }

    /** Remove this device's group keys from this phone and from the server's device list. */
    fun removeThisDevice(done: () -> Unit = {}) = act { c ->
        c.revoke()
        val mailbox = c.mailbox
        val handle = state.value.handles[mailbox]
        teardown()
        note("This device was removed from your groups. It will be added again, without history, when you open groups next.")
        done()
        onMailbox(mailbox, handle, setOf("groups") + if (state.value.disappearing) setOf("disappearing") else emptySet())
    }

    // ---- events from the account stream or a push ----------------------------------------------

    fun onEvent(type: String, data: JsonObject) {
        val c = client ?: return
        val gid = (data["group_id"] as? JsonPrimitive)?.content
        launch {
            when (type) {
                "group.message", "group.handshake" -> {
                    if (gid != null && (data["deleted"] as? JsonPrimitive)?.content == "true") {
                        val seqs = (data["seqs"] as? kotlinx.serialization.json.JsonArray).orEmpty().mapNotNull { (it as? JsonPrimitive)?.content?.toLongOrNull() }
                        val fallback = (data["seq"] as? JsonPrimitive)?.content?.toLongOrNull()
                        quietly { c.dropSeqs(gid, seqs.ifEmpty { listOfNotNull(fallback) }, tombstones = true) }
                    }
                    if (gid != null && c.inGroup(gid)) quietly { c.sync(gid) }
                    if (gid != null && state.value.groups.none { it.id == gid }) refresh()
                }
                "group.crypto_pending" -> if (gid != null) quietly { c.processPending(gid) }
                "group.member", "group.settings", "group.deleted" -> { if (gid != null) mutable.update { it.copy(members = it.members - gid) }; refresh() }
                "group.device" -> (data["mailbox"] as? JsonPrimitive)?.content?.let { m -> mutable.update { it.copy(devices = it.devices - m) } }
                "mls.welcome" -> { if (!quietly { c.processWelcomes() }.isNullOrEmpty()) refresh() }
                "mls.key_packages_low" -> quietly { c.topUpKeyPackages() }
                "message.expired" -> if (gid != null) {
                    val seqs = (data["seqs"] as? kotlinx.serialization.json.JsonArray).orEmpty().mapNotNull { (it as? JsonPrimitive)?.content?.toLongOrNull() }
                    quietly { c.dropSeqs(gid, seqs) }
                }
            }
            snapshot()
        }
    }

    /** A push for a group arrived while the app is in front: look now. */
    fun poke() { launch { client?.let { quietly { it.processWelcomes() } }; refresh() } }

    private fun act(work: suspend (GroupClient) -> Unit): Job {
        val c = client ?: return Job().apply { complete() }
        val run = generation
        mutable.update { it.copy(busy = true) }
        return launch {
            try { work(c) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (expired: SessionExpired) { if (run == generation) onSessionExpired() }
            catch (failure: Exception) { if (run == generation) note(explainGroupError(failure)) }
            finally { if (run == generation) { mutable.update { it.copy(busy = false) }; snapshot() } }
        }
    }

    private fun gpath(gid: String, rest: String = "") = "/v1/groups/" + enc(gid) + rest
    private fun enc(value: String) = java.net.URLEncoder.encode(value, "UTF-8")
}

/** The words for one notice in a group's history (the web inbox's `noticeText`). Null = not shown. */
fun groupNoticeText(item: HistoryItem, name: (String?) -> String): String? {
    val m = item.metadata ?: JsonObject(emptyMap())
    fun s(key: String) = (m[key] as? JsonPrimitive)?.content
    val actor = if (item.actor != null) name(item.actor) else "Someone"
    val who = s("address")?.let(name) ?: "someone"
    return when (item.notice) {
        "group_created" -> "$actor created the group"
        "member_invited" -> "$actor invited $who"
        "member_left" -> "$who left"
        "member_removed" -> "$actor removed $who"
        "member_banned" -> "$actor removed and banned $who"
        "role_changed" -> when (s("role")) { "admin" -> "$actor made $who an admin"; "restricted" -> "$actor restricted $who"; else -> "$who is now a member" }
        "settings_changed" -> {
            val changed = (m["changed"] as? kotlinx.serialization.json.JsonArray).orEmpty().mapNotNull { (it as? JsonPrimitive)?.content }
            when {
                "post_policy" in changed -> if (s("post_policy") == "admins") "$actor set posting to admins only" else "$actor let everyone post"
                "name" in changed -> "$actor renamed the group to “${s("name").orEmpty()}”"
                "join_policy" in changed -> if (s("join_policy") == "public") "$actor made the group public" else "$actor made the group invite only"
                else -> "$actor changed the group settings"
            }
        }
        "timer_changed" -> s("ephemeral_secs")?.toLongOrNull()?.let { "$actor set messages to disappear after ${timerLabel(it)}" } ?: "$actor turned off disappearing messages"
        "ownership_transferred" -> "$actor made $who the owner"
        "call_started" -> "$actor started a ${if (s("kind") == "video") "video" else "voice"} call"
        "call_ended" -> s("duration")?.toLongOrNull()?.takeIf { it > 0 }?.let { "Call ended · ${(it + 30) / 60} min" } ?: "Call ended"
        "link_reset" -> "$actor reset the invite link"
        "you_joined" -> "You joined this group. Earlier messages are not available on this device."
        "device_removed" -> "$who’s device was removed"
        else -> null // member_joined and device_added are shown from the verified commit instead
    }
}

/** A line drawn from a verified commit: who really joined, and which existing member added a new device. */
data class CommitLine(val text: String, val fingerprint: String? = null, val security: Boolean = false)

/**
 * The visible defence against a postbox minting devices for hosted mailboxes (P-2 §9, §12): a
 * device added for someone already in the group reads "Jane added a new device", with its fingerprint.
 */
fun commitLines(history: List<HistoryItem>, item: HistoryItem, me: String, myDevice: String?, name: (String?) -> String): List<CommitLine> {
    val knownBefore = mutableSetOf(me)
    for (y in history) {
        if (y === item) break
        if (y.kind == "commit") { y.committer?.let { knownBefore += it.mailbox }; y.added.forEach { knownBefore += it.mailbox } }
        if (y.kind == "message") y.sender?.let { knownBefore += it }
    }
    val out = mutableListOf<CommitLine>()
    item.added.groupBy { it.mailbox }.forEach { (mailbox, devices) ->
        if (mailbox in knownBefore || mailbox == item.committer?.mailbox) {
            devices.filterNot { mailbox == me && it.deviceId == myDevice }.forEach { out += CommitLine("${name(mailbox)} added a new device", it.fingerprint, security = true) }
        } else out += CommitLine("${name(mailbox)} joined")
    }
    return out
}

fun groupWarningText(warning: String?): String = when (warning) {
    "membership_mismatch" -> "Security warning: a membership change did not match the postbox’s record. Compare safety numbers in group info."
    "commit_rejected" -> "Security warning: this device refused an invalid group change."
    "sender_mismatch" -> "Security warning: a message’s sender did not match its device."
    "action_mismatch" -> "A message’s edit or reply reference could not be verified. Earlier messages were kept."
    "resynced" -> "This device lost the group’s keys and is being added again. Earlier messages stay unreadable here."
    else -> "Security warning."
}
