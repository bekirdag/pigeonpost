package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException
import java.util.concurrent.Executors

class OrganizationRoomStoreTest {
    private class Api(private val identity: () -> OrganizationRoomIdentity?) : OrganizationRoomApi {
        val actions = mutableListOf<String>()
        val payloads = mutableListOf<OrganizationRoomCreate>()
        var catalog = RoomOrganizations(listOf(RoomOrganization("org_fixture", "Example", "active", "member_fixture", settings = RoomOrganizationSettings("everyone"), mailbox = RoomMailbox("/k/member"))))
        var directory = listOf(OrganizationRoom("room_fixture", "Review", memberCount = 2, joinPolicy = "public"))
        var review = GroupPreview("room_fixture", "Review", joinPolicy = "public", canJoin = true)
        var result = Group("room_fixture", "Review", createdBy = "/k/member", my = GroupMine(role = "member", state = "keying"), orgId = "org_fixture")
        var hook: suspend (String) -> Unit = {}
        private suspend fun record(action: String, mailbox: String) {
            val request = currentCoroutineContext()[AuthorizationContext]?.lease ?: throw AuthorizationRetired()
            request.requireActive(); val source = identity() ?: throw AuthorizationRetired()
            check(source.mailbox == mailbox && request.sharesAuthorization(source.authorization)); actions += action
        }
        override suspend fun organizations(mailbox: String): RoomOrganizations { record("catalog", mailbox); val frozen = catalog; hook("catalog"); return frozen }
        override suspend fun rooms(mailbox: String, organizationId: String, search: String): List<OrganizationRoom> { record("rooms:$search", mailbox); val frozen = directory; hook("rooms:$search"); return frozen }
        override suspend fun preview(mailbox: String, groupId: String): GroupPreview { record("preview", mailbox); val frozen = review; hook("preview"); return frozen }
        override suspend fun group(mailbox: String, groupId: String): Group { record("group", mailbox); val frozen = result; hook("group"); return frozen }
        override suspend fun create(mailbox: String, payload: OrganizationRoomCreate): Group {
            record("create", mailbox); payloads += payload
            val frozen = result.copy(createdBy = mailbox, my = GroupMine(role = "owner", state = "keying"))
            hook("create"); return frozen
        }
        override suspend fun join(mailbox: String, groupId: String) { record("join", mailbox); hook("join") }
        override suspend fun acceptInvitation(mailbox: String, groupId: String) { record("accept", mailbox); hook("accept") }
    }
    private class Fixture(val scope: CoroutineScope) {
        var source: OrganizationRoomIdentity? = OrganizationRoomIdentity("person", AuthorizationLease(), "/k/member")
        var clock = 1_800_000_000L
        val api = Api { source }
        val expired = mutableListOf<AuthorizationLease>()
        val store = OrganizationRoomStore(api, scope, { source }, { clock }, expired::add)
        val state get() = store.state.value
        val context get() = state.contextId
        suspend fun load() { store.loadOrganizations(context)!!.join(); store.state.first { !it.loading } }
        suspend fun inspect() { store.inspect("room_fixture", state.directoryProof, context)!!.join() }
        fun create(name: String = "Design") = store.create(context, "org_fixture", name, "Notes", RoomJoinPolicy.MEMBERS, RoomPostPolicy.EVERYONE, true)
        fun replace(subject: String = "person", mailbox: String = "/k/member") {
            source?.authorization?.retire(); source = OrganizationRoomIdentity(subject, AuthorizationLease(), mailbox); store.synchronize()
        }
    }
    private suspend fun world(work: suspend Fixture.() -> Unit) {
        val dispatcher = Executors.newSingleThreadExecutor { Thread(it, "room-state-test").apply { isDaemon = true } }.asCoroutineDispatcher()
        val scope = CoroutineScope(SupervisorJob() + dispatcher); val fixture = Fixture(scope)
        try { withTimeout(8_000) { withContext(dispatcher) { fixture.store.synchronize(); fixture.work() } } }
        finally { withContext(dispatcher) { fixture.store.reset(); fixture.source?.authorization?.retire() }; scope.cancel(); dispatcher.close() }
    }

