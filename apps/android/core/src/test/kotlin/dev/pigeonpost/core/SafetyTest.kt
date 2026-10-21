@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package dev.pigeonpost.core

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.*
import kotlinx.coroutines.withTimeout
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

/** Phase 1b: capabilities gating, call history, filtering, blocking and local expiry. */
class SafetyTest {
    private open class Phase1b(val features: List<String> = listOf("call_history", "message_filtering", "call_admission", "avatars")) : FakePostbox() {
        var calls = listOf(
            CallLogEntry("c1", "incoming", "missed", "video", "/k/stranger", "/demo/builder", startedAt = 100),
            CallLogEntry("c2", "outgoing", "answered", "audio", peerHandle = "/demo/builder", startedAt = 50, answeredAt = 52, endedAt = 177),
        )
        var unseen = 1
        var filteredRows = listOf(Message("f1", "hello?", from = "/k/spam", peer = "/k/spam", receivedAt = 10), Message("f2", "again", from = "/k/spam", peer = "/k/spam", receivedAt = 20))
        val reports = mutableListOf<String>()
        val accepted = mutableListOf<String>()
        val seenUpTo = mutableListOf<Long>()
        override suspend fun capabilities() = Capabilities(features)
        override suspend fun callLog(identity: String, filter: String, before: Long?, limit: Int) = CallLogPage(calls.filter { filter != "missed" || it.missed }, unseen)
        override suspend fun markCallsSeen(identity: String, upTo: Long): Int { seenUpTo += upTo; unseen = 0; return 0 }
        override suspend fun deleteCall(identity: String, id: String) { calls = calls.filterNot { it.id == id } }
        override suspend fun reportCall(identity: String, callId: String) { reports += "call:$callId" }
        override suspend fun reportSpam(identity: String, id: String) { reports += "message:$id" }
        override suspend fun filteredInbox(identity: String) = filteredRows
        override suspend fun acceptFiltered(identity: String, peer: String) { accepted += peer; rows = rows + filteredRows; filteredRows = emptyList() }
        override suspend fun policy(identity: String) = FilterPolicy("ring", "silence", "show", "show", "filter", 180)
        override suspend fun setPolicy(identity: String, field: String, value: String) = policy(identity).with(field, value)
    }

    @Test fun featuresAreOffUntilTheServerSaysSo() = runTest {
        val store = InboxStore(Phase1b(emptyList()), this)
        store.loadAccount("/a"); advanceUntilIdle()
        assertTrue(store.state.value.features.isEmpty())
        assertTrue(store.state.value.calls.entries.isEmpty())
        assertTrue(store.state.value.filteredGroups.isEmpty())
        store.showView(LeftView.CALLS); advanceUntilIdle()
        assertEquals("no Calls page without call_history", LeftView.INBOX, store.state.value.leftView)
        store.reset()
    }

    @Test fun callsLoadWithTheBadgeAndViewingThemMarksThemSeen() = runTest {
        val api = Phase1b()
        val store = InboxStore(api, this)
        store.loadAccount("/a"); advanceUntilIdle()
        assertEquals(1, store.state.value.calls.unseen)
        assertEquals(2, store.state.value.calls.shown.size)
        store.showView(LeftView.CALLS); advanceUntilIdle()
        assertEquals(LeftView.CALLS, store.state.value.leftView)
        assertEquals(0, store.state.value.calls.unseen)
        assertEquals(listOf(100L), api.seenUpTo)
        store.callFilter("missed"); advanceUntilIdle()
        assertEquals(listOf("c1"), store.state.value.calls.entries.map { it.id })
        store.deleteCall(store.state.value.calls.entries.single()); advanceUntilIdle()
        assertTrue(store.state.value.calls.entries.isEmpty())
        store.reset()
    }

