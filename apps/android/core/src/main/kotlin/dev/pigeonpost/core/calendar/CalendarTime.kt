package dev.pigeonpost.core.calendar

import java.time.DateTimeException
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.ZoneId

sealed class CalendarTimeError(message: String) : IllegalArgumentException(message) {
    class InvalidFormat : CalendarTimeError("Enter a valid date and time.")
    class UnknownZone : CalendarTimeError("Choose a valid time zone.")
    class Nonexistent : CalendarTimeError("That local time does not exist because the clocks change. Choose another time.")
    data class Ambiguous(val earlier: Long, val later: Long) : CalendarTimeError("That time happens twice. Choose the earlier or later offset.")
    class Order : CalendarTimeError("The end must be after the start.")
}
enum class CalendarFold(val wire: String) { EARLIER("earlier"), LATER("later") }
data class CalendarInterval(val start: Long, val end: Long) {
    init { if (end <= start) throw CalendarTimeError.Order() }
}

object CalendarTime {
    private val zones = ZoneId.getAvailableZoneIds()
    fun zone(id: String): ZoneId {
        if (id !in zones) throw CalendarTimeError.UnknownZone()
        return ZoneId.of(id)
    }
    fun displayZone(preferred: String?): ZoneId = runCatching { zone(preferred ?: ZoneId.systemDefault().id) }.getOrElse { ZoneId.of("UTC") }
}

/** A local calendar date, never a UTC conversion of the event's authored wall time. */
class CalendarDay private constructor(val date: LocalDate) : Comparable<CalendarDay> {
    init { if (date.year !in 1..9999) throw CalendarTimeError.InvalidFormat() }
    constructor(text: String) : this(parse(text))
    val text: String get() = date.toString()
    fun plus(days: Long) = CalendarDay(date.plusDays(days))
    fun plusMonths(months: Long) = CalendarDay(date.plusMonths(months))
    fun firstOfMonth() = CalendarDay(date.withDayOfMonth(1))
    fun start(zone: ZoneId): Long {
        val start = date.atStartOfDay(zone)
        if (start.toLocalDate() != date) throw CalendarTimeError.Nonexistent()
        return start.toEpochSecond()
    }
    fun interval(zone: ZoneId) = CalendarInterval(start(zone), date.plusDays(1).atStartOfDay(zone).toEpochSecond())
    override fun compareTo(other: CalendarDay) = date.compareTo(other.date)
    override fun equals(other: Any?) = other is CalendarDay && date == other.date
    override fun hashCode() = date.hashCode()
    override fun toString() = text
    companion object {
        private val pattern = Regex("[0-9]{4}-[0-9]{2}-[0-9]{2}")
        private fun parse(text: String): LocalDate {
            if (!pattern.matches(text)) throw CalendarTimeError.InvalidFormat()
            return try { LocalDate.parse(text) } catch (_: DateTimeException) { throw CalendarTimeError.InvalidFormat() }
        }
        fun at(instant: Instant, zone: ZoneId) = CalendarDay(instant.atZone(zone).toLocalDate())
    }
}

/** Keep the typed string unchanged. Never let atZone silently shift a gap or pick a fold. */
class CalendarWallTime(val text: String, val tzid: String) {
    val zone = CalendarTime.zone(tzid)
    val local: LocalDateTime
    val day: CalendarDay
    init {
        if (!Regex("[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}(:[0-9]{2})?").matches(text)) throw CalendarTimeError.InvalidFormat()
        day = CalendarDay(text.take(10))
        local = try {
            LocalDateTime.of(day.date, LocalTime.of(text.substring(11, 13).toInt(), text.substring(14, 16).toInt(), if (text.length == 19) text.substring(17, 19).toInt() else 0))
        } catch (_: DateTimeException) { throw CalendarTimeError.InvalidFormat() }
    }
    fun resolve(fold: CalendarFold? = null): Long {
        val candidates = zone.rules.getValidOffsets(local).map { local.toEpochSecond(it) }.sorted()
        if (candidates.isEmpty()) throw CalendarTimeError.Nonexistent()
        if (candidates.size == 1) return candidates.single()
        return when (fold) {
            CalendarFold.EARLIER -> candidates.first()
            CalendarFold.LATER -> candidates.last()
            null -> throw CalendarTimeError.Ambiguous(candidates.first(), candidates.last())
        }
    }
    companion object {
        fun range(start: String, end: String, tzid: String, fold: CalendarFold? = null) =
            CalendarInterval(CalendarWallTime(start, tzid).resolve(fold), CalendarWallTime(end, tzid).resolve(fold))
    }
}

class CalendarMonthWindow(containing: CalendarDay, weekStart: Int) {
    val days: List<CalendarDay>
    init {
        if (weekStart !in 0..6) throw CalendarTimeError.InvalidFormat()
        val first = containing.firstOfMonth()
        val offset = (first.date.dayOfWeek.value % 7 - weekStart + 7) % 7
        days = (0L until 42L).map { first.plus(it - offset) }
    }
    fun interval(zone: ZoneId) = CalendarInterval(days.first().start(zone), days.last().plus(1).start(zone))
}

fun EventOccurrence.occursOn(day: CalendarDay, zone: ZoneId): Boolean {
    if (allDay == true) {
        val first = startDate ?: return false; val last = endDate ?: return false
        return runCatching { CalendarDay(first) <= day && day < CalendarDay(last) }.getOrDefault(false)
    }
    val first = startsAt; val last = endsAt
    return first != null && last != null && runCatching { val span = day.interval(zone); first < span.end && last > span.start }.getOrDefault(false)
}
