package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import okio.Buffer
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import java.io.IOException
import java.net.InetAddress
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

class CalendarClientTest {
    private class Source : TokenProvider {
        @Volatile var grant = AuthorizationLease()
        override val authorizationLease get() = grant
        var calls = 0; var invalidations = 0
        override suspend fun token(rejected: String?): String { calls++; return if (rejected == null) "fixture-first" else "fixture-renewed" }
        override suspend fun invalidate(rejected: String) { invalidations++ }
    }
    private lateinit var source: Source
    private lateinit var server: MockWebServer
    private lateinit var postbox: PostboxClient
    private lateinit var client: CalendarClient
    @Before fun setup() {
        source = Source()
        server = MockWebServer().also { it.start(InetAddress.getByName("127.0.0.1"), 0) }
        postbox = PostboxClient(source, server.url("/").newBuilder().host("127.0.0.1").build(), allowLoopbackForTests = true)
        client = CalendarClient(postbox, "/alice", source.grant.child())
    }
    @After fun teardown() { client.close(); server.shutdown() }
    private fun response(text: String) = MockResponse().setHeader("Content-Type", "application/json").setBody(text)
    private fun event(id: String = "evt_one") = response("""{"event":{"id":"$id","revision":7,"title":"Review","meeting":{"enabled":true,"future_policy":"keep"}}}""")
    private fun payload(vararg values: Pair<String, JsonElement>) = CalendarWritePayload(JsonObject(mapOf(*values)))
    private fun created() = payload("title" to JsonPrimitive("Review"), "request_key" to JsonPrimitive("fixture-create"))
    private suspend inline fun <reified T : Throwable> fails(noinline work: suspend () -> Unit): T {
        val failure = runCatching { work() }.exceptionOrNull()
        assertTrue("Expected ${T::class.simpleName}", failure is T)
        return failure as T
    }
    private fun RecordedRequest.json() = WireJson.parseToJsonElement(body.readUtf8()).jsonObject

