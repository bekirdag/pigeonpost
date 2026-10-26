package dev.pigeonpost.core

import kotlinx.coroutines.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import java.net.InetAddress
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class PostboxAuthorizationTest {
    private lateinit var server: MockWebServer
    private class Source : TokenProvider {
        @Volatile var grant = AuthorizationLease()
        override val authorizationLease get() = grant
        var calls = 0
        var invalidations = 0
        var issue: suspend (String?) -> String = { "fixture-first" }
        override suspend fun token(rejected: String?): String { calls++; return issue(rejected) }
        override suspend fun invalidate(rejected: String) { invalidations++ }
    }
    private lateinit var source: Source
    @Before fun setup() {
        source = Source()
        server = MockWebServer().also { it.start(InetAddress.getByName("127.0.0.1"), 0) }
    }
    @After fun teardown() { server.shutdown() }
    private fun api(tokens: TokenProvider = source) = PostboxClient(tokens,
        server.url("/").newBuilder().host("127.0.0.1").build(), allowLoopbackForTests = true)
    private fun response() = MockResponse().setBody("""{"features":["groups"]}""")

    @Test fun aSuspendedOldTaskCannotIssueALaterRequestWithNewCredentials() = runBlocking {
        val scope = AuthorizedStoreScope(CoroutineScope(currentCoroutineContext()))
        scope.activate(source.grant)
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        val old = scope.async { entered.complete(Unit); release.await(); runCatching { api().capabilities() } }
        entered.await(); scope.retire(); source.grant.retire(); source.grant = AuthorizationLease(); scope.activate(source.grant)
        source.issue = { "fixture-next-account" }; release.complete(Unit)
        assertTrue(old.await().exceptionOrNull() is AuthorizationRetired)
        assertEquals(0, source.calls); assertEquals(0, server.requestCount)
        server.enqueue(response())
        assertEquals(setOf("groups"), scope.async { api().capabilities().enabled }.await())
        assertEquals("Bearer fixture-next-account", server.takeRequest().getHeader("Authorization"))
    }
    @Test fun anInactiveStoreCannotUseTheStillSignedInSession() = runBlocking {
        val scope = AuthorizedStoreScope(CoroutineScope(currentCoroutineContext()))
        scope.activate(source.grant); scope.retire()
        assertTrue(scope.async { runCatching { api().capabilities() } }.await().exceptionOrNull() is AuthorizationRetired)
        assertTrue(source.grant.isValid); assertEquals(0, source.calls); assertEquals(0, server.requestCount)
    }
    @Test fun siblingActorsMayUseTheSameAcceptedAuthorization() = runBlocking {
        val actor = ScopedTokenProvider(source, source.grant.child())
        server.enqueue(response())
        val result = withContext(AuthorizationContext(source.grant.child())) { api(actor).capabilities() }
        assertEquals(setOf("groups"), result.enabled); assertEquals(1, server.requestCount)
    }
    @Test fun anUnretiredOldContextStillCannotBorrowAnUnrelatedRoot() = runBlocking {
        val old = source.grant.child(); source.grant = AuthorizationLease()
        val result = runCatching { withContext(AuthorizationContext(old)) { api().capabilities() } }
        assertTrue(result.exceptionOrNull() is AuthorizationRetired)
        assertEquals(0, source.calls); assertEquals(0, server.requestCount)
    }
    @Test fun aLateResponseIsRejectedWithoutRefreshingOrReturningItsBody() = runBlocking {
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                entered.countDown(); check(release.await(3, TimeUnit.SECONDS)); return response()
            }
        }
        try {
            val result = async { runCatching { api().capabilities() } }
            assertTrue(withContext(Dispatchers.IO) { entered.await(3, TimeUnit.SECONDS) })
            source.grant.retire(); source.grant = AuthorizationLease(); release.countDown()
            assertTrue(withTimeout(3_000) { result.await() }.exceptionOrNull() is AuthorizationRetired)
            assertEquals(1, source.calls); assertEquals(0, source.invalidations); assertEquals(1, server.requestCount)
        } finally { release.countDown() }
    }
    @Test fun a401RefreshCannotSendTheReplacementAccountsToken() = runBlocking {
        val renewing = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        source.issue = { rejected -> if (rejected == null) "fixture-first" else { renewing.complete(Unit); release.await(); "fixture-next-account" } }
        server.enqueue(MockResponse().setResponseCode(401))
        val result = async { runCatching { api().capabilities() } }
        renewing.await(); source.grant.retire(); source.grant = AuthorizationLease(); release.complete(Unit)
        assertTrue(result.await().exceptionOrNull() is AuthorizationRetired)
        assertEquals(1, server.requestCount); assertEquals(0, source.invalidations)
    }
    @Test fun aLegitimateRefreshKeepsTheSameAcceptedContext() = runBlocking {
        val lease = source.grant
        source.issue = { if (it == null) "fixture-first" else "fixture-renewed" }
        server.enqueue(MockResponse().setResponseCode(401)); server.enqueue(response())
        assertEquals(setOf("groups"), api().capabilities().enabled)
        assertSame(lease, source.grant); assertTrue(lease.isValid)
        assertEquals("Bearer fixture-first", server.takeRequest().getHeader("Authorization"))
        assertEquals("Bearer fixture-renewed", server.takeRequest().getHeader("Authorization"))
    }
}
