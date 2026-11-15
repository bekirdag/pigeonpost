package dev.pigeonpost.core.widgets

import org.junit.Assert.*
import org.junit.Test

class WidgetSnapshotTest {

    private val MAILBOX = "/bekir"
    private val GROUP = "grp_abcdefghijklmnopqrstuvwxyz"
    private val NOW = 1_800_000_000L

    private fun makeInput(
        showPreviews: Boolean = true,
        locked: Boolean = false,
        conversations: List<WidgetSourceConversation> = emptyList(),
        nests: WidgetSourceNests? = null,
        agenda: WidgetSourceAgenda? = null
    ) = WidgetInput(
        mailbox = MAILBOX,
        conversations = conversations,
        showPreviews = showPreviews,
        locked = locked,
        nests = nests,
        nestsEnabled = nests != null,
        agenda = agenda,
        calendarEnabled = agenda != null
    )

    @Test
    fun `previews off returns null preview even with lastText`() {
        val input = makeInput(
            showPreviews = false,
            conversations = listOf(
                WidgetSourceConversation(peer = "/alice", name = "Alice", at = 100, unread = 1, lastText = "Hello", group = false),
                WidgetSourceConversation(peer = GROUP, name = "Group", at = 100, unread = 1, lastText = "Hi", group = true)
            )
        )
        val snapshot = WidgetSnapshots.build(input, null, NOW)
        snapshot.conversations.forEach {
            assertNull(it.preview)
        }
    }

    @Test
    fun `previews on direct strips leading markers and truncates`() {
        val longText = "A".repeat(300)
        val input = makeInput(
            showPreviews = true,
            conversations = listOf(
                WidgetSourceConversation(peer = "/alice", name = "Alice", at = 100, unread = 1, lastText = "# Title\n\nBody", group = false),
                WidgetSourceConversation(peer = "/bob", name = "Bob", at = 100, unread = 1, lastText = "> Quoted\n\nContent", group = false),
                WidgetSourceConversation(peer = "/charlie", name = "Charlie", at = 100, unread = 1, lastText = longText, group = false)
            )
        )
        val snapshot = WidgetSnapshots.build(input, null, NOW)
        val alicePreview = snapshot.conversations.find { it.peer == "/alice" }?.preview
        val bobPreview = snapshot.conversations.find { it.peer == "/bob" }?.preview
        val charliePreview = snapshot.conversations.find { it.peer == "/charlie" }?.preview

        assertNotNull(alicePreview)
        assertFalse(alicePreview!!.startsWith("#"))
        assertFalse(alicePreview!!.startsWith(">"))

        assertNotNull(bobPreview)
        assertFalse(bobPreview!!.startsWith("#"))
        assertFalse(bobPreview!!.startsWith(">"))

        assertNotNull(charliePreview)
        assertEquals(WidgetSnapshots.PREVIEW_CHARS, charliePreview!!.length)
        assertTrue(charliePreview!!.endsWith("…"))
    }

    @Test
    fun `previews on group rows never get preview`() {
        val input = makeInput(
            showPreviews = true,
            conversations = listOf(
                WidgetSourceConversation(peer = GROUP, name = "Group", at = 100, unread = 1, lastText = "Secret", group = true)
            )
        )
        val snapshot = WidgetSnapshots.build(input, null, NOW)
        val groupConv = snapshot.conversations.firstOrNull { it.group }
        assertNotNull(groupConv)
        assertNull(groupConv!!.preview)
    }

    @Test
    fun `encode then decode round-trips equal`() {
        val input = makeInput(
            conversations = listOf(
                WidgetSourceConversation(peer = "/alice", name = "Alice", at = 100, unread = 1, lastText = "Hi", group = false)
            )
        )
        val original = WidgetSnapshots.build(input, null, NOW)
        val encoded = WidgetSnapshots.encode(original)
        val decoded = WidgetSnapshots.decode(encoded)
        assertEquals(original, decoded)
    }

    @Test
    fun `decode returns null for version 2`() {
        // Construct a snapshot with version 2
        val snapshot = WidgetSnapshot(
            version = 2,
            mailbox = MAILBOX,
            generatedAt = NOW,
            conversations = emptyList()
        )
        val encoded = WidgetSnapshots.encode(snapshot)
        assertNull(WidgetSnapshots.decode(encoded))
    }