    @Test fun aMissedCallIsASystemLineInTheGeneralSubject() = runTest {
        val api = Phase1b()
        api.rows = listOf(Message("m1", "hi", from = "/k/stranger", peer = "/k/stranger", peerHandle = "/demo/builder", receivedAt = 90, read = true))
        val store = InboxStore(api, this)
        store.loadAccount("/a"); advanceUntilIdle()
        store.selectPeer("/demo/builder")
        val lines = store.state.value.subject!!.messages.filter { it.system }
        assertEquals(1, lines.size)
        assertTrue(lines.single().body.startsWith("Missed video call · "))
        assertEquals("a system line is not unread", 0, store.state.value.conversation!!.unread)
        store.reset()
    }

    @Test fun filteredMailIsGroupedAndAcceptMovesItToTheInbox() = runTest {
        val api = Phase1b()
        val store = InboxStore(api, this)
        store.loadAccount("/a"); advanceUntilIdle()
        val group = store.state.value.filteredGroups.single()
        assertEquals("/k/spam", group.peer)
        assertEquals(2, group.messages.size)
        assertTrue("filtered mail is not a conversation", store.state.value.conversations.none { it.peer == "/k/spam" })
        store.acceptFiltered(group); advanceUntilIdle()
        assertEquals(listOf("/k/spam"), api.accepted)
        assertTrue(store.state.value.filteredGroups.isEmpty())
        assertEquals("/k/spam", store.state.value.selectedPeer)
        assertTrue(store.state.value.conversations.any { it.peer == "/k/spam" })
        store.reset()
    }

    @Test fun blockingFromFilteredReportsTheLastMessageAndHidesTheGroup() = runTest {
        val api = Phase1b()
        val store = InboxStore(api, this)
        store.loadAccount("/a"); advanceUntilIdle()
        val group = store.state.value.filteredGroups.single()
        store.blockSender(group.peer, report = true, messageId = group.messages.last().id); advanceUntilIdle()
        assertEquals(Contact("/k/spam", null, "block", "review", emptyList()), api.saved.single())
        assertEquals(listOf("message:f2"), api.reports)
        assertTrue("a blocked sender leaves Filtered", store.state.value.filteredGroups.isEmpty())
        assertEquals("Blocked and reported as spam.", store.state.value.error)
        store.reset()
    }

    @Test fun blockingEveryoneAtANamespaceUsesTheWildcardAndACallReport() = runTest {
        val api = Phase1b()
        val store = InboxStore(api, this)
        store.loadAccount("/a"); advanceUntilIdle()
        store.blockSender("/spamco/agent", wide = true, report = true, callId = "c1"); advanceUntilIdle()
        assertEquals("/spamco/*", api.saved.single().peer)
        assertEquals(listOf("call:c1"), api.reports)
        assertNull(wildcardFor("/k/abc"))
        assertNull(wildcardFor("/github/someone"))
        assertEquals("/bekir/*", wildcardFor("/bekir"))
        store.reset()
    }

    @Test fun theBlockedConversationShowsTheBanner() = runTest {
        val api = object : Phase1b() { override suspend fun contacts(identity: String) = ContactsResponse(listOf(Contact("/demo/builder", admission = "block"))) }
        api.rows = listOf(Message("m1", "hi", peerHandle = "/demo/builder", receivedAt = 1))
        val store = InboxStore(api, this)
        store.loadAccount("/a"); advanceUntilIdle()
        store.selectPeer("/demo/builder")
        assertTrue(store.state.value.blockedPeer)
        store.reset()
    }

    @Test fun policyChangesRollBackWhenTheServerRefuses() = runTest {
        val api = object : Phase1b() { override suspend fun setPolicy(identity: String, field: String, value: String): FilterPolicy = throw ApiException(400, "invalid_policy") }
        val store = InboxStore(api, this)
        store.loadAccount("/a"); advanceUntilIdle()
        store.loadPolicy(); advanceUntilIdle()
        assertEquals("ring", store.state.value.policy!!.unknownCalls)
        store.setPolicy("unknown_calls", "silence"); advanceUntilIdle()
        assertEquals("ring", store.state.value.policy!!.unknownCalls)
        assertNotNull(store.state.value.policyError)
        store.reset()
    }