    @Test fun catalogAndPreviewAreReadOnlyUntilTheReviewedAction() = runBlocking<Unit> { world {
        load(); inspect(); assertEquals(listOf("catalog", "rooms:", "preview"), api.actions)
        assertEquals(RoomAction.JOIN, store.reviewAction(state.reviewProof, context)); assertNull(state.opening)
        store.openReviewed(state.reviewProof, context)!!.join()
        assertEquals(listOf("catalog", "rooms:", "preview", "join", "group"), api.actions)
        assertTrue(store.allows(state.opening!!))
    } }
    @Test fun missingOrUnknownMembershipCannotOpenACachedPrivateConversation() = runBlocking<Unit> { world {
        load()
        for (membership in listOf(null, "future")) {
            api.review = api.review.copy(joinPolicy = "private", myState = membership, canJoin = false)
            inspect(); assertEquals(RoomAction.UNAVAILABLE, store.reviewAction(state.reviewProof, context)); assertNull(store.openReviewed(state.reviewProof, context))
        }
        assertFalse(api.actions.any { it in setOf("group", "join", "accept") })
    } }
    @Test fun invitationsAndAlreadyJoinedRoomsUseTheirDistinctExplicitActions() = runBlocking<Unit> { world {
        load(); api.review = api.review.copy(myState = "invited"); inspect()
        assertEquals(RoomAction.ACCEPT_INVITATION, store.reviewAction(state.reviewProof, context))
        store.openReviewed(state.reviewProof, context)!!.join(); assertEquals(1, api.actions.count { it == "accept" })
        api.review = api.review.copy(myState = "active"); inspect()
        store.openReviewed(state.reviewProof, context)!!.join(); assertEquals(2, api.actions.count { it == "group" }); assertFalse("join" in api.actions)
    } }
    @Test fun readonlyMembershipCanOpenButCannotJoinOrCreate() = runBlocking<Unit> { world {
        api.catalog = RoomOrganizations(api.catalog.orgs.map { it.copy(status = "read_only") }); load(); inspect()
        assertFalse(store.canCreate(context)); assertNull(create()); assertEquals(RoomAction.UNAVAILABLE, store.reviewAction(state.reviewProof, context))
        api.review = api.review.copy(myState = "active"); inspect(); store.openReviewed(state.reviewProof, context)!!.join()
        assertTrue(store.allows(state.opening!!)); assertFalse("join" in api.actions)
    } }
    @Test fun finalMembershipAndOrganizationAreConfirmedBeforeOpening() = runBlocking<Unit> { world {
        load(); api.review = api.review.copy(myState = "active")
        val original = api.result
        for (group in listOf(original.copy(id = "room_other"), original.copy(orgId = "org_other"), original.copy(orgId = null), original.copy(my = null),
            original.copy(my = GroupMine(state = "invited")), original.copy(my = GroupMine(role = "future")), original.copy(deletedAt = 1))) {
            api.result = group; inspect(); store.openReviewed(state.reviewProof, context)!!.join(); assertNull(state.opening); assertTrue(state.mustReviewAgain)
        }
    } }
    @Test fun anUnconfirmedJoinRequiresAFreshPreviewAndNeverBlindlyReplays() = runBlocking<Unit> { world {
        load(); inspect(); val proof = state.reviewProof
        api.hook = { if (it == "join") throw IOException("Fixture accepted response loss") }
        store.openReviewed(proof, context)!!.join(); assertTrue(state.mustReviewAgain); assertNull(store.openReviewed(proof, context))
        assertEquals(1, api.actions.count { it == "join" })
        api.hook = {}; api.review = api.review.copy(myState = "keying"); inspect()
        store.openReviewed(state.reviewProof, context)!!.join(); assertTrue(store.allows(state.opening!!)); assertEquals(1, api.actions.count { it == "join" })
    } }
    @Test fun alreadyMemberStillNeedsTheFreshOrganizationAndMembershipRead() = runBlocking<Unit> { world {
        load(); inspect(); api.hook = { if (it == "join") throw ApiException(409, "already_member") }
        store.openReviewed(state.reviewProof, context)!!.join(); assertEquals(1, api.actions.count { it == "group" }); assertTrue(store.allows(state.opening!!))
    } }
    @Test fun uncertainCreationFreezesTheExactIntentUntilAnExplicitRetryOrDiscard() = runBlocking<Unit> { world {
        load(); api.hook = { if (it == "create") throw IOException("Fixture accepted response loss") }
        create()!!.join(); val pending = state.pendingCreation!!
        assertFalse(store.close()); assertNull(create("Changed")); assertEquals(1, api.payloads.size)
        api.hook = {}; create()!!.join()
        assertEquals(2, api.payloads.size); assertSame(pending, api.payloads[1]); assertEquals(api.payloads[0].encoded, api.payloads[1].encoded)
        assertNull(state.pendingCreation); assertNotNull(state.completedCreation); assertTrue(store.allows(state.opening!!))
        assertNull(create()); assertEquals(2, api.payloads.size)
    } }
    @Test fun aKnownCreatedRoomCanBeCheckedWithoutAnotherCreationPost() = runBlocking<Unit> { world {
        load(); api.hook = { if (it == "group") throw IOException("Fixture read unavailable") }
        create()!!.join(); assertNotNull(state.pendingCreation); assertEquals("room_fixture", state.createdCandidateId)
        api.hook = {}; store.checkCreated(context)!!.join()
        assertEquals(1, api.payloads.size); assertNull(state.pendingCreation); assertTrue(store.allows(state.opening!!))
        store.openingFailed(state.opening!!); assertNull(state.opening); assertNotNull(state.completedCreation)
        store.checkCreated(context)!!.join(); assertTrue(store.allows(state.opening!!)); assertEquals(1, api.payloads.size)
    } }
    @Test fun oldDirectoryProofAndContextCannotClearOrDecideTheNewReview() = runBlocking<Unit> { world {
        load(); val oldDirectory = state.directoryProof; store.loadRooms("new", context)!!.join()
        assertNull(store.inspect("room_fixture", oldDirectory, context)); inspect(); val oldContext = context
        replace(); load(); inspect(); val preview = state.preview; val proof = state.reviewProof
        assertNull(store.loadRooms("old", oldContext)); assertSame(preview, state.preview); assertEquals(proof, state.reviewProof)
        assertNull(store.openReviewed(proof, oldContext)); assertFalse("join" in api.actions)
    } }
    @Test fun aChangedIdentityImmediatelyInvalidatesAnOpeningBeforeSynchronization() = runBlocking<Unit> { world {
        load(); inspect(); store.openReviewed(state.reviewProof, context)!!.join(); val opening = state.opening!!
        source = OrganizationRoomIdentity("replacement", AuthorizationLease(), "/k/member")
        assertFalse(store.allows(opening)); assertFalse(store.isCurrent()); assertNull(store.openReviewed(state.reviewProof, context))
        assertNull(state.catalog); assertNull(state.opening)
    } }
    @Test fun aLateCreationCannotReadOrOpenAnythingUnderTheReplacementAccount() = runBlocking<Unit> { world {
        load(); val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        api.hook = { if (it == "create") withContext(NonCancellable) { entered.complete(Unit); release.await() } }
        val old = create()!!; entered.await(); val oldContext = context
        replace("replacement"); api.hook = {}; load(); release.complete(Unit); old.join()
        assertEquals(0, api.actions.count { it == "group" }); assertNull(state.opening); assertFalse(state.busy)
        assertFalse(store.discardCreation(oldContext, "org_fixture")); assertNull(state.pendingCreation)
    } }
    @Test fun anOldDirectoryReadCannotReplaceNewResultsOrClearItsLoading() = runBlocking<Unit> { world {
        load(); val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>(); val nextEntered = CompletableDeferred<Unit>(); val nextRelease = CompletableDeferred<Unit>()
        api.hook = {
            if (it == "rooms:old") withContext(NonCancellable) { entered.complete(Unit); release.await() }
            if (it == "rooms:new") { nextEntered.complete(Unit); nextRelease.await() }
        }
        val old = store.loadRooms("old", context)!!; entered.await()
        api.directory = api.directory.map { it.copy(name = "New result") }; val next = store.loadRooms("new", context)!!; nextEntered.await()
        release.complete(Unit); old.join(); assertTrue(state.loadingRooms)
        nextRelease.complete(Unit); next.join(); assertEquals("New result", state.rooms.single().name); assertFalse(state.loadingRooms)
    } }
    @Test fun membershipDenialClearsPrivateMetadataAndUncertainCreation() = runBlocking<Unit> { world {
        load(); api.hook = { if (it == "create") throw IOException("Fixture response loss") }; create()!!.join(); assertNotNull(state.pendingCreation)
        api.hook = { if (it.startsWith("rooms:")) throw ApiException(403, "not_org_member") }
        store.loadRooms("", context)!!.join(); assertNull(state.catalog); assertTrue(state.rooms.isEmpty()); assertNull(state.preview); assertNull(state.pendingCreation)
    } }
    @Test fun accountBackoffSurvivesMailboxChangesAndExplicitDiscard() = runBlocking<Unit> { world {
        load(); api.hook = { if (it == "create") throw ApiException(429, "rate_limited", retryAfter = 60) }; create()!!.join()
        val deadline = state.retryAt; assertEquals(clock + 60, deadline)
        assertTrue(store.discardCreation(context, "org_fixture")); val count = api.actions.size
        replace(mailbox = "/k/another"); assertEquals(deadline, state.retryAt); assertNull(store.loadOrganizations(context)); assertEquals(count, api.actions.size)
        replace("another-person"); assertNull(state.retryAt)
    } }
    @Test fun doubleSubmitAndOrganizationChangesAreRefusedWhileTheCreationIsInFlight() = runBlocking<Unit> { world {
        api.catalog = RoomOrganizations(api.catalog.orgs + api.catalog.orgs.single().copy(id = "org_second", memberId = "member_second"))
        load(); assertTrue(store.selectOrganization("org_fixture", context))
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        api.hook = { if (it == "create") { entered.complete(Unit); release.await() } }
        val first = create()!!; entered.await()
        assertNull(create()); assertFalse(store.selectOrganization("org_second", context)); assertFalse(store.close(discardUnconfirmed = true))
        release.complete(Unit); first.join(); assertEquals(1, api.payloads.size); assertTrue(store.allows(state.opening!!))
    } }
    @Test fun anUnsentDraftSurvivesDirectoryRefreshAndRejectsOldEditorsAfterDiscard() = runBlocking<Unit> { world {
        load(); assertTrue(store.beginCreation(context)); val draft = state.draft!!
        store.changeDraft(draft.copy(name = "Keep this draft", description = "Private notes"), context)
        load(); assertEquals("Keep this draft", state.draft!!.name); assertEquals(draft.id, state.draft!!.id)
        assertTrue(store.beginCreation(context)); assertEquals(draft.id, state.draft!!.id)
        assertTrue(store.discardCreation(context, "org_fixture")); assertTrue(store.beginCreation(context)); val replacement = state.draft!!
        store.changeDraft(draft.copy(name = "Stale"), context); assertEquals(replacement, state.draft); assertNull(store.submitDraft(draft.id, context))
        assertFalse("create" in api.actions)
    } }
    @Test fun aFrozenCreationDraftCannotBeEditedOrSubmittedUnderAnotherContext() = runBlocking<Unit> { world {
        load(); store.beginCreation(context); val draft = state.draft!!.copy(name = "Frozen intent")
        store.changeDraft(draft, context); api.hook = { if (it == "create") throw IOException("Fixture accepted response loss") }
        store.submitDraft(draft.id, context)!!.join(); val pending = state.pendingCreation!!
        store.changeDraft(draft.copy(name = "Changed"), context); assertEquals("Frozen intent", state.draft!!.name); assertSame(pending, state.pendingCreation)
        val oldContext = context; replace(); load(); store.beginCreation(context)
        assertNull(store.submitDraft(draft.id, oldContext)); assertEquals(1, api.payloads.size)
    } }
    @Test fun revocationAtTheFinalEncryptedInboxCheckForgetsPrivateRoomMetadata() = runBlocking<Unit> { world {
        load(); inspect(); store.openReviewed(state.reviewProof, context)!!.join(); val opening = state.opening!!
        store.openingFailed(opening, ApiException(403, "not_org_member"))
        assertNull(state.catalog); assertNull(state.preview); assertNull(state.opening); assertTrue(state.rooms.isEmpty())
        assertFalse(store.allows(opening)); assertNull(store.openReviewed(state.reviewProof, context))
    } }
}
