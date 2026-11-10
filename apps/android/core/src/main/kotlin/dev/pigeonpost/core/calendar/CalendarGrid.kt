package dev.pigeonpost.core.calendar

import java.time.ZoneId

/** The calendar's ways of looking at time (SDS §14B.13): a day list, or a time grid of one day or one week. */
enum class CalendarView { AGENDA, DAY, WEEK }

/** One timed occurrence placed on the grid: which day column, the minutes it covers there, and its lane among overlaps. */
data class CalendarGridBlock(
    val occurrence: EventOccurrence, val dayIndex: Int, val startMinute: Int, val endMinute: Int,
    val lane: Int, val lanes: Int, val continuesBefore: Boolean, val continuesAfter: Boolean,
)

data class CalendarNowLine(val dayIndex: Int, val minute: Int)

object CalendarGrid {
    const val DAY_MINUTES = 24 * 60
    /** Short events stay tall enough to read and to tap. */
    const val MINIMUM_MINUTES = 20

    /** The seven days of the week that contains [selected], starting on [weekStart] (0 = Sunday … 6 = Saturday). */
    fun week(selected: CalendarDay, weekStart: Int): List<CalendarDay> {
        val start = weekStart.takeIf { it in 0..6 } ?: 1
        val offset = (selected.date.dayOfWeek.value % 7 - start + 7) % 7
        return (0L until 7L).map { selected.plus(it - offset) }
    }

    fun days(view: CalendarView, selected: CalendarDay, weekStart: Int): List<CalendarDay> = when (view) {
        CalendarView.WEEK -> week(selected, weekStart)
        else -> listOf(selected)
    }

    private fun bounds(day: CalendarDay, zone: ZoneId): Pair<Long, Long>? = try { day.interval(zone).let { it.start to it.end } } catch (_: CalendarTimeError) { null }

    /** All-day items (and anything without times) per day column. Cancelled items stay, shown as cancelled. */
    fun allDay(occurrences: List<EventOccurrence>, days: List<CalendarDay>, zone: ZoneId): List<List<EventOccurrence>> = days.map { day ->
        occurrences.filter { (it.allDay == true || it.startsAt == null || it.endsAt == null) && it.occursOn(day, zone) }
    }

    /**
     * Timed occurrences cut to each day they touch, in minutes from that day's local midnight.
     * Overlapping items in a day share the width in lanes, as in every calendar.
     */
    private class Piece(val occurrence: EventOccurrence, val from: Int, val to: Int, val before: Boolean, val after: Boolean) { var lane = 0 }

    fun layout(occurrences: List<EventOccurrence>, days: List<CalendarDay>, zone: ZoneId): List<CalendarGridBlock> {
        val out = mutableListOf<CalendarGridBlock>()
        days.forEachIndexed { index, day ->
            val (dayStart, dayEnd) = bounds(day, zone) ?: return@forEachIndexed
            val pieces = occurrences.mapNotNull { o ->
                val start = o.startsAt ?: return@mapNotNull null; val end = o.endsAt ?: return@mapNotNull null
                if (o.allDay == true || end < start) return@mapNotNull null
                // A zero-length item belongs to the day it starts in.
                if (!(start < dayEnd && (end > dayStart || end == start && start >= dayStart))) return@mapNotNull null
                val from = ((maxOf(start, dayStart) - dayStart) / 60).toInt().coerceIn(0, DAY_MINUTES - MINIMUM_MINUTES)
                val to = ((minOf(end, dayEnd) - dayStart) / 60).toInt().coerceIn(from + MINIMUM_MINUTES, DAY_MINUTES)
                Piece(o, from, to, (start < dayStart), (end > dayEnd))
            }.sortedWith(compareBy<Piece>({ it.from }, { -it.to }, { it.occurrence.occurrenceId }))
            var cluster = mutableListOf<Piece>()
            var clusterEnd = -1
            fun flush() {
                val lanes = (cluster.maxOfOrNull { it.lane } ?: -1) + 1
                cluster.forEach { out += CalendarGridBlock(it.occurrence, index, it.from, it.to, it.lane, lanes, it.before, it.after) }
                cluster = mutableListOf(); clusterEnd = -1
            }
            for (piece in pieces) {
                if (piece.from >= clusterEnd) flush()
                val busy = cluster.filter { it.to > piece.from }.map { it.lane }.toSet()
                piece.lane = generateSequence(0) { it + 1 }.first { it !in busy }
                cluster += piece
                clusterEnd = maxOf(clusterEnd, piece.to)
            }
            flush()
        }
        return out
    }

    /** Where "now" falls on the grid, or null when no shown day is today. */
    fun now(epochSecond: Long, days: List<CalendarDay>, zone: ZoneId): CalendarNowLine? {
        days.forEachIndexed { index, day ->
            val (start, end) = bounds(day, zone) ?: return@forEachIndexed
            if (epochSecond in start until end) return CalendarNowLine(index, ((epochSecond - start) / 60).toInt().coerceIn(0, DAY_MINUTES - 1))
        }
        return null
    }
}