    @Test
    fun `decode returns null for garbage text`() {
        assertNull(WidgetSnapshots.decode("garbage"))
    }

    @Test
    fun `decode returns null for null`() {
        assertNull(WidgetSnapshots.decode(null))
    }

    @Test
    fun `locked input empties conversations, nests, agenda but keeps counts`() {
        val nests = WidgetSourceNests(nests = listOf(
            WidgetNest(id = "1", label = "N1", status = "running")
        ), hoursLimit = 24)
        val events = listOf(WidgetSourceEvent(
            eventId = "e1", occurrenceId = "o1", title = "Event", startsAt = NOW + 1000, endsAt = null,
            allDay = false, startDate = null, endDate = null, location = null, status = null, myRsvp = null, meeting = false
        ))
        val agenda = WidgetSourceAgenda(zone = "UTC", events = events)

        val input = makeInput(
            locked = true,
            conversations = listOf(WidgetSourceConversation(peer = "/alice", name = "Alice", at = 100, unread = 1)),
            nests = nests,
            agenda = agenda
        )
        val snapshot = WidgetSnapshots.build(input, null, NOW)

        assertTrue(snapshot.conversations.isEmpty())
        assertEquals(1, snapshot.unread)

        assertNotNull(snapshot.nests)
        assertTrue(snapshot.nests!!.nests.isEmpty())
        assertEquals(1, snapshot.nests!!.count)

        assertNotNull(snapshot.agenda)
        assertTrue(snapshot.agenda!!.events.isEmpty())
        assertEquals(1, snapshot.agenda!!.count)
    }

    @Test
    fun `rows sorted newest first and capped`() {
        val convs = List(20) { i ->
            WidgetSourceConversation(
                peer = "/user$i",
                name = "User$i",
                at = i.toLong(),
                unread = 1,
                lastText = "Text",
                group = false
            )
        }
        val input = makeInput(conversations = convs)
        val snapshot = WidgetSnapshots.build(input, null, NOW)

        assertEquals(WidgetSnapshots.MAX_CONVERSATIONS, snapshot.conversations.size)
        val times = snapshot.conversations.map { it.at }
        assertTrue(times.sortedDescending() == times)
    }

    @Test
    fun `invalid peers are dropped`() {
        val input = makeInput(
            conversations = listOf(
                WidgetSourceConversation(peer = "not an address", name = "Bad", at = 100, unread = 1, group = false),
                WidgetSourceConversation(peer = "/valid", name = "Good", at = 100, unread = 1, group = false)
            )
        )
        val snapshot = WidgetSnapshots.build(input, null, NOW)
        assertEquals(1, snapshot.conversations.size)
        assertEquals("/valid", snapshot.conversations.first().peer)
    }

    @Test
    fun `bump other mailbox returns null`() {
        val input = makeInput(conversations = emptyList())
        val snapshot = WidgetSnapshots.build(input, null, NOW)
        assertNull(WidgetSnapshots.bump(snapshot, "/other", "/alice", null, "msg1", NOW))
    }

    @Test
    fun `bump repeated message id returns null`() {
        val input = makeInput(conversations = emptyList())
        val snapshot = WidgetSnapshots.build(input, null, NOW)
        val bumped1 = WidgetSnapshots.bump(snapshot, MAILBOX, "/alice", null, "msg1", NOW)
        val bumped2 = WidgetSnapshots.bump(bumped1!!, MAILBOX, "/alice", null, "msg1", NOW + 1)
        assertNull(bumped2)
    }

    @Test
    fun `bump new peer inserted at top with unread 1`() {
        val input = makeInput(conversations = emptyList())
        val snapshot = WidgetSnapshots.build(input, null, NOW)
        val bumped = WidgetSnapshots.bump(snapshot, MAILBOX, "/alice", "Alice", "msg1", NOW)

        assertNotNull(bumped)
        assertEquals(1, bumped!!.unread)
        assertEquals(1, bumped.conversations.size)
        val conv = bumped.conversations.first()
        assertEquals("/alice", conv.peer)
        assertEquals(1, conv.unread)
    }

