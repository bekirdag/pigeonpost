package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException
import java.util.concurrent.Executors

class DeviceApprovalStoreTest {
    private class Api(private val identity: () -> DeviceApprovalIdentity?) : DevicePairingApi {
        val actions = mutableListOf<String>()
        val roots = mutableListOf<AuthorizationLease>()
        var preview = DevicePairingPreview("pair_fixture", PairingOrganization("org_fixture", "Example"), "/example/build", "/k/fixture", false,
            "Build machine", "123 456", listOf("mailbox"), requestedAt = 1_799_999_970, expiresAt = 1_800_000_570, canApprove = true, state = "pending")
        var hook: suspend (String) -> Unit = {}
        private suspend fun record(action: String) {
            val lease = currentCoroutineContext()[AuthorizationContext]?.lease ?: throw AuthorizationRetired()
            lease.requireActive(); val root = identity()?.authorization ?: throw AuthorizationRetired()
            check(lease.sharesAuthorization(root)); actions += action; roots += root
        }
        override suspend fun resolveDevicePairing(code: DevicePairingCode): DevicePairingPreview {
            record("resolve"); val frozen = preview; hook("resolve"); return frozen
        }
        override suspend fun approveDevicePairing(preview: DevicePairingPreview, code: DevicePairingCode) { record("approve"); hook("approve") }
        override suspend fun denyDevicePairing(preview: DevicePairingPreview, code: DevicePairingCode) { record("deny"); hook("deny") }
    }
    private class Fixture(val scope: CoroutineScope) {
        var clock = 1_800_000_000L
        var source: DeviceApprovalIdentity? = DeviceApprovalIdentity("member", AuthorizationLease())
        var transition = false
        val api = Api { source }
        val expired = mutableListOf<AuthorizationLease>()
        val store = DeviceApprovalStore(api, scope, { source }, { transition }, { clock }, expired::add)
        val context get() = store.state.value.contextId
        val proof get() = store.state.value.proofId
        suspend fun load() { store.resolve("ABCD1234", context)!!.join() }
        fun challenge(): ApiException = ApiException(401, "step_up_required", problem = buildJsonObject { put("acr_values", "2"); put("max_age", 600) })
        suspend fun challenged() { load(); api.hook = { if (it == "approve") throw challenge() }; store.approve(proof, context)!!.join() }
        fun replace(subject: String = "member") { source?.authorization?.retire(); source = DeviceApprovalIdentity(subject, AuthorizationLease()); store.synchronize() }
    }
    private suspend fun world(work: suspend Fixture.() -> Unit) {
        val dispatcher = Executors.newSingleThreadExecutor { Thread(it, "approval-state-test").apply { isDaemon = true } }.asCoroutineDispatcher()
        val scope = CoroutineScope(SupervisorJob() + dispatcher); val fixture = Fixture(scope)
        try { withTimeout(8_000) { withContext(dispatcher) { fixture.store.synchronize(); fixture.work() } } }
        finally { withContext(dispatcher) { fixture.store.close(); fixture.source?.authorization?.retire() }; scope.cancel(); dispatcher.close() }
    }
    @Test fun arrivalOnlyReadsAndRequiresAnExplicitReviewedDecision() = runBlocking<Unit> { world {
        load(); assertEquals(listOf("resolve"), api.actions); assertEquals(DeviceApprovalPhase.REVIEW, store.state.value.phase)
        assertTrue(store.canApprove(proof, context)); assertTrue(store.canDeny(proof, context))
        store.approve(proof, context)!!.join()
        assertEquals(listOf("resolve", "approve"), api.actions); assertEquals(DeviceApprovalPhase.APPROVED, store.state.value.phase)
    } }
    @Test fun absentIdentityAndMalformedCodesCannotIssueARequest() = runBlocking<Unit> { world {
        assertNull(store.resolve("ABCD/1234", context)); assertTrue(api.actions.isEmpty())
        source = null; store.synchronize(); assertNull(store.resolve("ABCD1234", context)); assertTrue(api.actions.isEmpty())
    } }
    @Test fun oldProofsAndContextsCannotActOnARefreshedOrReplacementRequest() = runBlocking<Unit> { world {
        load(); val old = proof; load(); assertNotEquals(old, proof)
        assertNull(store.approve(old, context)); assertNull(store.deny(old, context))
        val original = context; replace(); assertNull(store.resolve("ABCD1234", original)); assertNull(store.approve(old, original))
        assertEquals(listOf("resolve", "resolve"), api.actions)
    } }
    @Test fun expiryAndMissingOrUnknownPermissionsDisableDecisions() = runBlocking<Unit> { world {
        load(); clock = api.preview.expiresAt
        assertNull(store.approve(proof, context)); assertEquals(DeviceApprovalPhase.EXPIRED, store.state.value.phase)
        clock = 1_800_000_000
        for (candidate in listOf(api.preview.copy(canApprove = false), api.preview.copy(capabilities = listOf("future_privilege")), api.preview.copy(state = "approved"), api.preview.copy(state = "denied"))) {
            api.preview = candidate; load(); assertFalse(store.canApprove(proof, context))
        }
        assertTrue(api.actions.all { it == "resolve" })
    } }
    @Test fun aLostApprovalResponseRequiresStatusInspectionInsteadOfAnotherPost() = runBlocking<Unit> { world {
        load(); api.hook = { if (it == "approve") throw IOException("fixture response loss") }
        store.approve(proof, context)!!.join(); assertEquals(DeviceApprovalPhase.UNCONFIRMED, store.state.value.phase)
        assertNull(store.approve(proof, context))
        api.hook = {}; api.preview = api.preview.copy(state = "approved", canApprove = false)
        store.checkStatus(context)!!.join()
        assertEquals(DeviceApprovalPhase.APPROVED, store.state.value.phase); assertEquals(listOf("resolve", "approve", "resolve"), api.actions)
    } }
    @Test fun denialAcknowledgmentIsFollowedByTheActualFinalState() = runBlocking<Unit> { world {
        load(); api.hook = { if (it == "deny") api.preview = api.preview.copy(state = "approved", canApprove = false) }
        store.deny(proof, context)!!.join()
        assertEquals(DeviceApprovalPhase.APPROVED, store.state.value.phase); assertEquals(listOf("resolve", "deny", "resolve"), api.actions)
    } }
    @Test fun anUnconfirmedDenialIsNeverReportedAsDenied() = runBlocking<Unit> { world {
        load(); store.deny(proof, context)!!.join()
        assertEquals(DeviceApprovalPhase.UNCONFIRMED, store.state.value.phase); assertFalse(store.canApprove(proof, context))
        api.preview = api.preview.copy(state = "denied", canApprove = false); store.checkStatus(context)!!.join()
        assertEquals(DeviceApprovalPhase.DENIED, store.state.value.phase)
    } }
    @Test fun mfaSuccessOnlyReloadsAndRequiresAnotherExplicitFreshApproval() = runBlocking<Unit> { world {
        challenged(); val original = context; val oldProof = proof; val oldRoot = source!!.authorization
        assertNotNull(store.state.value.challenge); assertFalse(store.canApprove(proof, context)); api.hook = {}
        store.verifySignIn(proof, context) { requirement, expected ->
            assertSame(oldRoot, expected); assertEquals("2", requirement.requestedACR)
            oldRoot.retire(); source = null; transition = true; store.synchronize()
            assertEquals(DeviceApprovalPhase.VERIFYING, store.state.value.phase)
            val next = AuthorizationLease(); source = DeviceApprovalIdentity("member", next); transition = false; store.synchronize(); next
        }!!.join()
        withTimeout(2_000) { store.state.first { it.phase == DeviceApprovalPhase.REVIEW && it.contextId != original } }
        assertEquals(listOf("resolve", "approve", "resolve"), api.actions); assertFalse(oldRoot.isValid)
        assertNull(store.approve(oldProof, original)); assertTrue(store.canApprove(proof, context))
        store.approve(proof, context)!!.join(); assertEquals(DeviceApprovalPhase.APPROVED, store.state.value.phase)
        assertSame(source!!.authorization, api.roots.last())
    } }
    @Test fun unsupportedChallengesNeverBecomeAnAuthenticationMethodOrAnotherPost() = runBlocking<Unit> { world {
        load(); api.hook = { if (it == "approve") throw ApiException(401, "step_up_required") }
        store.approve(proof, context)!!.join()
        assertEquals(DeviceApprovalPhase.UNAVAILABLE, store.state.value.phase); assertNull(store.state.value.challenge)
        assertNull(store.verifySignIn(proof, context) { _, _ -> fail("No method may be invented"); AuthorizationLease() })
        assertEquals(listOf("resolve", "approve"), api.actions)
    } }
    @Test fun aDifferentOrUnreturnedGrantCannotAdoptTheOldRequest() = runBlocking<Unit> { world {
        challenged(); val reads = api.actions.count { it == "resolve" }
        store.verifySignIn(proof, context) { _, _ -> AuthorizationLease() }!!.join()
        assertNull(store.state.value.code); assertNull(store.state.value.preview); assertEquals(reads, api.actions.count { it == "resolve" })
        challenged()
        store.verifySignIn(proof, context) { _, _ -> replace("another-person"); source!!.authorization }!!.join()
        assertNull(store.state.value.preview); assertNull(store.state.value.code)
    } }
    @Test fun closingDuringVerificationSuppressesEvenANoncancellableSuccess() = runBlocking<Unit> { world {
        challenged(); val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        val verifying = store.verifySignIn(proof, context) { _, old -> withContext(NonCancellable) {
            entered.complete(Unit); release.await(); old.retire(); val next = AuthorizationLease(); source = DeviceApprovalIdentity("member", next); next
        } }!!
        entered.await(); store.close(); release.complete(Unit); verifying.join()
        assertNull(store.state.value.code); assertNull(store.state.value.preview); assertEquals(listOf("resolve", "approve"), api.actions)
    } }
    @Test fun accountBackoffSurvivesCodeChangesAndClosingTheScreen() = runBlocking<Unit> { world {
        api.hook = { throw ApiException(429, "rate_limited", retryAfter = 45) }
        load(); val due = clock + 45
        assertEquals(due, store.state.value.retryAt); store.changeCode(context)
        assertNull(store.resolve("EFGH5678", context)); store.close(); store.synchronize()
        assertNull(store.resolve("EFGH5678", context)); assertEquals(1, api.actions.size)
        clock = due; store.refreshExpiry(); api.hook = {}; load(); assertEquals(2, api.actions.size)
    } }
    @Test fun anotherPersonDoesNotInheritThePreviousAccountsBackoff() = runBlocking<Unit> { world {
        api.hook = { throw ApiException(429, "rate_limited", retryAfter = 45) }; load()
        replace("another-person"); api.hook = {}; assertNull(store.state.value.retryAt); load()
        assertEquals(DeviceApprovalPhase.REVIEW, store.state.value.phase)
    } }
    @Test fun aLateClosedReadCannotReplaceOrClearTheNewReadsBusyState() = runBlocking<Unit> { world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>(); var first = true
        api.hook = { if (first) { first = false; withContext(NonCancellable) { entered.complete(Unit); release.await() } } }
        val old = store.resolve("ABCD1234", context)!!; entered.await(); store.close(); store.synchronize()
        val nextEntered = CompletableDeferred<Unit>(); val nextRelease = CompletableDeferred<Unit>()
        api.preview = api.preview.copy(label = "Replacement machine")
        api.hook = { nextEntered.complete(Unit); nextRelease.await() }
        val next = store.resolve("EFGH5678", context)!!; nextEntered.await(); release.complete(Unit); old.join()
        assertEquals(DeviceApprovalPhase.RESOLVING, store.state.value.phase); assertNull(store.state.value.preview)
        nextRelease.complete(Unit); next.join(); assertEquals("Replacement machine", store.state.value.preview!!.label)
    } }
    @Test fun deniedMembershipClearsPrivateMetadataAndOldConfirmations() = runBlocking<Unit> { world {
        load(); val original = proof; api.hook = { throw ApiException(403, "forbidden") }; load()
        assertNull(store.state.value.preview); assertNull(store.approve(original, context)); assertNull(store.deny(original, context))
    } }
    @Test fun duplicateApprovalCannotStartTwoPostsBeforeTheFirstRuns() = runBlocking<Unit> { world {
        load(); val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        api.hook = { if (it == "approve") { entered.complete(Unit); release.await() } }
        val first = store.approve(proof, context)!!; assertNull(store.approve(proof, context)); entered.await()
        assertEquals(1, api.actions.count { it == "approve" }); release.complete(Unit); first.join()
    } }
    @Test fun expiryNotifiesOnlyTheCurrentlyBoundAcceptedGrant() = runBlocking<Unit> { world {
        val accepted = source!!.authorization; api.hook = { throw SessionExpired() }; load()
        assertEquals(listOf(accepted), expired); assertNull(store.state.value.preview); assertNull(store.state.value.authorizationId)
    } }
}
