package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.Group
import dev.pigeonpost.core.groups.GroupPreview
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.util.UUID

class OrganizationRoomIdentity(val subject: String, val authorization: AuthorizationLease, val mailbox: String) {
    fun same(other: OrganizationRoomIdentity?) = other != null && subject == other.subject && authorization === other.authorization && mailbox == other.mailbox
    override fun toString() = "OrganizationRoomIdentity(redacted)"
}
class OrganizationRoomOpening internal constructor(val contextId: String, val organizationId: String, val groupId: String) {
    override fun toString() = "OrganizationRoomOpening(redacted)"
}
data class OrganizationRoomsState(
    val contextId: String = UUID.randomUUID().toString(), val mailbox: String? = null,
    val catalog: RoomOrganizations? = null, val selectedOrganizationId: String? = null,
    val rooms: List<OrganizationRoom> = emptyList(), val directoryProof: String = UUID.randomUUID().toString(),
    val preview: GroupPreview? = null, val reviewProof: String = UUID.randomUUID().toString(), val mustReviewAgain: Boolean = false,
    val pendingCreation: OrganizationRoomCreate? = null, val createdCandidateId: String? = null, val completedCreation: Group? = null,
    val opening: OrganizationRoomOpening? = null,
    val loadingCatalog: Boolean = false, val loadingRooms: Boolean = false, val reviewing: Boolean = false, val busy: Boolean = false,
    val message: String? = null, val retryAt: Long? = null,
) {
    val organization get() = catalog?.forMailbox(mailbox.orEmpty())?.firstOrNull { it.id == selectedOrganizationId }
    val loading get() = loadingCatalog || loadingRooms || reviewing
}

