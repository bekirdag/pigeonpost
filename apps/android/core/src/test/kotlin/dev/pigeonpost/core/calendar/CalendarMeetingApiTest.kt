package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import okhttp3.mockwebserver.*
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import java.net.InetAddress
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class CalendarMeetingApiTest {
    private class Tokens : TokenProvider {
        @Volatile var grant = AuthorizationLease()
        override val authorizationLease get() = grant
        var acquisitions = 0; var invalidations = 0
        override suspend fun token(rejected: String?): String { acquisitions++; return if (rejected == null) "fixture-first" else "fixture-renewed" }
        override suspend fun invalidate(rejected: String) { invalidations++ }
    }
    private lateinit var source: Tokens
    private lateinit var postbox: MockWebServer
    private lateinit var calls: MockWebServer
    private lateinit var api: PostboxClient
    @Before fun setup() {
        source = Tokens()
        postbox = MockWebServer().apply { start(InetAddress.getByName("127.0.0.1"), 0) }
        calls = MockWebServer().apply { start(InetAddress.getByName("127.0.0.1"), 0) }
        api = PostboxClient(source, postbox.url("/"), allowLoopbackForTests = true, inbox = calls.url("/"))
    }
    @After fun teardown() { postbox.shutdown(); calls.shutdown() }
    private fun status() = MockResponse().setBody("""{"state":"open","occurrences":[{"occurrence_id":"occ_one","starts_at":100,"ends_at":200,"opens_at":50,"closes_at":250}]}""")
    @Test fun lookupUsesTheCallOriginAndExplicitActingMailboxWithoutAJoin() = runBlocking<Unit> {
        calls.enqueue(status())
        assertEquals(1, api.scheduledMeeting("/k/alice", "ABCDE-FGHJK").choices.size)
        val request = calls.takeRequest()
        assertEquals("GET", request.method)
        assertEquals("/calls/v1/meetings/scheduled/ABCDEFGHJK", request.requestUrl!!.encodedPath)
        assertEquals("/k/alice", request.requestUrl!!.queryParameter("as"))
        assertEquals("Bearer fixture-first", request.getHeader("Authorization"))
        assertEquals(0L, request.bodySize); assertEquals(0, postbox.requestCount)
    }
    @Test fun renewalKeepsTheSameCodeAndMailbox() = runBlocking<Unit> {
        calls.enqueue(MockResponse().setResponseCode(401)); calls.enqueue(status())
        assertEquals(1, api.scheduledMeeting("/k/alice", "ABCDEFGHJK").choices.size)
        val first = calls.takeRequest(); val next = calls.takeRequest()
        assertEquals(first.path, next.path); assertEquals("Bearer fixture-renewed", next.getHeader("Authorization"))
        assertEquals(2, source.acquisitions)
    }
    @Test fun redirectCannotSendCredentialsToAnotherOrigin() = runBlocking<Unit> {
        calls.enqueue(MockResponse().setResponseCode(302).setHeader("Location", postbox.url("/private")))
        val failure = runCatching { api.scheduledMeeting("/k/alice", "ABCDEFGHJK") }.exceptionOrNull()
        assertTrue(failure is ApiException); assertEquals(0, postbox.requestCount); assertEquals(1, calls.requestCount)
    }
    @Test fun malformedLocatorsAndUnsafeOriginsAreRefusedBeforeTokenAcquisition() = runBlocking<Unit> {
        for (code in listOf("ABCDEF", "ABCDE/FGHJK", "ABCDE%2FFGHJK")) assertTrue(runCatching { api.scheduledMeeting("/k/alice", code) }.isFailure)
        assertTrue(runCatching { api.scheduledMeeting("/mutable/handle", "ABCDEFGHJK") }.isFailure)
        val unsafe = PostboxClient(source, postbox.url("/"), allowLoopbackForTests = true, inbox = calls.url("/nested/"))
        assertTrue(runCatching { unsafe.scheduledMeeting("/k/alice", "ABCDEFGHJK") }.isFailure)
        assertEquals(0, source.acquisitions); assertEquals(0, calls.requestCount)
    }
    @Test fun lateStatusAnd401NeverBorrowTheNextLoginsToken() = runBlocking<Unit> {
        for (code in listOf(200, 401)) {
            val entered = CountDownLatch(1); val release = CountDownLatch(1)
            calls.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); return if (code == 401) MockResponse().setResponseCode(401) else status() }
            }
            val before = source.acquisitions
            coroutineScope {
                val old = async(Dispatchers.IO) { api.scheduledMeeting("/k/alice", "ABCDEFGHJK") }
                assertTrue(withContext(Dispatchers.IO) { entered.await(5, TimeUnit.SECONDS) })
                source.grant.retire(); source.grant = AuthorizationLease(); release.countDown()
                val failure = runCatching { old.await() }.exceptionOrNull()
                assertTrue(failure is AuthorizationRetired)
            }
            assertEquals(before + 1, source.acquisitions); assertEquals(0, source.invalidations)
        }
    }
    @Test fun explicitInactiveScopeCannotAcquireTheCurrentToken() = runBlocking<Unit> {
        val failure = runCatching { withContext(AuthorizationContext(null)) { api.scheduledMeeting("/k/alice", "ABCDEFGHJK") } }.exceptionOrNull()
        assertTrue(failure is AuthorizationRetired); assertEquals(0, source.acquisitions)
    }
    @Test fun overlargeOrMalformedStatusIsRefusedWithoutAutomaticReplay() = runBlocking<Unit> {
        calls.enqueue(MockResponse().setBody("x").setHeader("Content-Length", 1024 * 1024 + 1))
        assertEquals("response_too_large", (runCatching { api.scheduledMeeting("/k/alice", "ABCDEFGHJK") }.exceptionOrNull() as ApiException).code)
        calls.enqueue(MockResponse().setBody("not-json"))
        assertEquals("bad_response", (runCatching { api.scheduledMeeting("/k/alice", "ABCDEFGHJK") }.exceptionOrNull() as ApiException).code)
        assertEquals(2, calls.requestCount)
    }
}
