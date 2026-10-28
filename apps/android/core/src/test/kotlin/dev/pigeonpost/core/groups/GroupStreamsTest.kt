package dev.pigeonpost.core.groups

import dev.pigeonpost.core.WireJson
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.*
import org.junit.Test

class GroupStreamsTest {
    private val me = "/k/me000000000000000000000a"
    private val host = "/k/host00000000000000000000"
    private val sid = "str_" + "a".repeat(26)
    private val gid = "grp_" + "b".repeat(26)
    private val live = GroupStream(id = sid, groupId = gid, title = "Sprint demo", by = host, state = "live", revision = 1)
    private fun group(role: String = "member", state: String = "active", policy: String = "admins") =
        Group(id = gid, streamPolicy = policy, my = GroupMine(role = role, state = state))

    @Test fun theWireFormatDecodesAsDocumented() {
        val body = """{"streams":[{"id":"$sid","group_id":"$gid","title":"Sprint demo","sources":["camera"],"by":"$host","by_handle":"/jane",
            "state":"live","started_at":1793113200,"ends_by":1793127600,"viewers":3,"peak_viewers":4,"stage":[{"address":"$me","state":"invited"}],"max_viewers":190,"revision":2}]}"""
        val s = WireJson.decodeFromString<GroupStreamsResponse>(body).streams!!.single()
        assertEquals("invited", s.stageOf(me)); assertNull(s.stageOf(host)); assertTrue(s.live); assertEquals(3, s.viewers)
        val g = WireJson.decodeFromString<Group>("""{"id":"$gid","stream_policy":"everyone"}""")
        assertEquals("everyone", g.streamPolicy)
        assertEquals("admins", WireJson.decodeFromString<Group>("""{"id":"$gid"}""").streamPolicy)
    }

    @Test fun goingLiveFollowsTheGroupsPolicyAndRoles() {
        assertFalse(GroupStreams.canGoLive(group("member")))
        assertTrue(GroupStreams.canGoLive(group("admin")))
        assertTrue(GroupStreams.canGoLive(group("owner")))
        assertTrue(GroupStreams.canGoLive(group("member", policy = "everyone")))
        assertFalse(GroupStreams.canGoLive(group("restricted", policy = "everyone")))
        assertFalse(GroupStreams.canGoLive(group("owner", state = "invited")))
    }

    @Test fun theBannerSaysWhoIsLiveAndWhatTheButtonDoes() {
        val name = { a: String? -> if (a == host) "Jane" else "?" }
        assertEquals("Jane is live: Sprint demo", GroupStreams.bannerText(live, me, name))
        assertEquals("You are live: Sprint demo", GroupStreams.bannerText(live.copy(by = me), me, name))
        assertEquals("Watch", GroupStreams.actionLabel(live, me, null))
        assertEquals("Open", GroupStreams.actionLabel(live.copy(by = me), me, null))
        assertEquals("Return", GroupStreams.actionLabel(live, me, LiveSession(sid, gid, "viewer")))
        assertNull(GroupStreams.stageNote(live, me, null, name))
        val invited = live.copy(stage = listOf(StreamStage(me, "invited")))
        assertTrue(GroupStreams.stageNote(invited, me, null, name)!!.startsWith("Jane invited you onto the stage. Watch to answer"))
        assertEquals("Jane invited you onto the stage.", GroupStreams.stageNote(invited, me, LiveSession(sid, gid, "invited"), name))
    }

    @Test fun titlesAreTrimmedAndBounded() {
        assertEquals("Demo", GroupStreams.title("  Demo "))
        assertNull(GroupStreams.title("   "))
        assertNull(GroupStreams.title("x".repeat(101)))
        assertNull(GroupStreams.title("a\u0000b"))
        assertEquals("x".repeat(100), GroupStreams.title("x".repeat(100)))
    }

    @Test fun refusalsAreExplainedAndCancellingIsSilent() {
        assertNull(GroupStreams.explain("cancelled"))
        assertTrue(GroupStreams.explain("needs_e2ee")!!.contains("WebView"))
        assertEquals("Only admins can go live in this group.", GroupStreams.explain("stream_restricted"))
        assertTrue(GroupStreams.explain("stream_capacity")!!.contains("very busy"))
        assertEquals("Couldn’t open the live stream. Try again.", GroupStreams.explain("something_new"))
    }

    private fun only(events: List<JsonObject>) = events.single()
    private fun stageOf(e: JsonObject) = e["stage"]!!.jsonObject

    @Test fun aStageInvitationReachesThePageOnce() {
        val invited = live.copy(stage = listOf(StreamStage(me, "invited")), revision = 2)
        val e = only(GroupStreams.pageEvents(live, StreamCheck.Live(invited), me, sid))
        assertEquals(sid, e["stream_id"]!!.jsonPrimitive.content)
        assertEquals("invited", stageOf(e)["state"]!!.jsonPrimitive.content)
        assertEquals(me, stageOf(e)["address"]!!.jsonPrimitive.content)
        assertEquals(host, stageOf(e)["by"]!!.jsonPrimitive.content)
        // Seen already: nothing more to say.
        assertTrue(GroupStreams.pageEvents(invited, StreamCheck.Live(invited), me, sid).isEmpty())
        // An invitation for someone else is not ours.
        assertTrue(GroupStreams.pageEvents(live, StreamCheck.Live(live.copy(stage = listOf(StreamStage(host + "x", "invited")))), me, sid).isEmpty())
        // Accepting (speaker) is the page's own doing.
        assertTrue(GroupStreams.pageEvents(invited, StreamCheck.Live(invited.copy(stage = listOf(StreamStage(me, "speaker")))), me, sid).isEmpty())
    }

    @Test fun beingTakenOffTheStageAndTheEndAreForwarded() {
        val speaking = live.copy(stage = listOf(StreamStage(me, "speaker")))
        val off = only(GroupStreams.pageEvents(speaking, StreamCheck.Live(live), me, sid))
        assertEquals("viewer", stageOf(off)["state"]!!.jsonPrimitive.content)
        val ended = only(GroupStreams.pageEvents(live, StreamCheck.Ended, me, sid))
        assertEquals(JsonPrimitive("ended"), ended["state"]); assertEquals(JsonPrimitive(sid), ended["stream_id"])
        assertEquals("ended", only(GroupStreams.pageEvents(live, StreamCheck.Live(live.copy(state = "ended")), me, sid))["state"]!!.jsonPrimitive.content)
        // A failed look changes nothing.
        assertTrue(GroupStreams.pageEvents(speaking, StreamCheck.Unknown, me, sid).isEmpty())
    }

    @Test fun theStreamNoticeReadsAsOnTheWeb() {
        val item = HistoryItem(kind = "notice", notice = "stream", actor = host,
            metadata = WireJson.parseToJsonElement("""{"stream_id":"$sid","duration":2040,"viewers":18,"reason":"host_stopped"}""").jsonObject)
        assertEquals("Jane streamed for 34 min · 18 viewers", groupNoticeText(item) { if (it == host) "Jane" else "?" })
        val short = item.copy(metadata = WireJson.parseToJsonElement("""{"duration":5,"viewers":1}""").jsonObject)
        assertEquals("Jane streamed for 1 min · 1 viewer", groupNoticeText(short) { "Jane" })
    }
}