    @Test fun listingAnEmptyCalendarNeverCreatesTheDefault() = runBlocking<Unit> {
        server.enqueue(response("""{"calendars":[]}"""))
        assertTrue(client.calendars().isEmpty())
        val request = server.takeRequest()
        assertEquals("GET", request.method); assertEquals("/v1/calendars", request.path)
        assertEquals("/alice", request.getHeader("x-pigeonpost-identity"))
        assertEquals(1, server.requestCount)
    }
    @Test fun creationKeepsOneEncodingAcrossRenewalAndExplicitRetry() = runBlocking<Unit> {
        val meeting = linkedMapOf<String, JsonElement>("enabled" to JsonPrimitive(true))
        val fields = linkedMapOf<String, JsonElement>("title" to JsonPrimitive("Review"), "request_key" to JsonPrimitive("fixture-create"), "meeting" to JsonObject(meeting))
        val body = CalendarWritePayload(JsonObject(fields))
        fields["title"] = JsonPrimitive("Changed after freezing"); meeting["enabled"] = JsonPrimitive(false)
        body.bytes().fill(0) // The caller cannot mutate the retained bytes either.
        server.enqueue(MockResponse().setResponseCode(401)); server.enqueue(event()); server.enqueue(event())
        assertEquals("evt_one", client.createEvent(body).event.id)
        assertEquals("evt_one", client.createEvent(body).event.id)
        val requests = List(3) { server.takeRequest() }
        val first = requests.first().body.readUtf8()
        assertEquals("""{"meeting":{"enabled":true},"request_key":"fixture-create","title":"Review"}""", first)
        requests.drop(1).forEach { assertEquals(first, it.body.readUtf8()) }
        assertEquals("Bearer fixture-first", requests[0].getHeader("Authorization"))
        assertEquals("Bearer fixture-renewed", requests[1].getHeader("Authorization"))
        assertEquals(body, CalendarWritePayload.restore(first))
        fails<IllegalArgumentException> { CalendarWritePayload.restore("{ \"title\":\"Review\" }") }
    }
    @Test fun anAcceptedPostWithALostResponseIsNeverAutomaticallyRepeated() = runBlocking<Unit> {
        server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST))
        fails<IOException> { client.createEvent(created()) }
        assertEquals(1, server.requestCount)
        val first = server.takeRequest().body.readUtf8()
        server.enqueue(event())
        client.createEvent(created())
        assertEquals(first, server.takeRequest().body.readUtf8())
        assertEquals(2, server.requestCount)
    }
    @Test fun pathSyntaxAndNonMemberRoutesAreRefusedBeforeNetwork() = runBlocking<Unit> {
        for (id in listOf("../identities", "%2e%2e", "cal_one/other", "https://fixture.invalid", "bad?query", "a#b"))
            fails<ApiException> { client.detail(CalendarEventKey(id)) }
        fails<IllegalArgumentException> { postbox.calendarRequest("/alice", "GET", "/v1/calendar/%2e%2e/events") }
        fails<IllegalArgumentException> { postbox.calendarRequest("/alice", "GET", "/v1/calendar/guest/recover") }
        fails<IllegalArgumentException> { postbox.calendarRequest("/alice", "GET", "/v1/calendar/feed/secret.ics") }
        fails<IllegalArgumentException> { postbox.calendarRequest("/alice", "GET", "/v1/calendars", body = byteArrayOf(1)) }
        assertEquals(0, server.requestCount); assertEquals(0, source.calls)
    }
    @Test fun occurrenceWindowsCannotOverflowOrSilentlyExpand() = runBlocking<Unit> {
        for ((from, to) in listOf(10L to 10L, 10L to 9L, Long.MIN_VALUE to Long.MAX_VALUE, 0L to 62 * 86400L + 1))
            assertEquals("window_too_large", fails<ApiException> { client.occurrences(from, to) }.code)
        server.enqueue(response("""{"occurrences":[]}"""))
        assertTrue(client.occurrences(0, 62 * 86400L, listOf("cal_one", "cal_two")).isEmpty())
        val target = server.takeRequest().requestUrl!!
        assertEquals("cal_one,cal_two", target.queryParameter("calendars")); assertEquals("invitations", target.queryParameter("include"))
    }
    @Test fun cancellationUsesTheReviewedIntegerRevisionAndOpaqueOccurrence() = runBlocking<Unit> {
        server.enqueue(response("{}"))
        client.cancelEvent(CalendarEventKey("evt_one", "occ_original_key"), 9, CalendarEventScope.THIS)
        val request = server.takeRequest(); val body = request.json()
        assertEquals("POST", request.method); assertEquals("/v1/calendar/events/evt_one/cancel", request.path)
        assertEquals(9L, body["expected_revision"]!!.jsonPrimitive.long); assertFalse(body["expected_revision"]!!.jsonPrimitive.isString)
        assertEquals("this", body["scope"]!!.jsonPrimitive.content); assertEquals("occ_original_key", body["occurrence_id"]!!.jsonPrimitive.content)
    }
    @Test fun occurrenceChangesAndRevisionsAreValidatedBeforeSending() = runBlocking<Unit> {
        fails<IllegalArgumentException> { client.cancelEvent(CalendarEventKey("evt_one"), 1, CalendarEventScope.THIS) }
        fails<IllegalArgumentException> { client.updateEvent("evt_one", payload("scope" to JsonPrimitive("all"), "expected_revision" to JsonPrimitive("7"))) }
        fails<IllegalArgumentException> { client.updateEvent("evt_one", payload("scope" to JsonPrimitive("this"), "expected_revision" to JsonPrimitive(7), "occurrence_id" to JsonNull)) }
        fails<IllegalArgumentException> { client.updateEvent("evt_one", payload("scope" to JsonPrimitive("following"), "expected_revision" to JsonPrimitive(-1))) }
        assertEquals(0, server.requestCount)
    }
    @Test fun movingOneOccurrenceRetainsItsIdentityAndUnknownMeetingPolicy() = runBlocking<Unit> {
        server.enqueue(response("""{"event":{"id":"evt_one","revision":7,"meeting":{"enabled":true,"future_policy":"keep"}},"occurrence":{"occurrence_id":"occ_original_key","start":"2026-10-23T09:00"}}"""))
        val detail = client.detail(CalendarEventKey("evt_one", "occ_original_key"))
        assertEquals("keep", detail.original["meeting"]!!.jsonObject["future_policy"]!!.jsonPrimitive.content)
        assertEquals("occ_original_key", detail.occurrence!!.occurrenceId)
        assertEquals("occ_original_key", server.takeRequest().requestUrl!!.queryParameter("occurrence_id"))
    }
    @Test fun aDetailForAnotherEventOrOccurrenceIsRejected() = runBlocking<Unit> {
        server.enqueue(event("evt_other"))
        assertEquals("bad_response", fails<ApiException> { client.detail(CalendarEventKey("evt_one")) }.code)
        server.enqueue(response("""{"event":{"id":"evt_one"},"occurrence":{"occurrence_id":"occ_other"}}"""))
        assertEquals("bad_response", fails<ApiException> { client.detail(CalendarEventKey("evt_one", "occ_original_key")) }.code)
    }
    @Test fun aFollowingEditAcceptsOnlyItsDeclaredSplit() = runBlocking<Unit> {
        val body = payload("scope" to JsonPrimitive("following"), "occurrence_id" to JsonPrimitive("occ_original"), "expected_revision" to JsonPrimitive(7))
        server.enqueue(response("""{"event":{"id":"evt_new"},"split_from":{"event_id":"evt_one","revision":8}}"""))
        assertEquals("evt_new", client.updateEvent("evt_one", body).event.id)
        server.enqueue(event("evt_unrelated"))
        assertEquals("bad_response", fails<ApiException> { client.updateEvent("evt_one", body) }.code)
    }
    @Test fun aPrivacyOnlySettingsChangeDoesNotResetTimezoneOrWorkingHours() = runBlocking<Unit> {
        server.enqueue(response("""{"previews":false,"timezone":"Europe/Istanbul","week_start":1}"""))
        assertFalse(client.saveSettings(buildJsonObject { put("previews", false) }).previews!!)
        assertEquals(setOf("previews"), server.takeRequest().json().keys)
    }
    @Test fun calendarExportIsRawIcsWithItsLineEndings() = runBlocking<Unit> {
        val bytes = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".toByteArray()
        server.enqueue(MockResponse().setHeader("Content-Type", "text/calendar").setBody(Buffer().write(bytes)))
        assertArrayEquals(bytes, client.ics("evt_one"))
        val request = server.takeRequest()
        assertEquals("/v1/calendar/events/evt_one.ics", request.path); assertEquals("text/calendar", request.getHeader("Accept"))
    }
    @Test fun invitationsKeepPendingAndFilteredSeparateWithoutContactWrites() = runBlocking<Unit> {
        repeat(2) { server.enqueue(response("""{"invitations":[]}""")) }
        client.invitations(false); client.invitations(true)
        assertEquals("/v1/calendar/invitations?state=pending", server.takeRequest().path)
        assertEquals("/v1/calendar/invitations?state=filtered", server.takeRequest().path)
        assertEquals(2, server.requestCount)
    }
    @Test fun feedControlsUseTheMemberFeedsRouteAndDoNotInventACachedLink() = runBlocking<Unit> {
        server.enqueue(response("""{"enabled":true,"projection":"busy"}""")); server.enqueue(response("""{"enabled":false}"""))
        assertNull(client.feed("cal_one").url); client.setFeed("cal_one", false, "busy")
        assertEquals("/v1/calendar/feeds/cal_one", server.takeRequest().path)
        val revoke = server.takeRequest(); assertEquals("POST", revoke.method); assertFalse(revoke.json()["enabled"]!!.jsonPrimitive.boolean)
    }
    @Test fun memberRemindersCannotRequestExternalDeliveryOrInvalidMinutes() = runBlocking<Unit> {
        for (bad in listOf(listOf(CalendarReminder(10, "external")), listOf(CalendarReminder(-1)), List(6) { CalendarReminder(it) }))
            fails<IllegalArgumentException> { client.reminders("evt_one", bad) }
        server.enqueue(response("{}")); client.reminders("evt_one", listOf(CalendarReminder(10)))
        assertEquals("push", server.takeRequest().json()["reminders"]!!.jsonArray.single().jsonObject["channel"]!!.jsonPrimitive.content)
        assertEquals(1, server.requestCount)
    }
    @Test fun unknownOrMalformedAvailabilityNeverMeansFree() = runBlocking<Unit> {
        server.enqueue(response("""{"busy":{"/alice":[],"/bob":"unknown","/carol":[{"start":20,"end":10}]}}"""))
        val result = client.freeBusy(listOf("/alice", "/bob", "/carol", "/dave"), 0, 100)
        assertEquals(CalendarAvailability.Known(emptyList()), result["/alice"])
        for (name in listOf("/bob", "/carol", "/dave")) assertEquals(CalendarAvailability.Unknown, result[name])
    }
    @Test fun importPreviewSendsOnlyLocalBytesAndNeverFetchesAnIcsUrl() = runBlocking<Unit> {
        val bytes = "BEGIN:VCALENDAR\r\nURL:https://never-fetch.invalid/private.ics\r\nEND:VCALENDAR\r\n".toByteArray()
        server.enqueue(response("""{"draft_id":"imp_fixture","warnings":["attendees_dropped"]}"""))
        assertEquals("imp_fixture", client.previewImport(bytes, "cal_one").draftId)
        val request = server.takeRequest()
        assertEquals("/v1/calendar/import/preview?calendar_id=cal_one", request.path)
        assertEquals("text/calendar", request.getHeader("Content-Type")); assertArrayEquals(bytes, request.body.readByteArray())
        fails<IllegalArgumentException> { client.previewImport(ByteArray(512 * 1024 + 1)) }
        assertEquals(1, server.requestCount)
    }
    @Test fun importConfirmationUsesUniqueCanonicalIndicesAndAnExplicitDestination() = runBlocking<Unit> {
        server.enqueue(response("""{"created":[]}"""))
        client.confirmImport("imp_fixture", listOf(2, 0, 2, 1), "cal_one")
        val request = server.takeRequest(); val body = request.json()
        assertEquals("/v1/calendar/import/imp_fixture/confirm", request.path)
        assertEquals(listOf(0, 1, 2), body["select"]!!.jsonArray.map { it.jsonPrimitive.int })
        assertEquals("cal_one", body["calendar_id"]!!.jsonPrimitive.content)
        fails<IllegalArgumentException> { client.confirmImport("imp_fixture", List(201) { it }) }
    }
    @Test fun aRetiredMailboxCannotBorrowANewActorOnTheSameLogin() = runBlocking<Unit> {
        val old = source.grant.child().apply { retire() }
        withContext(AuthorizationContext(old)) { fails<AuthorizationRetired> { client.calendars() } }
        withContext(AuthorizationContext(AuthorizationLease())) { fails<AuthorizationRetired> { client.calendars() } }
        assertEquals(0, source.calls); assertEquals(0, server.requestCount)
        server.enqueue(response("""{"calendars":[]}"""))
        withContext(AuthorizationContext(source.grant)) { client.calendars() }
    }
    @Test fun aLateCalendarResponseCannotBeReturnedAfterRetirement() = runBlocking<Unit> {
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse { entered.countDown(); check(release.await(3, TimeUnit.SECONDS)); return response("""{"calendars":[]}""") }
        }
        try {
            val result = async { runCatching { client.calendars() } }
            assertTrue(withContext(Dispatchers.IO) { entered.await(3, TimeUnit.SECONDS) })
            client.close(); release.countDown()
            assertTrue(result.await().exceptionOrNull() is AuthorizationRetired)
            assertTrue(source.grant.isValid); assertEquals(0, source.invalidations)
        } finally { release.countDown() }
    }
    @Test fun readRecoveryKeepsTheOriginalCalendarAuthorization() = runBlocking<Unit> {
        val requests = AtomicInteger()
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                if (requests.incrementAndGet() == 1) {
                    source.grant.retire(); source.grant = AuthorizationLease()
                    return MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST)
                }
                return response("""{"calendars":[]}""")
            }
        }
        fails<AuthorizationRetired> { client.calendars() }
        assertEquals(1, server.requestCount); assertEquals(1, source.calls)
    }
}