    @Test fun expiredDirectMessagesLeaveOnTheDevicesOwnClock() = runTest {
        val api = Phase1b(emptyList())
        api.rows = listOf(Message("brief", "gone soon", peer = "/p", receivedAt = 1, expiresAt = 1_000), Message("kept", "stays", peer = "/p", receivedAt = 2))
        var clock = 900L
        val store = InboxStore(api, this, clock = { clock })
        store.loadAccount("/a"); advanceUntilIdle()
        assertEquals(2, store.state.value.messages.size)
        clock = 1_001
        store.pruneExpired()
        assertEquals(listOf("kept"), store.state.value.messages.map { it.id })
        store.reset()
    }

    @Test fun callLogEntriesDescribeThemselvesLikeTheWebInbox() {
        val answered = CallLogEntry("x", "outgoing", "answered", "audio", startedAt = 1, answeredAt = 10, endedAt = 135)
        assertEquals("Outgoing · Voice · 2:05", answered.summary)
        assertEquals("Missed · Video · 4 people", CallLogEntry("y", "incoming", "missed", "video", participants = listOf("a", "b", "c", "d"), groupId = "grp_x").summary)
        assertTrue(CallLogEntry("z", "incoming", "cancelled").missed)
        assertEquals("1:01:01", callDuration(3661))
    }

    @Test fun theClientSpeaksThePhase1bWireFormat() = runBlocking {
        val server = MockWebServer(); server.start()
        try {
            val client = PostboxClient(object : TokenProvider { override suspend fun token(rejected: String?) = "t" }, server.url("/"), allowLoopbackForTests = true)
            server.enqueue(MockResponse().setBody("""{"features":["call_history","avatars"],"api_version":"1.1"}"""))
            assertEquals(setOf("call_history", "avatars"), withTimeout(5000) { client.capabilities() }.enabled)
            server.takeRequest().let { assertEquals("/v1/capabilities", it.path) }
            server.enqueue(MockResponse().setResponseCode(404).setBody("""{"error":"not_found"}"""))
            assertTrue("an older postbox means everything is off", client.capabilities().enabled.isEmpty())
            server.takeRequest()

            server.enqueue(MockResponse().setBody("""{"entries":[{"id":"c1","direction":"incoming","outcome":"missed","kind":"video","peer":"/k/x","started_at":5}],"unseen_missed":1,"next_before":4}"""))
            val page = client.callLog("/k/me", "missed", before = 9)
            assertEquals(1, page.unseenMissed); assertEquals(4L, page.nextBefore); assertTrue(page.entries!!.single().missed)
            server.takeRequest().let { assertEquals("/v1/calls/log?identity=%2Fk%2Fme&filter=missed&limit=50&before=9", it.path) }

            server.enqueue(MockResponse().setBody("""{"unknown_calls":"silence","call_history_days":30}"""))
            val saved = client.setPolicy("/k/me", "call_history_days", "30")
            assertEquals(30, saved.callHistoryDays)
            server.takeRequest().let { assertEquals("PUT", it.method); assertEquals("""{"identity":"/k/me","call_history_days":30}""", it.body.readUtf8()) }

            server.enqueue(MockResponse().setBody("{}"))
            client.unblock("/k/me", "/spamco/*")
            server.takeRequest().let { assertEquals("DELETE", it.method); assertEquals("/v1/contacts?identity=%2Fk%2Fme&peer=%2Fspamco%2F*", it.path) }

            server.enqueue(MockResponse().setBody("""{"avatars":{"/bekir":{"id":"av1","version":2,"hash":"h1","urls":{"128":"/v1/avatar-blobs/av1/2/128.webp"}},"/k/none":null}}"""))
            val pictures = client.avatars(listOf("/bekir", "/k/none", "/x/*"))
            assertEquals("/v1/avatar-blobs/av1/2/128.webp", pictures["/bekir"]!!.path())
            assertNull(pictures["/k/none"])
            server.takeRequest().let { assertEquals("/v1/avatars?addresses=%2Fbekir%2C%2Fk%2Fnone", it.path) }

            server.enqueue(MockResponse().setBody("""{"avatar":{"id":"av1","version":3,"visibility":"known","hash":"h2","urls":{"256":"/v1/avatar-blobs/av1/3/256.webp"}}}"""))
            val mine = client.setAvatar("/k/me", byteArrayOf(1, 2, 3), "image/jpeg", CropRect(10, 20, 300), "known", all = true)
            assertEquals("h2", mine!!.hash)
            server.takeRequest().let {
                assertEquals("PUT", it.method)
                assertEquals("/v1/profile/avatar?identity=%2Fk%2Fme&crop=10%2C20%2C300%2C300&visibility=known&all=1", it.path)
                assertEquals("image/jpeg", it.getHeader("Content-Type"))
            }

            server.enqueue(MockResponse().setBody("""{"ok":true,"ephemeral_secs":3600}"""))
            client.setThreadTimer("/k/me", "t_1", 3600)
            server.takeRequest().let { assertEquals("PATCH", it.method); assertEquals("/v1/threads/t_1?identity=%2Fk%2Fme", it.path); assertEquals("""{"identity":"/k/me","ephemeral_secs":3600}""", it.body.readUtf8()) }
            server.enqueue(MockResponse().setBody("""{"ok":true}"""))
            client.setThreadTimer("/k/me", "t_1", null)
            assertEquals("""{"identity":"/k/me","ephemeral_secs":null}""", server.takeRequest().body.readUtf8())
        } finally { server.shutdown() }
    }

