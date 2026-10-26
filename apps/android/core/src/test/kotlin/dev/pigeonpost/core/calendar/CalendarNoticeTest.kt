package dev.pigeonpost.core.calendar

import org.junit.Assert.*
import org.junit.Test

class CalendarNoticeTest {
    private val base = mapOf("type" to "calendar", "identity" to "/k/receiver", "mailbox" to "/display/only", "message_id" to "cal:opaque:reminder", "calendar_event_id" to "evt_one", "calendar_occurrence_id" to "occ_original", "peer" to "/k/organizer")
    @Test fun androidUsesCanonicalRecipientIdentityRatherThanTheSubtitleOrOrganizer() {
        val notice = CalendarNotice.parse(base)!!
        assertEquals("/k/receiver", notice.mailbox)
        assertEquals(CalendarEventKey("evt_one", "occ_original"), notice.event)
    }
    @Test fun legacyNoticesOpenOnlyCalendarWithoutDerivingAnEventFromOpaqueMessageId() {
        val data = mapOf("identity" to "/k/receiver", "message_id" to "cal:evt_guess:occ_guess:reminder", "peer" to "/k/organizer")
        assertTrue(CalendarNotice.matches(data)); assertNull(CalendarNotice.parse(data)!!.event)
    }
    @Test fun invitationsMayOmitTheOccurrenceButNeverTheCanonicalRecipient() {
        assertEquals(CalendarEventKey("evt_one"), CalendarNotice.parse(base - "calendar_occurrence_id")!!.event)
        assertNull(CalendarNotice.parse(base - "identity"))
    }
    @Test fun invalidRecipientNeverFallsBackToASubtitleOrPeer() {
        for (invalid in listOf("", "/k/", "/k/a/b", "/k/a%2Fb", "/k/a\n", "/k/ü", "/handle", "/k/" + "x".repeat(158)))
            assertNull(CalendarNotice.parse(base + ("identity" to invalid)))
    }
    @Test fun malformedEventAndOccurrenceLocatorsFallBackToCalendarWithoutGuessing() {
        for (invalid in listOf("", "evt_", "evt_../bad", "evt_bad\n", "evt_ü", "evt_" + "x".repeat(80)))
            assertNull(CalendarNotice.parse(base + ("calendar_event_id" to invalid))!!.event)
        for (invalid in listOf("", "occ_", "occ_bad/value", "occ_bad%2Fvalue", "occ_ü", "x".repeat(81)))
            assertNull(CalendarNotice.parse(base + ("calendar_occurrence_id" to invalid))!!.event)
    }
    @Test fun ordinaryAndCallNotificationsStayOutOfCalendarRouting() {
        for (data in listOf(emptyMap(), mapOf("type" to "call_ring", "message_id" to "message_one"), mapOf("identity" to "/k/receiver", "peer" to "/k/organizer"))) {
            assertFalse(CalendarNotice.matches(data)); assertNull(CalendarNotice.parse(data))
        }
    }
    @Test fun claimFieldsAndGuestTicketsCannotGrantAuthorityOrAppearInDescriptions() {
        val notice = CalendarNotice.parse(base + mapOf("role" to "host", "token" to "private-ticket", "title" to "Private title"))!!
        assertEquals(CalendarEventKey("evt_one", "occ_original"), notice.event)
        assertFalse(notice.toString().contains("private", ignoreCase = true))
        assertFalse(notice.toString().contains("organizer"))
    }
}