/** Each directory, preview, decision and creation belongs to one human grant and canonical inbox. */
class OrganizationRoomStore(
    private val api: OrganizationRoomApi, private val scope: CoroutineScope,
    private val identity: () -> OrganizationRoomIdentity?, private val now: () -> Long = { System.currentTimeMillis() / 1000 },
    private val onSessionExpired: (AuthorizationLease) -> Unit = {},
) {
    private enum class Lane { CATALOG, ROOMS, REVIEW, MUTATION }
    private class Work(val lane: Lane, val actor: AuthorizationLease, val request: AuthorizationLease, val organization: String?) { var job: Job? = null }
    private val mutable = MutableStateFlow(OrganizationRoomsState())
    val state = mutable.asStateFlow()
    private var bound: OrganizationRoomIdentity? = null
    private var actor: AuthorizationLease? = null
    private val works = mutableMapOf<Lane, Work>()
    private var rateSubject: String? = null
    private var deadline: Long? = null
    private fun live() = identity()?.takeIf { it.subject.isNotEmpty() && it.subject.toByteArray().size <= 256 && it.authorization.isValid && OrganizationRoomInput.mailbox(it.mailbox) }
    fun isCurrent(context: String = state.value.contextId) = context == state.value.contextId && actor?.isValid == true && bound?.same(live()) == true
    private fun cancel(lane: Lane) { works.remove(lane)?.let { it.request.retire(); it.job?.cancel() } }
    private fun bind(next: OrganizationRoomIdentity?, preserveRate: Boolean = false) {
        Lane.entries.forEach(::cancel); actor?.retire(); actor = null; bound = null
        if (!preserveRate && next?.subject != rateSubject) { rateSubject = null; deadline = null }
        val child = try { next?.authorization?.child() } catch (_: AuthorizationRetired) { null }
        if (child != null) { actor = child; bound = next }
        mutable.value = OrganizationRoomsState(contextId = child?.id ?: UUID.randomUUID().toString(), mailbox = if (child != null) next?.mailbox else null, retryAt = deadline)
    }
    fun synchronize() { val next = live(); if (bound?.same(next) != true || actor?.isValid != true) bind(next) }
    /** The UI must explicitly acknowledge abandoning an uncertain creation before closing it. */
    fun close(discardUnconfirmed: Boolean = false): Boolean {
        if (state.value.busy || state.value.pendingCreation != null && !discardUnconfirmed) return false
        bind(null, preserveRate = live()?.subject == rateSubject); return true
    }
    fun reset() { bind(null, preserveRate = live()?.subject == rateSubject) }
    fun canCreate(context: String): Boolean = isCurrent(context) && !state.value.busy && state.value.organization?.mayCreate(state.value.mailbox.orEmpty()) == true
    fun allows(opening: OrganizationRoomOpening): Boolean = isCurrent(opening.contextId) && state.value.opening === opening &&
        state.value.organization?.id == opening.organizationId && state.value.organization?.readable == true
    private fun current(work: Work) = works[work.lane] === work && actor === work.actor && isCurrent(work.actor.id) &&
        (work.organization == null || state.value.organization?.let { it.id == work.organization && it.readable } == true)
    private fun limited(): Boolean {
        if (deadline?.let { it > now() } != true) { deadline = null; mutable.update { it.copy(retryAt = null) }; return false }
        mutable.update { it.copy(message = "Wait before retrying this request.", retryAt = deadline) }; return true
    }
    private fun flags(lane: Lane, value: Boolean) { mutable.update { when (lane) {
        Lane.CATALOG -> it.copy(loadingCatalog = value)
        Lane.ROOMS -> it.copy(loadingRooms = value)
        Lane.REVIEW -> it.copy(reviewing = value)
        Lane.MUTATION -> it.copy(busy = value)
    } } }
    private fun run(lane: Lane, context: String, org: String? = null, action: suspend (Work) -> Unit): Job? {
        synchronize()
        val owner = actor ?: return null
        if (!isCurrent(context) || state.value.busy || limited() || org != null && state.value.organization?.id != org) return null
        cancel(lane)
        val request = try { owner.child() } catch (_: AuthorizationRetired) { synchronize(); return null }
        val work = Work(lane, owner, request, org); works[lane] = work
        flags(lane, true); mutable.update { it.copy(message = null) }
        val job = scope.launch(AuthorizationContext(request), start = CoroutineStart.LAZY) {
            try { request.requireActive(); action(work) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { if (current(work)) failed(failure, lane == Lane.MUTATION) }
            finally {
                request.retire()
                if (works[lane] === work) { works.remove(lane); if (actor === owner && isCurrent(owner.id)) flags(lane, false) }
            }
        }
        work.job = job; job.start(); return job
    }
    private fun clearReview() {
        cancel(Lane.REVIEW)
        mutable.update { it.copy(preview = null, reviewProof = UUID.randomUUID().toString(), mustReviewAgain = false, reviewing = false, opening = null) }
    }
    fun closeReview(context: String) { if (isCurrent(context) && !state.value.busy) clearReview() }
    fun selectOrganization(id: String, context: String): Boolean {
        synchronize()
        if (!isCurrent(context) || state.value.busy || state.value.catalog?.forMailbox(state.value.mailbox.orEmpty())?.any { it.id == id } != true) return false
        if (state.value.selectedOrganizationId == id) return true
        if (state.value.pendingCreation != null) { mutable.update { it.copy(message = "Check or discard the earlier creation before changing organizations.") }; return false }
        cancel(Lane.ROOMS); clearReview()
        mutable.update { it.copy(selectedOrganizationId = id, rooms = emptyList(), directoryProof = UUID.randomUUID().toString(), loadingRooms = false,
            completedCreation = null, createdCandidateId = null, message = null) }
        return true
    }
    fun loadOrganizations(context: String = state.value.contextId, search: String = ""): Job? = run(Lane.CATALOG, context) { work ->
        val catalog = api.organizations(bound!!.mailbox)
        if (!current(work)) return@run
        if (!catalog.valid) throw ApiException(200, "bad_response")
        cancel(Lane.ROOMS); clearReview()
        val matches = catalog.forMailbox(bound!!.mailbox)
        val selected = state.value.selectedOrganizationId?.takeIf { id -> matches.any { it.id == id } } ?: matches.singleOrNull()?.id
        val keep = state.value.pendingCreation != null && state.value.pendingCreation?.organizationId == selected
        mutable.update { it.copy(catalog = catalog, selectedOrganizationId = selected, rooms = emptyList(), directoryProof = UUID.randomUUID().toString(), loadingRooms = false,
            pendingCreation = it.pendingCreation.takeIf { keep }, createdCandidateId = it.createdCandidateId.takeIf { keep },
            completedCreation = it.completedCreation?.takeIf { group -> group.orgId == selected }) }
        if (selected != null) loadRooms(search, context)
    }
    fun loadRooms(search: String, context: String = state.value.contextId): Job? {
        synchronize()
        if (!isCurrent(context)) return null
        val org = state.value.organization ?: return null
        if (!org.readable || state.value.busy) return null
        clearReview()
        mutable.update { it.copy(directoryProof = UUID.randomUUID().toString()) }
        return run(Lane.ROOMS, context, org.id) { work ->
            val rooms = api.rooms(bound!!.mailbox, org.id, search)
            if (current(work)) {
                if (rooms.size > 50 || rooms.any { !it.valid } || rooms.map { it.id }.distinct().size != rooms.size) throw ApiException(200, "bad_response")
                mutable.update { it.copy(rooms = rooms, directoryProof = UUID.randomUUID().toString()) }
            }
        }
    }
    fun inspect(id: String, proof: String, context: String): Job? {
        synchronize()
        val org = state.value.organization ?: return null
        if (!isCurrent(context) || state.value.busy || state.value.directoryProof != proof || state.value.rooms.none { it.id == id }) return null
        clearReview()
        return run(Lane.REVIEW, context, org.id) { work ->
            val preview = api.preview(bound!!.mailbox, id)
            if (current(work)) {
                if (preview.id != id) throw ApiException(200, "bad_response")
                mutable.update { it.copy(preview = preview, reviewProof = UUID.randomUUID().toString(), mustReviewAgain = false) }
            }
        }
    }
    fun reviewAction(proof: String, context: String): RoomAction {
        if (!isCurrent(context) || state.value.reviewProof != proof || state.value.busy || state.value.mustReviewAgain || state.value.organization?.readable != true) return RoomAction.UNAVAILABLE
        val preview = state.value.preview ?: return RoomAction.UNAVAILABLE
        val action = when (preview.myState) {
            "active", "keying" -> RoomAction.OPEN
            "invited" -> RoomAction.ACCEPT_INVITATION
            null, "left", "removed" -> if (preview.joinPolicy == "public" && preview.canJoin) RoomAction.JOIN else RoomAction.UNAVAILABLE
            else -> RoomAction.UNAVAILABLE
        }
        return if (action == RoomAction.OPEN || state.value.organization?.writable == true) action else RoomAction.UNAVAILABLE
    }
    private fun confirm(group: Group, id: String, org: String) {
        if (group.id != id || group.orgId != org || group.deletedAt != null || group.my?.state !in setOf("active", "keying") ||
            group.my?.role !in setOf("owner", "admin", "member", "restricted")) throw ApiException(200, "bad_response")
    }
    private fun opened(group: Group, org: String) {
        mutable.update { it.copy(opening = OrganizationRoomOpening(it.contextId, org, group.id), message = null) }
    }
    fun openReviewed(proof: String, context: String): Job? {
        synchronize()
        val action = reviewAction(proof, context)
        if (action == RoomAction.UNAVAILABLE) return null
        val org = state.value.organization!!; val id = state.value.preview!!.id; val mailbox = bound!!.mailbox
        return run(Lane.MUTATION, context, org.id) { work ->
            try {
                if (action == RoomAction.JOIN) api.join(mailbox, id)
                if (action == RoomAction.ACCEPT_INVITATION) api.acceptInvitation(mailbox, id)
            } catch (failure: ApiException) { if (failure.code != "already_member") throw failure }
            if (!current(work)) return@run
            val group = api.group(mailbox, id)
            if (current(work) && state.value.reviewProof == proof) { confirm(group, id, org.id); opened(group, org.id) }
        }
    }
    fun beginCreation(context: String): Boolean {
        if (!canCreate(context)) return false
        if (state.value.pendingCreation == null) mutable.update { it.copy(completedCreation = null, createdCandidateId = null, opening = null, message = null) }
        return true
    }
    fun create(context: String, organizationId: String, name: String, description: String, join: RoomJoinPolicy, post: RoomPostPolicy, listed: Boolean): Job? {
        synchronize()
        if (!canCreate(context) || state.value.organization?.id != organizationId || limited()) return null
        if (state.value.completedCreation != null) { mutable.update { it.copy(message = "This room was created. Open it or start another room.") }; return null }
        val previous = state.value.pendingCreation
        val payload = try { OrganizationRoomCreate(organizationId, name, description, join, post, listed, previous?.requestKey ?: "rk_" + UUID.randomUUID()) }
        catch (_: IllegalArgumentException) { mutable.update { it.copy(message = "Use a name of 1–80 characters and a description of at most 500 characters.") }; return null }
        if (previous != null && payload.encoded != previous.encoded) { mutable.update { it.copy(message = "Check or discard the earlier creation before changing its details.") }; return null }
        val mailbox = bound!!.mailbox
        return run(Lane.MUTATION, context, organizationId) { work ->
            mutable.update { it.copy(pendingCreation = previous ?: payload) }
            val id = state.value.createdCandidateId ?: api.create(mailbox, previous ?: payload).let { created ->
                if (current(work)) {
                    confirm(created, created.id, organizationId)
                    if (created.createdBy != mailbox || created.my?.role != "owner") throw ApiException(200, "bad_response")
                    mutable.update { it.copy(createdCandidateId = created.id) }
                }
                created.id
            }
            if (!current(work)) return@run
            val group = api.group(mailbox, id)
            if (current(work)) {
                confirm(group, id, organizationId)
                mutable.update { it.copy(pendingCreation = null, completedCreation = group) }
                opened(group, organizationId)
            }
        }
    }
    fun checkCreated(context: String): Job? {
        synchronize()
        if (!isCurrent(context)) return null
        val org = state.value.organization ?: return null
        val id = state.value.createdCandidateId ?: state.value.completedCreation?.id ?: return null
        val mailbox = bound!!.mailbox
        return run(Lane.MUTATION, context, org.id) { work ->
            val group = api.group(mailbox, id)
            if (current(work)) {
                confirm(group, id, org.id); mutable.update { it.copy(pendingCreation = null, completedCreation = group) }; opened(group, org.id)
            }
        }
    }
    fun discardCreation(context: String, organizationId: String): Boolean {
        if (!isCurrent(context) || state.value.busy || state.value.organization?.id != organizationId) return false
        mutable.update { it.copy(pendingCreation = null, createdCandidateId = null, completedCreation = null, opening = null, message = null) }; return true
    }
    fun openingFailed(opening: OrganizationRoomOpening) {
        if (allows(opening)) mutable.update { it.copy(opening = null, message = "The room is confirmed, but its encrypted inbox is not ready. Open it again to retry.") }
    }
    fun openingCompleted(opening: OrganizationRoomOpening) { if (allows(opening)) mutable.update { it.copy(opening = null) } }
    private fun failed(failure: Exception, decision: Boolean) {
        val api = failure as? ApiException
        if (failure is SessionExpired) { val root = bound?.authorization; bind(null); root?.let(onSessionExpired); return }
        if (api?.status in setOf(401, 403, 404, 410)) {
            cancel(Lane.ROOMS); clearReview()
            mutable.update { it.copy(catalog = null, selectedOrganizationId = null, rooms = emptyList(), loadingRooms = false,
                pendingCreation = null, createdCandidateId = null, completedCreation = null, message = "Access could not be confirmed. Refresh your organizations.") }; return
        }
        if (api?.status == 429) { rateSubject = bound?.subject; deadline = now() + (api.retryAfter ?: 30).coerceIn(1, 3600) }
        mutable.update { it.copy(mustReviewAgain = decision && it.preview != null, opening = null, retryAt = deadline, message = when {
            api?.status == 429 -> "Too many requests. Wait before trying again."
            it.pendingCreation != null -> "Creation is not confirmed. Retry the same details or check your existing rooms."
            decision -> "The result is not confirmed. Review the room again before deciding."
            else -> "The rooms could not be confirmed. Refresh and try again."
        }) }
    }
}
