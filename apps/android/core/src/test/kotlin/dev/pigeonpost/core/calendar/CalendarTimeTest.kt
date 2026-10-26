package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.WireJson
import org.junit.Assert.*
import org.junit.Test
import java.time.Instant

class CalendarTimeTest {
    private inline fun <reified T : Throwable> fails(work: () -> Unit): T {
        try { work(); fail("Expected ${T::class.simpleName}") }
        catch (failure: Throwable) { if (failure is T) return failure; throw failure }
        error("Unreachable")
    }

    @Test fun datesAreStrictAsciiGregorianDays() {
        assertEquals("2028-02-29", CalendarDay("2028-02-29").text)
        for (text in listOf("2027-02-29", "2026-04-31", "0000-01-01", "2026-1-01", "２０２６-01-01", "2026-01-01Z", "2026-01-01 "))
            fails<CalendarTimeError.InvalidFormat> { CalendarDay(text) }
        assertEquals(CalendarDay("2027-01-01"), CalendarDay("2026-12-31").plus(1))
    }
    @Test fun wallTimePreservesItsLiteralStringAndSeconds() {
        val wall = CalendarWallTime("2026-10-22T09:00:07", "Europe/Istanbul")
        assertEquals("2026-10-22T09:00:07", wall.text)
        assertEquals(Instant.parse("2026-10-22T06:00:07Z").epochSecond, wall.resolve())
        for (text in listOf("2026-10-22T24:00", "2026-10-22T09:60", "2026-10-22 09:00", "2026-10-22T09:00Z", "2026-10-22T09:00.5", "2026-10-22T09:00:60"))
            fails<CalendarTimeError.InvalidFormat> { CalendarWallTime(text, "Europe/Istanbul") }
        for (zone in listOf("Not/AZone", "+03:00", "GMT+03:00")) fails<CalendarTimeError.UnknownZone> { CalendarWallTime("2026-10-22T09:00", zone) }
    }
    @Test fun aSpringGapIsRefusedInsteadOfShifted() {
        fails<CalendarTimeError.Nonexistent> { CalendarWallTime("2026-03-08T02:30", "America/New_York").resolve() }
    }
    @Test fun aRepeatedTimeRequiresAnExplicitOffsetChoice() {
        val wall = CalendarWallTime("2026-11-01T01:30", "America/New_York")
        val ambiguity = fails<CalendarTimeError.Ambiguous> { wall.resolve() }
        assertEquals(3600L, ambiguity.later - ambiguity.earlier)
        assertEquals(Instant.parse("2026-11-01T05:30:00Z").epochSecond, wall.resolve(CalendarFold.EARLIER))
        assertEquals(Instant.parse("2026-11-01T06:30:00Z").epochSecond, wall.resolve(CalendarFold.LATER))
    }
    @Test fun localDaysKeepTheir23And25HourDurations() {
        val zone = CalendarTime.zone("America/New_York")
        CalendarDay("2026-03-08").interval(zone).let { assertEquals(23 * 3600L, it.end - it.start) }
        CalendarDay("2026-11-01").interval(zone).let { assertEquals(25 * 3600L, it.end - it.start) }
    }
    @Test fun aSkippedCalendarDayIsNotNormalizedIntoAnotherDate() {
        val zone = CalendarTime.zone("Pacific/Apia")
        fails<CalendarTimeError.Nonexistent> { CalendarDay("2011-12-30").start(zone) }
        CalendarDay("2011-12-29").interval(zone).let { assertEquals(24 * 3600L, it.end - it.start) }
    }
    @Test fun eventEndMustBeAfterStartForTheChosenOffsets() {
        fails<CalendarTimeError.Order> { CalendarWallTime.range("2026-10-22T09:00", "2026-10-22T09:00", "Europe/Istanbul") }
        fails<CalendarTimeError.Order> { CalendarWallTime.range("2026-10-22T10:00", "2026-10-22T09:00", "Europe/Istanbul") }
    }
    @Test fun aMonthUses42LocalDaysAndTheSelectedWeekStart() {
        val month = CalendarMonthWindow(CalendarDay("2026-03-15"), 1)
        assertEquals(42, month.days.size)
        assertEquals("2026-02-23", month.days.first().text)
        assertEquals(1, month.days.first().date.dayOfWeek.value)
        val interval = month.interval(CalendarTime.zone("America/New_York"))
        assertEquals(42 * 86400L - 3600, interval.end - interval.start)
        fails<CalendarTimeError.InvalidFormat> { CalendarMonthWindow(CalendarDay("2026-03-15"), 7) }
    }
    @Test fun allDayRangesUseExclusiveEndWithoutConvertingZones() {
        val event = EventOccurrence("occ_original", "evt_one", allDay = true, startDate = "2026-10-22", endDate = "2026-10-24")
        for (zone in listOf("America/Los_Angeles", "Europe/Istanbul")) {
            assertTrue(event.occursOn(CalendarDay("2026-10-22"), CalendarTime.zone(zone)))
            assertTrue(event.occursOn(CalendarDay("2026-10-23"), CalendarTime.zone(zone)))
            assertFalse(event.occursOn(CalendarDay("2026-10-24"), CalendarTime.zone(zone)))
        }
    }
    @Test fun aMovedOccurrenceKeepsItsOriginalOpaqueIdentity() {
        val event = WireJson.decodeFromString<EventOccurrence>("""{"event_id":"evt_one","occurrence_id":"occ_original_20261022T090000","recurrence_key":"20261022T090000","starts_at":1792821600,"ends_at":1792825200,"status":"new_future_status","future_field":true}""")
        assertEquals(CalendarEventKey("evt_one", "occ_original_20261022T090000"), event.key)
        assertEquals("new_future_status", event.status)
        assertNull(event.role)
        assertNull(event.organizer)
    }
    @Test fun partialDetailsAndRedactedGuestsDoNotGrantMissingPermissions() {
        val detail = WireJson.decodeFromString<CalendarEventResponse>("""{"event":{"id":"evt_one","attendees":[{"attendee_id":"att_one","kind":"external","name":"Guest"}]},"occurrence":{"occurrence_id":"occ_original"}}""")
        assertNull(detail.event.canEdit)
        assertNull(detail.event.attendees!!.single().contact)
        assertEquals("occ_original", detail.occurrence!!.occurrenceId)
    }
    @Test fun serializedFieldsMatchTheWireAndCapabilityUrlsAreNotDebugDescriptions() {
        assertEquals("""{"minutes_before":10}""", WireJson.encodeToString(CalendarReminder.serializer(), CalendarReminder(10)))
        val folder = WireJson.decodeFromString<CalendarFolder>("""{"id":"cal_one","is_default":true,"revision":9}""")
        assertTrue(folder.isDefault == true); assertEquals(9L, folder.revision)
        assertFalse(CalendarGuestLink("https://fixture.invalid/m/code?token=private-fixture").toString().contains("private-fixture"))
        assertFalse(CalendarFeed(url = "https://fixture.invalid/feed/private-fixture").toString().contains("private-fixture"))
    }
}
