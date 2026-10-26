package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.*
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import java.net.InetAddress
import java.time.Instant
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class DevicePairingApiTest {
    private class Tokens : TokenProvider {
        @Volatile var grant = AuthorizationLease()
        override val authorizationLease get() = grant
        var acquisitions = 0; var invalidations = 0
        override suspend fun token(rejected: String?): String { acquisitions++; return if (rejected == null) "fixture-first" else "fixture-renewed" }
        override suspend fun invalidate(rejected: String) { invalidations++ }
    }
    private lateinit var tokens: Tokens
    private lateinit var server: MockWebServer
    private lateinit var api: PostboxClient
    private val code = DevicePairingCode.parse("ABCD1234")!!
    private fun preview() = DevicePairingPreview("pair_fixture", PairingOrganization("org_fixture", "Example"), "/example/build", "/k/fixture", false,
        "Build machine", "123 456", listOf("mailbox"), requestedAt = Instant.now().epochSecond - 60, expiresAt = Instant.now().epochSecond + 540, canApprove = true, state = "pending")
    private fun resolved() = MockResponse().setBody(WireJson.encodeToString(DevicePairingPreview.serializer(), preview()))
    private fun approved(address: String = "/k/fixture", handle: String = "/example/build") = MockResponse().setBody("""{"approved":true,"address":"$address","handle":"$handle"}""")
    private fun challenge(body: String = """{"error":"step_up_required","acr_values":["2"],"max_age":600}""") = MockResponse().setResponseCode(401).setBody(body)
    @Before fun setup() { tokens = Tokens(); server = MockWebServer().apply { start(InetAddress.getByName("127.0.0.1"), 0) }; api = PostboxClient(tokens, server.url("/"), allowLoopbackForTests = true) }
    @After fun teardown() { server.shutdown() }
    @Test fun requestsCarryOnlyTheReviewedCodeToTheirExactEndpoint() = runBlocking<Unit> {
        server.enqueue(resolved()); server.enqueue(approved()); server.enqueue(MockResponse().setBody("""{"denied":true}"""))
        val p = api.resolveDevicePairing(code); api.approveDevicePairing(p, code); api.denyDevicePairing(p, code)
        for (suffix in listOf("resolve", "pair_fixture/approve", "pair_fixture/deny")) {
            val request = server.takeRequest(); assertEquals("POST", request.method); assertEquals("/v1/device-pairings/$suffix", request.path)
            val body = WireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
            assertEquals(setOf("user_code"), body.keys); assertEquals("ABCD-1234", body.getValue("user_code").jsonPrimitive.content)
        }
        assertEquals(3, server.requestCount) // Denial API returns only an acknowledgment, never an inferred final state.
    }
    @Test fun ordinaryRenewalRetainsTheExactDecisionBytes() = runBlocking<Unit> {
        server.enqueue(MockResponse().setResponseCode(401)); server.enqueue(approved())
        api.approveDevicePairing(preview(), code)
        val first = server.takeRequest(); val next = server.takeRequest()
        assertEquals(first.path, next.path); assertEquals(first.body.readUtf8(), next.body.readUtf8())
        assertEquals("Bearer fixture-renewed", next.getHeader("Authorization")); assertEquals(2, tokens.acquisitions); assertEquals(0, tokens.invalidations)
    }
    @Test fun aStrongAuthenticationRefusalNeverRenewsOrReplaysTheDecision() = runBlocking<Unit> {
        server.enqueue(challenge())
        val failure = runCatching { api.approveDevicePairing(preview(), code) }.exceptionOrNull() as ApiException
        assertTrue(failure.needsStrongSignIn); assertEquals("2", failure.signInRequirement!!.requestedACR)
        assertEquals(1, server.requestCount); assertEquals(1, tokens.acquisitions); assertEquals(0, tokens.invalidations); assertTrue(tokens.grant.isValid)
    }
    @Test fun aChallengeAfterOrdinaryRenewalDoesNotInvalidateTheAcceptedLogin() = runBlocking<Unit> {
        server.enqueue(MockResponse().setResponseCode(401)); server.enqueue(challenge())
        val failure = runCatching { api.approveDevicePairing(preview(), code) }.exceptionOrNull() as ApiException
        assertTrue(failure.needsStrongSignIn); assertEquals(2, server.requestCount); assertEquals(2, tokens.acquisitions)
        assertEquals(0, tokens.invalidations); assertTrue(tokens.grant.isValid)
    }
    @Test fun malformedHeaderOnlyChallengesStillStopAutomaticReplay() = runBlocking<Unit> {
        server.enqueue(challenge("{}").setHeader("WWW-Authenticate", "Bearer error=insufficient_user_authentication, acr_values=2, acr_values=1, max_age=600"))
        val failure = runCatching { api.approveDevicePairing(preview(), code) }.exceptionOrNull() as ApiException
        assertTrue(failure.needsStrongSignIn); assertNull(failure.signInRequirement)
        assertEquals(1, server.requestCount); assertEquals(1, tokens.acquisitions); assertEquals(0, tokens.invalidations)
    }
    @Test fun terminalExpiredUnpermittedAndMalformedPreviewsNeverAcquireAToken() = runBlocking<Unit> {
        val p = preview()
        for (invalid in listOf(p.copy(state = "approved"), p.copy(expiresAt = p.requestedAt + 1), p.copy(canApprove = false), p.copy(pairingId = "../escape"), p.copy(capabilities = listOf("future_privilege"))))
            assertEquals("pairing_not_actionable", (runCatching { api.approveDevicePairing(invalid, code) }.exceptionOrNull() as ApiException).code)
        assertEquals(0, tokens.acquisitions); assertEquals(0, server.requestCount)
    }
    @Test fun acknowledgmentsMustMatchTheExactReviewedTarget() = runBlocking<Unit> {
        for (response in listOf(approved("/k/another"), approved(handle = "/example/another"), MockResponse().setBody("""{"approved":false}"""), approved("/example/build"))) {
            server.enqueue(response)
            assertEquals("bad_response", (runCatching { api.approveDevicePairing(preview(), code) }.exceptionOrNull() as ApiException).code)
        }
        server.enqueue(approved("/k/newagent")); api.approveDevicePairing(preview().copy(address = null, createsAgent = true), code)
    }
    @Test fun redirectsCannotForwardTheHumanCredentialElsewhere() = runBlocking<Unit> {
        val other = MockWebServer().apply { start(InetAddress.getByName("127.0.0.1"), 0) }
        try {
            server.enqueue(MockResponse().setResponseCode(302).setHeader("Location", other.url("/steal")))
            assertEquals(302, (runCatching { api.resolveDevicePairing(code) }.exceptionOrNull() as ApiException).status)
            assertEquals(0, other.requestCount); assertEquals(1, server.requestCount)
        } finally { other.shutdown() }
    }
    @Test fun oversizedAndMalformedResponsesAreNotRetriedOrExposed() = runBlocking<Unit> {
        server.enqueue(MockResponse().setBody("x").setHeader("Content-Length", 1024 * 1024 + 1))
        assertEquals("response_too_large", (runCatching { api.resolveDevicePairing(code) }.exceptionOrNull() as ApiException).code)
        server.enqueue(MockResponse().setBody("private malformed fixture"))
        val failure = runCatching { api.resolveDevicePairing(code) }.exceptionOrNull() as ApiException
        assertEquals("bad_response", failure.code); assertFalse(failure.toString().contains("private malformed")); assertEquals(2, server.requestCount)
    }
    @Test fun aLateDecisionResultCannotBecomeTheReplacementPersonsResult() = runBlocking<Unit> {
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); return approved() }
        }
        coroutineScope {
            val old = async(Dispatchers.IO) { api.approveDevicePairing(preview(), code) }
            assertTrue(withContext(Dispatchers.IO) { entered.await(5, TimeUnit.SECONDS) })
            tokens.grant.retire(); tokens.grant = AuthorizationLease(); release.countDown()
            assertTrue(runCatching { old.await() }.exceptionOrNull() is AuthorizationRetired)
        }
        assertEquals(1, tokens.acquisitions); assertEquals(0, tokens.invalidations); assertTrue(tokens.grant.isValid)
    }
    @Test fun explicitInactiveScopesCannotBorrowTheCurrentPersonsToken() = runBlocking<Unit> {
        assertTrue(runCatching { withContext(AuthorizationContext(null)) { api.resolveDevicePairing(code) } }.exceptionOrNull() is AuthorizationRetired)
        assertEquals(0, tokens.acquisitions); assertEquals(0, server.requestCount)
    }
    @Test fun anOrdinaryRepeated401StillExpiresTheOriginalLogin() = runBlocking<Unit> {
        server.enqueue(MockResponse().setResponseCode(401)); server.enqueue(MockResponse().setResponseCode(401))
        assertTrue(runCatching { api.resolveDevicePairing(code) }.exceptionOrNull() is SessionExpired)
        assertEquals(2, server.requestCount); assertEquals(2, tokens.acquisitions); assertEquals(1, tokens.invalidations)
    }
    @Test fun rateLimitsRetainBoundedRetryAfterWithoutReplayingTheRequest() = runBlocking<Unit> {
        for ((header, expected) in listOf("45" to 45, "9999999999" to 3600, "unrecognized" to null)) {
            server.enqueue(MockResponse().setResponseCode(429).setHeader("Retry-After", header).setBody("""{"error":"rate_limited"}"""))
            val failure = runCatching { api.resolveDevicePairing(code) }.exceptionOrNull() as ApiException
            assertEquals(expected, failure.retryAfter)
        }
        assertEquals(3, server.requestCount); assertEquals(3, tokens.acquisitions); assertEquals(0, tokens.invalidations)
    }
}