    @Test
    fun `bump existing peer clears preview and increments unread`() {
        val input = makeInput(
            showPreviews = true,
            conversations = listOf(
                WidgetSourceConversation(peer = "/alice", name = "Alice", at = 100, unread = 1, lastText = "Hello", group = false)
            )
        )
        val snapshot = WidgetSnapshots.build(input, null, NOW)
        val convBefore = snapshot.conversations.first()
        assertNotNull(convBefore.preview)

        val bumped = WidgetSnapshots.bump(snapshot, MAILBOX, "/alice", null, "msg1", NOW + 100)
        assertNotNull(bumped)
        val convAfter = bumped!!.conversations.find { it.peer == "/alice" }
        assertNotNull(convAfter)
        assertNull(convAfter!!.preview)
        assertEquals(2, convAfter.unread)
        assertEquals(2, bumped.unread)
    }

    @Test
    fun `agenda drops cancelled, declined, past, future events`() {
        val nowSec = NOW
        val validEvent = WidgetSourceEvent(
            eventId = "e1", occurrenceId = "o1", title = "Valid", startsAt = nowSec + 1000, endsAt = null,
            allDay = false, startDate = null, endDate = null, location = null, status = null, myRsvp = null, meeting = false
        )
        val cancelledEvent = WidgetSourceEvent(
            eventId = "e2", occurrenceId = "o2", title = "Cancelled", startsAt = nowSec + 1000, endsAt = null,
            allDay = false, startDate = null, endDate = null, location = null, status = "cancelled", myRsvp = null, meeting = false
        )
        val declinedEvent = WidgetSourceEvent(
            eventId = "e3", occurrenceId = "o3", title = "Declined", startsAt = nowSec + 1000, endsAt = null,
            allDay = false, startDate = null, endDate = null, location = null, status = null, myRsvp = "declined", meeting = false
        )
        val pastEvent = WidgetSourceEvent(
            eventId = "e4", occurrenceId = "o4", title = "Past", startsAt = nowSec - 1000, endsAt = null,
            allDay = false, startDate = null, endDate = null, location = null, status = null, myRsvp = null, meeting = false
        )
        val farFutureEvent = WidgetSourceEvent(
            eventId = "e5", occurrenceId = "o5", title = "Far", startsAt = nowSec + (15 * 24 * 3600).toLong(), endsAt = null,
            allDay = false, startDate = null, endDate = null, location = null, status = null, myRsvp = null, meeting = false
        )

        val agenda = WidgetSourceAgenda(zone = "UTC", events = listOf(validEvent, cancelledEvent, declinedEvent, pastEvent, farFutureEvent))
        val input = makeInput(agenda = agenda)
        val snapshot = WidgetSnapshots.build(input, null, NOW)

        assertNotNull(snapshot.agenda)
        assertEquals(1, snapshot.agenda!!.events.size)
        assertEquals("Valid", snapshot.agenda!!.events.first().title)
    }

    @Test
    fun `preview is the first written line without markup or control characters`() {
        assertEquals("Title", WidgetSnapshots.preview("\n\n# Title\n\nBody"))
        assertEquals("quoted reply", WidgetSnapshots.preview(">  quoted\u0007   reply"))
        assertNull(WidgetSnapshots.preview("   \n  "))
        assertNull(WidgetSnapshots.preview(null))
    }

    @Test
    fun `a snapshot written as locked is redacted again when it is read`() {
        val leaked = WidgetSnapshot(mailbox = MAILBOX, generatedAt = NOW, locked = true, previews = true, unread = 3,
            conversations = listOf(WidgetConversation(peer = "/alice", name = "Alice", unread = 3, preview = "secret")),
            nests = WidgetNests(enabled = true, nests = listOf(WidgetNest("nst_1", "Helper", "ready"))),
            agenda = WidgetAgenda(enabled = true, zone = "UTC", events = listOf(WidgetEvent("evt_1", "occ_1", "Board meeting", startsAt = NOW))))
        val read = WidgetSnapshots.decode(WidgetSnapshots.encode(leaked))!!
        assertTrue(read.locked)
        assertFalse(read.previews)
        assertEquals(3, read.unread)
        assertTrue(read.conversations.isEmpty())
        assertEquals(1, read.nests!!.count)
        assertTrue(read.nests!!.nests.isEmpty())
        assertEquals(1, read.agenda!!.count)
        assertTrue(read.agenda!!.events.isEmpty())
        assertFalse(WidgetSnapshots.encode(read).contains("secret") || WidgetSnapshots.encode(read).contains("Alice") ||
            WidgetSnapshots.encode(read).contains("Board meeting") || WidgetSnapshots.encode(read).contains("Helper"))
    }

