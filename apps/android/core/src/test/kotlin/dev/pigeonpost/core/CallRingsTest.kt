package dev.pigeonpost.core

import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class CallRingsTest {
    private val me = "/k/alice0000000000000000000a"
    private val now = 1_792_500_000L
    private val token = "A".repeat(40) + "b_-"
    private fun ring(vararg changes: Pair<String, String?>): Map<String, String> {
        val data = mutableMapOf("type" to "call_ring", "room" to "r0om1d2xyzABCDEFGH-_12", "code" to "ABC234", "kind" to "video",
            "ring_token" to token, "from_address" to "/k/bob00000000000000000000000b", "from_handle" to "/bob", "from_display" to "Bob",
            "to" to "/alice", "identity" to me, "expires_at" to (now + 45).toString())
        changes.forEach { (key, value) -> if (value == null) data.remove(key) else data[key] = value }
        return data
    }

    @Test fun aWellFormedRingForThisMailboxIsShown() {
        val value = CallRings.ring(ring(), me, now)!!
        assertEquals("r0om1d2xyzABCDEFGH-_12", value.room)
        assertEquals(token, value.ringToken)
        assertTrue(value.video)
        assertEquals("Bob", value.callerName)
        assertEquals("/bob", value.callerDetail)
        assertEquals(now + 45, value.expiresAt)
        assertEquals("ABC234", value.code)
    }

    @Test fun ringsForAnotherMailboxOrWhileSignedOutAreIgnored() {
        assertNull(CallRings.ring(ring(), "/k/someone0000000000000000z", now))
        assertNull(CallRings.ring(ring(), null, now))
        assertNull(CallRings.ring(ring("identity" to null), me, now))
    }

    @Test fun otherMessageTypesAreNotRings() {
        assertNull(CallRings.ring(ring("type" to "message"), me, now))
        assertNull(CallRings.ring(ring("type" to null), me, now))
    }

    @Test fun malformedFieldsAreRejected() {
        assertNull(CallRings.ring(ring("ring_token" to "short"), me, now))
        assertNull(CallRings.ring(ring("ring_token" to "A".repeat(42) + "="), me, now))
        assertNull(CallRings.ring(ring("ring_token" to "A".repeat(44)), me, now))
        assertNull(CallRings.ring(ring("room" to "short"), me, now))
        assertNull(CallRings.ring(ring("room" to "a".repeat(65)), me, now))
        assertNull(CallRings.ring(ring("room" to "../../etc/passwd"), me, now))
        assertNull(CallRings.ring(ring("kind" to "screen"), me, now))
        assertNull(CallRings.ring(ring("from_address" to "bob"), me, now))
        assertNull(CallRings.ring(ring("from_handle" to "not a handle"), me, now))
        assertNull(CallRings.ring(ring("to" to "nobody"), me, now))
        assertNull(CallRings.ring(ring("expires_at" to "soon"), me, now))
    }

    @Test fun lateOrImplausiblyLongRingsAreDropped() {
        assertNull(CallRings.ring(ring("expires_at" to now.toString()), me, now))
        assertNull(CallRings.ring(ring("expires_at" to (now - 1).toString()), me, now))
        assertNull(CallRings.ring(ring("expires_at" to (now + 3600).toString()), me, now))
        assertNotNull(CallRings.ring(ring("expires_at" to (now + 1).toString()), me, now))
    }

    @Test fun optionalFieldsMayBeMissing() {
        val value = CallRings.ring(ring("from_handle" to null, "from_display" to null, "code" to null), me, now)!!
        assertNull(value.code)
        assertEquals(displayName("/k/bob00000000000000000000000b"), value.callerName)
        assertEquals("/k/bob00000000000000000000000b", value.callerDetail)
        val handleOnly = CallRings.ring(ring("from_display" to ""), me, now)!!
        assertEquals("/bob", handleOnly.callerName)
        assertNull(handleOnly.callerDetail)
        assertNull(CallRings.ring(ring("code" to "<script>"), me, now)!!.code)
    }

    @Test fun displayNamesAreCleanedForTheLockScreen() {
        assertEquals("Bob evil", CallRings.cleanDisplay("Bob‮\u0000 evil\n"))
        assertEquals(60, CallRings.cleanDisplay("x".repeat(200))!!.length)
        assertNull(CallRings.cleanDisplay("‏\u0007 "))
        assertNull(CallRings.cleanDisplay(null))
    }

    @Test fun cancelsMatchTheSignedInMailbox() {
        val data = mapOf("type" to "call_cancel", "room" to "r0om1d2xyzABCDEFGH-_12", "reason" to "answered_elsewhere", "identity" to me)
        assertEquals(CallCancel("r0om1d2xyzABCDEFGH-_12", "answered_elsewhere", me), CallRings.cancel(data, me))
        assertEquals("ended", CallRings.cancel(data + ("reason" to "weird"), me)!!.reason)
        assertNull(CallRings.cancel(data, "/k/other"))
        assertNull(CallRings.cancel(data + ("room" to "x"), me))
        assertNull(CallRings.cancel(data + ("type" to "call_ring"), me))
    }

    @Test fun ringStateAndDeclineUseOnlyTheRingToken() {
        val server = MockWebServer(); server.start()
        try {
            val client = RingClient(server.url("/").toString())
            val room = "r0om1d2xyzABCDEFGH-_12"
            server.enqueue(MockResponse().setBody("""{"state":"ringing","kind":"audio","expires_at":1}"""))
            assertEquals(RingState(true, null), client.state(room, token))
            val first = server.takeRequest()
            assertEquals("/calls/v1/rings/$room", first.path)
            assertEquals(token, first.getHeader("x-ring-token"))
            assertNull(first.getHeader("authorization"))

            server.enqueue(MockResponse().setBody("""{"state":"ended","reason":"declined"}"""))
            assertEquals(RingState(false, "declined"), client.state(room, token))
            server.takeRequest()
            server.enqueue(MockResponse().setResponseCode(404).setBody("""{"error":"no_ring"}"""))
            assertEquals(RingState(false, "ended"), client.state(room, token))
            server.takeRequest()
            server.enqueue(MockResponse().setResponseCode(503))
            assertNull(client.state(room, token))
            server.takeRequest()

            server.enqueue(MockResponse().setBody("""{"ok":true}"""))
            assertTrue(client.decline(room, token))
            val decline = server.takeRequest()
            assertEquals("POST", decline.method)
            assertEquals("/calls/v1/rings/$room/decline", decline.path)
            assertEquals("""{"ring_token":"$token","reason":"declined"}""", decline.body.readUtf8())
            assertNull(decline.getHeader("authorization"))

            assertFalse(client.decline("../x", token))
            assertEquals(5, server.requestCount)
        } finally { server.shutdown() }
    }
}