    @Test fun groupRoutesCarryTheMailboxAndDeviceHeaders() = runBlocking {
        val server = MockWebServer(); server.start()
        try {
            val client = PostboxClient(object : TokenProvider { override suspend fun token(rejected: String?) = "t" }, server.url("/"), allowLoopbackForTests = true)
            server.enqueue(MockResponse().setResponseCode(409).setBody("""{"error":"epoch_conflict","current_epoch":7}"""))
            try { client.groupJson("/k/me", "POST", "/v1/groups/grp_x/messages", null, "android-abc"); fail() }
            catch (e: ApiException) { assertEquals("epoch_conflict", e.code); assertEquals("7", e.problem!!["current_epoch"].toString()) }
            server.takeRequest().let {
                assertEquals("/k/me", it.getHeader("x-pigeonpost-identity"))
                assertEquals("android-abc", it.getHeader("x-pigeonpost-device"))
                assertEquals("{}", it.body.readUtf8())
            }
        } finally { server.shutdown() }
    }
}

class GroupLinksTest {
    @Test fun onlyInboxGroupLinksAreAccepted() {
        val id = "grp_abcdefghijklmnopqrstuvwxyz"
        assertEquals(id to "tok-en_1", dev.pigeonpost.core.groups.GroupLinks.parse("https://inbox.pigeonpost.dev/g/$id#tok-en_1"))
        assertEquals(id to null, dev.pigeonpost.core.groups.GroupLinks.parse("https://inbox.pigeonpost.dev/g/$id/"))
        assertNull(dev.pigeonpost.core.groups.GroupLinks.parse("http://inbox.pigeonpost.dev/g/$id#t"))
        assertNull(dev.pigeonpost.core.groups.GroupLinks.parse("https://evil.example/g/$id#t"))
        assertNull(dev.pigeonpost.core.groups.GroupLinks.parse("https://inbox.pigeonpost.dev/g/grp_short#t"))
        assertNull(dev.pigeonpost.core.groups.GroupLinks.parse("https://user@inbox.pigeonpost.dev/g/$id#t"))
        assertEquals(id to null, dev.pigeonpost.core.groups.GroupLinks.parse("https://inbox.pigeonpost.dev/g/$id#bad<token>"))
    }
}
