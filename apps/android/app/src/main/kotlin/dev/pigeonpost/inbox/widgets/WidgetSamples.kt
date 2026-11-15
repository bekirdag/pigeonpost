package dev.pigeonpost.inbox.widgets

import dev.pigeonpost.core.widgets.WidgetAgenda
import dev.pigeonpost.core.widgets.WidgetConversation
import dev.pigeonpost.core.widgets.WidgetEvent
import dev.pigeonpost.core.widgets.WidgetNest
import dev.pigeonpost.core.widgets.WidgetNests
import dev.pigeonpost.core.widgets.WidgetSnapshot
import java.time.Instant
import java.time.ZoneId

/**
 * Made-up content for the widget picker's previews and the screenshot tests. Never real data:
 * the picker is shown before anyone signs in. Previews stay off here too, as they are by default.
 */
object WidgetSamples {
    const val MAILBOX = "/k/sample"

    fun snapshot(now: Long, zone: ZoneId = ZoneId.systemDefault()): WidgetSnapshot {
        val today = Instant.ofEpochSecond(now).atZone(zone).toLocalDate()
        fun at(daysFromToday: Long, hour: Int, minute: Int = 0) = today.plusDays(daysFromToday).atTime(hour, minute).atZone(zone).toEpochSecond()
        val conversations = listOf(
            WidgetConversation("/maya", "Maya Okafor", now - 4 * 60, unread = 2),
            WidgetConversation("grp_samplesampleabcdefghijklmn", "Design review", now - 38 * 60, unread = 5, group = true),
            WidgetConversation("/acme/support", "Acme Support", now - 2 * 3600, unread = 1),
            WidgetConversation("/jonas", "Jonas Berg", now - 26 * 3600),
            WidgetConversation("/lena-park", "Lena Park", now - 50 * 3600),
            WidgetConversation("/studio/ops", "Studio Ops", now - 75 * 3600),
        )
        return WidgetSnapshot(
            mailbox = MAILBOX, generatedAt = now, unread = conversations.sumOf { it.unread }, conversations = conversations,
            nests = WidgetNests(enabled = true, hoursLimit = 120, nests = listOf(
                WidgetNest("nst_a1", "Research helper", "ready", runningSecs = 46 * 3600L),
                WidgetNest("nst_b2", "Inbox triage", "working", runningSecs = 31 * 3600L),
                WidgetNest("nst_c3", "Bookkeeping", "paused", runningSecs = 9 * 3600L),
            )),
            agenda = WidgetAgenda(enabled = true, zone = zone.id, events = listOf(
                WidgetEvent("evt_s1", "occ_s1", "Design review", startsAt = at(0, 15, 30), endsAt = at(0, 16, 15), meeting = true),
                WidgetEvent("evt_s2", "occ_s2", "Dinner with Maya", startsAt = at(0, 19), endsAt = at(0, 21), location = "Harbour Kitchen"),
                WidgetEvent("evt_s3", "occ_s3", "Team offsite", allDay = true, startDate = today.plusDays(1).toString(), endDate = today.plusDays(2).toString()),
                WidgetEvent("evt_s4", "occ_s4", "Quarterly planning", startsAt = at(1, 10), endsAt = at(1, 11, 30), meeting = true),
                WidgetEvent("evt_s5", "occ_s5", "Dentist", startsAt = at(3, 9, 15), endsAt = at(3, 10), location = "Old Town Clinic"),
            )),
        )
    }

    fun empty(now: Long) = WidgetSnapshot(mailbox = MAILBOX, generatedAt = now,
        nests = WidgetNests(enabled = true), agenda = WidgetAgenda(enabled = true, zone = ZoneId.systemDefault().id))
}
