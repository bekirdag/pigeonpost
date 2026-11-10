package dev.pigeonpost.core.calendar

import org.junit.Assert.*
import org.junit.Test
import java.time.LocalDateTime
import java.time.ZoneId

class CalendarGridTest {
    private val zone = ZoneId.of("Europe/Istanbul")
    private fun at(text: String) = LocalDateTime.parse(text).atZone(zone).toEpochSecond()
    private fun timed(id: String, start: String, end: String) = EventOccurrence("occ_$id", "evt_$id", startsAt = at(start), endsAt = at(end), title = id)

    @Test fun weekStartsOnTheChosenDay() {
        val wednesday = CalendarDay("2026-11-04")
        assertEquals("2026-11-02", CalendarGrid.week(wednesday, 1).first().text) // Monday
        assertEquals("2026-11-01", CalendarGrid.week(wednesday, 0).first().text) // Sunday
        assertEquals("2026-10-31", CalendarGrid.week(wednesday, 6).first().text) // Saturday
        assertEquals(7, CalendarGrid.week(wednesday, 1).size)
        assertEquals(listOf(wednesday), CalendarGrid.days(CalendarView.DAY, wednesday, 1))
        // A bad setting falls back to Monday rather than failing.
        assertEquals("2026-11-02", CalendarGrid.week(wednesday, 9).first().text)
    }

    @Test fun timedItemsArePlacedInMinutesAndOverlapsShareLanes() {
        val day = CalendarDay("2026-11-04")
        val blocks = CalendarGrid.layout(listOf(
            timed("a", "2026-11-04T09:00", "2026-11-04T10:00"),
            timed("b", "2026-11-04T09:30", "2026-11-04T11:00"),
            timed("c", "2026-11-04T10:00", "2026-11-04T10:30"),
            timed("d", "2026-11-04T14:00", "2026-11-04T14:05"),
        ), listOf(day), zone).associateBy { it.occurrence.title }
        assertEquals(540, blocks.getValue("a").startMinute); assertEquals(600, blocks.getValue("a").endMinute)
        assertEquals(0, blocks.getValue("a").lane); assertEquals(1, blocks.getValue("b").lane)
        assertEquals(0, blocks.getValue("c").lane) // a has ended, so c reuses its lane
        assertEquals(2, blocks.getValue("a").lanes); assertEquals(2, blocks.getValue("c").lanes)
        // A separate cluster gets the full width, and a 5-minute item stays readable.
        assertEquals(1, blocks.getValue("d").lanes)
        assertEquals(840 + CalendarGrid.MINIMUM_MINUTES, blocks.getValue("d").endMinute)
    }

    @Test fun anItemOverMidnightIsCutIntoBothDays() {
        val days = CalendarGrid.week(CalendarDay("2026-11-04"), 1)
        val blocks = CalendarGrid.layout(listOf(timed("late", "2026-11-04T23:00", "2026-11-05T01:30")), days, zone)
        assertEquals(2, blocks.size)
        val (first, second) = blocks.sortedBy { it.dayIndex }
        assertEquals(2, first.dayIndex); assertEquals(1380, first.startMinute); assertEquals(1440, first.endMinute)
        assertTrue(first.continuesAfter); assertFalse(first.continuesBefore)
        assertEquals(3, second.dayIndex); assertEquals(0, second.startMinute); assertEquals(90, second.endMinute)
        assertTrue(second.continuesBefore)
    }

    @Test fun allDayItemsStayOffTheTimeGrid() {
        val day = CalendarDay("2026-11-04")
        val holiday = EventOccurrence("occ_h", "evt_h", allDay = true, startDate = "2026-11-04", endDate = "2026-11-05", title = "Holiday")
        assertTrue(CalendarGrid.layout(listOf(holiday), listOf(day), zone).isEmpty())
        assertEquals(listOf("Holiday"), CalendarGrid.allDay(listOf(holiday), listOf(day), zone).single().map { it.title })
    }

    @Test fun theNowLineIsOnlyOnToday() {
        val days = CalendarGrid.week(CalendarDay("2026-11-04"), 1)
        assertEquals(CalendarNowLine(2, 13 * 60 + 45), CalendarGrid.now(at("2026-11-04T13:45:30"), days, zone))
        assertNull(CalendarGrid.now(at("2026-11-20T13:45"), days, zone))
        assertNull(CalendarGrid.now(at("2026-11-05T08:00"), listOf(CalendarDay("2026-11-04")), zone))
    }
}
