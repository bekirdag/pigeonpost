package dev.pigeonpost.core

import kotlinx.coroutines.*
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import okhttp3.mockwebserver.SocketPolicy
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.util.concurrent.TimeUnit

class PostboxClientTest {
    private lateinit var server: MockWebServer
    private lateinit var client: PostboxClient
    private val tokens = object : TokenProvider { override suspend fun token(rejected: String?) = if (rejected == null) "first" else "renewed" }
    @Before fun setup() { server = MockWebServer(); server.start(); client = PostboxClient(tokens, server.url("/"), allowLoopbackForTests = true) }
    @After fun teardown() { server.shutdown() }
    @Test fun accountHandlesUseMemberScopeAndKeepExpiredCrossStoreNames() = runBlocking {
        server.enqueue(MockResponse().setBody("""{"handles":[{"namespace":"apple","source":"apple","expires_at":300,"active":true},{"namespace":"google","source":"google","expires_at":200,"active":false}]}"""))
        val rows = client.accountHandles()
        assertEquals(2, rows.size); assertFalse(rows[1].active); assertEquals("/google", rows[1].name)
        val request = server.takeRequest()
        assertEquals("/v1/me/handles?include_inactive=true", request.path)
        assertEquals("Bearer first", request.getHeader("Authorization"))
        assertNull(request.requestUrl!!.queryParameter("identity"))
    }
    @Test fun testerRegistrationUsesAuthenticatedPreviewRoutesAndOnlySendsTheName() = runBlocking {
        server.enqueue(MockResponse().setBody("""{"eligible":true,"namespace":null}"""))
        server.enqueue(MockResponse().setBody("""{"name":"alex","available":true}"""))
        server.enqueue(MockResponse().setBody("""{"eligible":true,"namespace":"/alex","mailbox":"/alex/main","source":"test_preview"}"""))
        assertTrue(client.handleOffer().eligible)
        assertTrue(client.checkHandle(" /Alex/ ").available)
        assertEquals("/alex/main", client.claimHandle(" /Alex/ ").mailbox)
        val offer = server.takeRequest(); assertEquals("/v1/claims/test", offer.path)
        val availability = server.takeRequest(); assertEquals("/v1/handles/alex/availability", availability.path)
        val claim = server.takeRequest(); assertEquals("/v1/claims/test", claim.path); assertEquals("POST", claim.method)
        assertEquals("Bearer first", claim.getHeader("Authorization"))
        assertEquals("""{"namespace":"alex"}""", claim.body.readUtf8())
    }
    @Test fun invalidHandleNamesNeverReachTheNetwork() = runBlocking {
        try { client.claimHandle("/a/b"); fail("Expected invalid namespace") } catch (_: IllegalArgumentException) {}
        try { client.checkHandle("a".repeat(33)); fail("Expected invalid namespace") } catch (_: IllegalArgumentException) {}
        assertEquals(0, server.requestCount)
    }
    @Test fun initialFetchAndPollKeepSentAndReadHistory() = runBlocking {
        repeat(2) { server.enqueue(MockResponse().setBody("{\"messages\":[]}")) }
        client.inbox("/team/main")
        client.inbox("/team/main", 25)
        repeat(2) { index ->
            val request = server.takeRequest()
            assertEquals("/team/main", request.requestUrl!!.queryParameter("identity"))
            assertEquals("true", request.requestUrl!!.queryParameter("include_sent"))
            assertEquals("true", request.requestUrl!!.queryParameter("include_read"))
            assertEquals(if (index == 0) null else "25", request.requestUrl!!.queryParameter("wait"))
        }
    }
    @Test fun unauthorizedRequestRetriesOnceWithRenewedToken() = runBlocking {
        server.enqueue(MockResponse().setResponseCode(401)); server.enqueue(MockResponse().setBody("{\"identities\":[]}"))
        assertTrue(client.identities().isEmpty())
        assertEquals("Bearer first", server.takeRequest().getHeader("Authorization"))
        assertEquals("Bearer renewed", server.takeRequest().getHeader("Authorization"))
        assertEquals(2, server.requestCount)
    }
    @Test fun repeatedUnauthorizedInvalidatesTheRejectedSession() = runBlocking {
        var invalidated: String? = null
        client = PostboxClient(object : TokenProvider {
            override suspend fun token(rejected: String?) = if (rejected == null) "old" else "new"
            override suspend fun invalidate(rejected: String) { invalidated = rejected }
        }, server.url("/"), true)
        repeat(2) { server.enqueue(MockResponse().setResponseCode(401)) }
        try { client.identities(); fail("Expected expired session") } catch (_: SessionExpired) {}
        assertEquals("new", invalidated)
        assertEquals(2, server.requestCount)
    }
    @Test fun concurrent401ResponsesUseTheTokenProvidersCoalescingContract() = runBlocking {
        var refreshes = 0; var token = "old"; val lock = Mutex()
        client = PostboxClient(object : TokenProvider {
            override suspend fun token(rejected: String?) = lock.withLock {
                if (rejected != null && token == rejected) { delay(20); token = "new"; refreshes++ }
                token
            }
        }, server.url("/"), true)
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest) = if (request.getHeader("Authorization") == "Bearer old") MockResponse().setResponseCode(401)
                else MockResponse().setBody("{\"identities\":[]}")
        }
        coroutineScope { List(8) { async { client.identities() } }.awaitAll() }
        assertEquals(1, refreshes)
    }
    @Test fun uncertainSendIsNeverAutomaticallyReplayed() = runBlocking {
        server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST))
        try { client.send("/me", "/peer", "body", null); fail("Expected uncertain send") } catch (_: IOException) {}
        assertEquals("POST", server.takeRequest().method)
        assertEquals(1, server.requestCount)
    }
    @Test fun authenticatedRedirectIsNotFollowed() = runBlocking {
        server.enqueue(MockResponse().setResponseCode(307).addHeader("Location", server.url("/unexpected")))
        try { client.identities(); fail("Expected rejected redirect") } catch (failure: ApiException) { assertEquals(307, failure.status) }
        assertEquals(1, server.requestCount)
    }
    @Test fun cancelStopsAnOutstandingPoll() = runBlocking {
        server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.NO_RESPONSE))
        val job = launch(Dispatchers.Default) { client.inbox("/me", 25) }
        assertNotNull(withContext(Dispatchers.IO) { server.takeRequest(3, TimeUnit.SECONDS) })
        withTimeout(2000) { job.cancelAndJoin() }
        assertTrue(job.isCancelled)
    }
    @Test fun invalidOpaqueIdsNeverReachTheNetwork() = runBlocking {
        try { client.deleteThread("/me", "../other?identity=/victim"); fail("Expected invalid id") } catch (_: IllegalArgumentException) {}
        assertEquals(0, server.requestCount)
    }
    @Test fun attachmentDownloadCarriesIdentityAndHonorsSizeLimit() = runBlocking {
        server.enqueue(MockResponse().setBody("Hello"))
        val output = ByteArrayOutputStream()
        client.download("/me", "a_123", output)
        assertEquals("Hello", output.toString("UTF-8"))
        assertEquals("/me", server.takeRequest().getHeader("x-pigeonpost-identity"))
        server.enqueue(MockResponse().setBody("Too big"))
        try { client.download("/me", "a_123", ByteArrayOutputStream(), 2); fail("Expected size limit") } catch (_: IOException) {}
    }
}
