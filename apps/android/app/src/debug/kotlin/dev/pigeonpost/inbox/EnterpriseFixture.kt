package dev.pigeonpost.inbox

import android.content.Intent
import dev.pigeonpost.core.*
import dev.pigeonpost.core.calendar.CalendarTransport
import dev.pigeonpost.core.calendar.CalendarMeetingApi
import dev.pigeonpost.core.enterprise.*
import dev.pigeonpost.inbox.auth.SessionState
import dev.pigeonpost.inbox.auth.UserSession
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.io.IOException
import java.time.Instant
import java.util.concurrent.atomic.AtomicInteger

/** Explicit debug fixtures only: no HTTP, browser, real agents or external delivery. */
internal class EnterpriseFixtureSession(private val mode: String) : UserSession {
    @Volatile var grant: AuthorizationLease? = if (mode == "enterprise-signedout") null else AuthorizationLease()
    override val authorizationLease get() = grant
    @Volatile var verified = mode != "enterprise" && mode != "enterprise-signedout"
    val verifications = AtomicInteger()
    override val state = MutableStateFlow(SessionState(loading = false, signedIn = grant != null, termsAccepted = grant != null,
        authorizationId = grant?.id, username = "demo", subject = if (grant != null) "fixture-member" else null))
    override suspend fun token(rejected: String?) = grant?.let { it.requireActive(); "fixture-" + it.id } ?: throw SessionExpired()
    override suspend fun begin(provider: String?, otherAccount: Boolean): Intent? {
        grant?.retire(); grant = AuthorizationLease()
        state.value = state.value.copy(signedIn = true, termsAccepted = false, authorizationId = grant!!.id, subject = "fixture-member")
        return null
    }
    override suspend fun complete(result: Intent?) {}
    override suspend fun cancel() {}
    override suspend fun acceptTerms() { state.value = state.value.copy(termsAccepted = state.value.signedIn) }
    override suspend fun signOut() { grant?.retire(); grant = null; state.value = SessionState(loading = false) }
    override suspend fun verifyStrongSignIn(requirement: StrongSignInRequirement, expectedAuthorization: AuthorizationLease, open: (Intent) -> Unit): AuthorizationLease {
        expectedAuthorization.requireActive()
        if (expectedAuthorization !== grant) throw AuthorizationRetired()
        verifications.incrementAndGet(); expectedAuthorization.retire(); val next = AuthorizationLease(); grant = next; verified = true
        state.value = state.value.copy(authorizationId = next.id)
        return next
    }
}

internal class EnterpriseFixture(private val calendar: CalendarFixture, val session: EnterpriseFixtureSession, private val mode: String) :
    PostboxApi by calendar, CalendarTransport by calendar, CalendarMeetingApi by calendar, DevicePairingApi {
    companion object { @Volatile var current: EnterpriseFixture? = null }
    val resolves = AtomicInteger(); val approvals = AtomicInteger(); val denials = AtomicInteger()
    @Volatile var agentCreated = false
    private var decided = "pending"
    private val created = Instant.now().epochSecond
    init { current = this }
    override suspend fun capabilities() = Capabilities(if (mode == "enterprise-off") listOf("calendar") else listOf("calendar", "orgs"))
    private suspend fun authorized() {
        val child = currentCoroutineContext()[AuthorizationContext]?.lease ?: throw AuthorizationRetired()
        child.requireActive()
        val root = session.grant ?: throw AuthorizationRetired()
        if (!child.sharesAuthorization(root)) throw AuthorizationRetired()
    }
    override suspend fun resolveDevicePairing(code: DevicePairingCode): DevicePairingPreview {
        authorized(); resolves.incrementAndGet()
        if (code.rawValue !in setOf("ABCD1234", "EFGH5678")) throw ApiException(404, "pairing_not_found")
        val allowed = mode != "enterprise-no-permission" && decided == "pending"
        return DevicePairingPreview("pair_fixture", PairingOrganization("org_fixture", "Example"), "/example/build",
            if (mode == "enterprise-new-agent" && !agentCreated) null else "/k/fixtureagent", mode == "enterprise-new-agent" && !agentCreated,
            "Build machine", "123 456", listOf("mailbox"), requestedAt = created, expiresAt = created + 600,
            canApprove = allowed, reason = if (mode == "enterprise-no-permission") "delegation_required" else null, state = decided)
    }
    override suspend fun approveDevicePairing(preview: DevicePairingPreview, code: DevicePairingCode) {
        authorized(); approvals.incrementAndGet()
        if (!session.verified) throw ApiException(401, "step_up_required", problem = buildJsonObject { put("acr_values", "2"); put("max_age", 600) })
        if (mode == "enterprise-no-permission") throw ApiException(403, "forbidden")
        if (decided != "pending") throw ApiException(409, "pairing_decided")
        decided = "approved"; agentCreated = mode == "enterprise-new-agent"
        if (mode == "enterprise-unconfirmed") throw IOException("Fixture accepted response loss")
    }
    override suspend fun denyDevicePairing(preview: DevicePairingPreview, code: DevicePairingCode) {
        authorized(); denials.incrementAndGet()
        decided = if (mode == "enterprise-deny-race") "approved" else "denied"
    }
}