    @Test
    fun `a preview on disk is dropped when previews are off`() {
        val stored = WidgetSnapshot(mailbox = MAILBOX, generatedAt = NOW, previews = false,
            conversations = listOf(WidgetConversation(peer = "/alice", name = "Alice", preview = "secret")))
        assertNull(WidgetSnapshots.decode(WidgetSnapshots.encode(stored))!!.conversations.single().preview)
    }

    @Test
    fun `nothing that reaches the postbox is in the encoded snapshot`() {
        val text = WidgetSnapshots.encode(WidgetSnapshots.build(makeInput(conversations = listOf(
            WidgetSourceConversation(peer = "/alice", name = "Alice", at = 100, unread = 1, lastText = "Hi"))), null, NOW))
        for (word in listOf("token", "key", "secret", "authorization", "bearer")) assertFalse(word, text.lowercase().contains("\"$word"))
    }

    @Test
    fun `all-day events today stay listed and sort before later ones`() {
        val today = java.time.Instant.ofEpochSecond(NOW).atZone(java.time.ZoneOffset.UTC).toLocalDate()
        val allDay = WidgetSourceEvent(eventId = "evt_a", occurrenceId = "occ_a", title = "Holiday", startsAt = null, endsAt = null, allDay = true,
            startDate = today.toString(), endDate = today.plusDays(1).toString(), location = null, status = null, myRsvp = null, meeting = false)
        val ended = allDay.copy(eventId = "evt_b", startDate = today.minusDays(2).toString(), endDate = today.toString())
        val timed = allDay.copy(eventId = "evt_c", allDay = false, startsAt = NOW + 60, startDate = null, endDate = null, title = "Call")
        val agenda = WidgetSnapshots.build(makeInput(agenda = WidgetSourceAgenda("UTC", listOf(timed, ended, allDay))), null, NOW).agenda!!
        assertEquals(listOf("evt_a", "evt_c"), agenda.events.map { it.eventId })
        assertNull(agenda.events.first().startsAt)
    }

    @Test
    fun `a push into a locked snapshot only counts`() {
        val locked = WidgetSnapshots.build(makeInput(locked = true, conversations = listOf(
            WidgetSourceConversation(peer = "/alice", name = "Alice", at = 100, unread = 1))), null, NOW)
        val bumped = WidgetSnapshots.bump(locked, MAILBOX, "/bob", null, "msg9", NOW)!!
        assertEquals(2, bumped.unread)
        assertTrue(bumped.conversations.isEmpty())
    }

    @Test
    fun `a new mailbox does not inherit the previous mailbox's sections`() {
        val first = WidgetSnapshots.build(makeInput(nests = WidgetSourceNests(listOf(WidgetNest("nst_1", "Helper", "ready")))), null, NOW)
        val other = WidgetSnapshots.build(WidgetInput(mailbox = "/other", conversations = emptyList(), showPreviews = false,
            nestsEnabled = true, nests = null), first, NOW)
        assertNull(other.nests)
        assertTrue(other.pushIds.isEmpty())
    }

    @Test
    fun `agenda events sorted by start`() {
        val laterEvent = WidgetSourceEvent(
            eventId = "e1", occurrenceId = "o1", title = "Later", startsAt = NOW + 2000, endsAt = null,
            allDay = false, startDate = null, endDate = null, location = null, status = null, myRsvp = null, meeting = false
        )
        val earlierEvent = WidgetSourceEvent(
            eventId = "e2", occurrenceId = "o2", title = "Earlier", startsAt = NOW + 1000, endsAt = null,
            allDay = false, startDate = null, endDate = null, location = null, status = null, myRsvp = null, meeting = false
        )
        val agenda = WidgetSourceAgenda(zone = "UTC", events = listOf(laterEvent, earlierEvent))
        val input = makeInput(agenda = agenda)
        val snapshot = WidgetSnapshots.build(input, null, NOW)

        assertEquals(2, snapshot.agenda!!.events.size)
        assertEquals("Earlier", snapshot.agenda!!.events[0].title)
        assertEquals("Later", snapshot.agenda!!.events[1].title)
    }
}