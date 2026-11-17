package dev.pigeonpost.inbox.widgets

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.glance.ColorFilter
import androidx.glance.GlanceId
import androidx.glance.GlanceModifier
import androidx.glance.ImageProvider
import androidx.glance.LocalSize
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.SizeMode
import androidx.glance.appwidget.lazy.LazyColumn
import androidx.glance.appwidget.lazy.items
import androidx.glance.appwidget.provideContent
import androidx.glance.background
import androidx.glance.layout.Alignment
import androidx.glance.layout.Box
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.height
import androidx.glance.layout.padding
import androidx.glance.layout.width
import androidx.glance.semantics.contentDescription
import androidx.glance.semantics.semantics
import androidx.glance.text.FontWeight
import dev.pigeonpost.core.widgets.WidgetAgenda
import dev.pigeonpost.core.widgets.WidgetEvent
import dev.pigeonpost.core.widgets.WidgetSnapshot
import dev.pigeonpost.design.PPIcon
import dev.pigeonpost.inbox.R
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/** Agenda: today's and the next two weeks' events from the app's calendar. */
class AgendaWidget : GlanceAppWidget() {
    companion object {
        val SQUARE = DpSize(110.dp, 110.dp)
        val MEDIUM = DpSize(250.dp, 110.dp)
        val LARGE = DpSize(250.dp, 250.dp)
        val TABLET = DpSize(400.dp, 300.dp)
    }
    override val sizeMode = SizeMode.Responsive(setOf(SQUARE, MEDIUM, LARGE, TABLET))
    override val previewSizeMode = SizeMode.Responsive(setOf(MEDIUM, LARGE))
    override val stateDefinition = null

    override suspend fun provideGlance(context: Context, id: GlanceId) {
        val snapshots = WidgetStore.snapshots(context)
        provideContent { val snapshot by snapshots.collectAsState(); AgendaContent(snapshot, nowSeconds()) }
    }

    override suspend fun providePreview(context: Context, widgetCategory: Int) {
        provideContent { AgendaContent(WidgetSamples.snapshot(nowSeconds()), nowSeconds()) }
    }
}

class AgendaWidgetReceiver : PigeonpostWidgetReceiver() { override val glanceAppWidget: GlanceAppWidget = AgendaWidget() }

/** A day heading or an event, in the order the list shows them. */
internal sealed interface AgendaLine {
    data class Day(val label: String) : AgendaLine
    data class Event(val event: WidgetEvent) : AgendaLine
}

private fun zoneOf(agenda: WidgetAgenda): ZoneId = runCatching { ZoneId.of(agenda.zone) }.getOrDefault(ZoneId.systemDefault())

/** The day an event belongs to in the list: its first day, or today for one that started earlier. */
internal fun agendaDay(event: WidgetEvent, zone: ZoneId, today: LocalDate): LocalDate {
    val start = if (event.allDay) event.startDate?.let { runCatching { LocalDate.parse(it) }.getOrNull() }
        else event.startsAt?.let { Instant.ofEpochSecond(it).atZone(zone).toLocalDate() }
    return maxOf(start ?: today, today)
}

internal fun dayLabel(day: LocalDate, today: LocalDate, locale: Locale = Locale.getDefault()): String = when (day) {
    today -> "Today"
    today.plusDays(1) -> "Tomorrow"
    else -> day.format(DateTimeFormatter.ofPattern("EEE d MMM", locale))
}

internal fun eventTime(event: WidgetEvent, zone: ZoneId, locale: Locale = Locale.getDefault()): String {
    if (event.allDay) return "All day"
    val format = DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT).withLocale(locale).withZone(zone)
    val start = event.startsAt?.let { format.format(Instant.ofEpochSecond(it)) } ?: return ""
    val end = event.endsAt?.let { format.format(Instant.ofEpochSecond(it)) }
    return if (end == null || end == start) start else "$start – $end"
}

internal fun agendaLines(agenda: WidgetAgenda, now: Long): List<AgendaLine> {
    val zone = zoneOf(agenda)
    val today = Instant.ofEpochSecond(now).atZone(zone).toLocalDate()
    // Drop what has ended since the snapshot was written; it is redrawn every half hour.
    val live = agenda.events.filter { event ->
        if (event.allDay) (event.endDate?.let { runCatching { LocalDate.parse(it) }.getOrNull() } ?: agendaDay(event, zone, today).plusDays(1)) > today
        else (event.endsAt ?: event.startsAt ?: 0) >= now
    }
    return live.groupBy { agendaDay(it, zone, today) }.toSortedMap().flatMap { (day, events) ->
        listOf(AgendaLine.Day(dayLabel(day, today))) + events.map { AgendaLine.Event(it) }
    }
}

@Composable
fun AgendaContent(snapshot: WidgetSnapshot?, now: Long) {
    val size = LocalSize.current
    val agenda = snapshot?.agenda
    val compact = size.width < 200.dp
    val open = if (snapshot == null) WidgetTargets.app() else WidgetTargets.calendar(snapshot.mailbox)
    WidgetCard(open, padding = if (size.width >= 400.dp) WidgetSpace.gutterLarge else WidgetSpace.gutter) {
        Column(GlanceModifier.fillMaxSize()) {
            if (snapshot != null) {
                val zone = agenda?.let(::zoneOf) ?: ZoneId.systemDefault()
                val today = Instant.ofEpochSecond(now).atZone(zone).toLocalDate()
                WidgetHeader(PPIcon.CalendarDays, if (compact) today.format(DateTimeFormatter.ofPattern("EEE d", Locale.getDefault()))
                    else today.format(DateTimeFormatter.ofPattern("EEEE d MMMM", Locale.getDefault())), compact = compact)
                Spacer(GlanceModifier.height(8.dp))
            }
            val lines = agenda?.takeIf { it.enabled && snapshot?.locked != true }?.let { agendaLines(it, now) }.orEmpty()
            when {
                snapshot == null -> SignedOut(compact = compact)
                agenda == null -> WidgetMessage(PPIcon.Calendar, "Open your calendar", "Open the calendar in Pigeonpost once to show your events here.", compact = compact)
                !agenda.enabled -> WidgetMessage(PPIcon.Calendar, "No calendar", "The calendar is not available for this inbox.", compact = compact)
                snapshot.locked -> WidgetMessage(PPIcon.Lock, if (agenda.count == 1) "1 upcoming event" else "${agenda.count} upcoming events", "Open Pigeonpost to see them.", compact = compact)
                lines.isEmpty() -> WidgetMessage(PPIcon.Calendar, "Nothing scheduled", "No events in the next two weeks.", compact = compact)
                else -> LazyColumn(GlanceModifier.fillMaxSize()) {
                    items(lines, itemId = { line -> when (line) { is AgendaLine.Day -> ("d" + line.label).hashCode().toLong(); is AgendaLine.Event -> (line.event.eventId + line.event.occurrenceId).hashCode().toLong() } }) { line ->
                        when (line) {
                            is AgendaLine.Day -> Label(line.label.uppercase(Locale.getDefault()), WidgetType.overline, WidgetColors.muted, FontWeight.Bold,
                                GlanceModifier.padding(top = 4.dp, bottom = 2.dp))
                            is AgendaLine.Event -> EventRow(snapshot.mailbox, line.event, zoneOf(agenda), compact)
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun EventRow(mailbox: String, event: WidgetEvent, zone: ZoneId, compact: Boolean) {
    val time = eventTime(event, zone)
    val detail = listOfNotNull(time.ifEmpty { null }, event.location.takeUnless { compact }).joinToString(" · ")
    Row(GlanceModifier.fillMaxWidth().padding(vertical = 4.dp).clickable(WidgetTargets.event(mailbox, event))
        .semantics { contentDescription = listOf(event.title, detail).filter { it.isNotEmpty() }.joinToString(", ") }, verticalAlignment = Alignment.CenterVertically) {
        Box(GlanceModifier.width(3.dp).height(if (compact) 28.dp else 34.dp)
            .background(ImageProvider(R.drawable.widget_pill), colorFilter = ColorFilter.tint(WidgetColors.accent))) {}
        Spacer(GlanceModifier.width(8.dp))
        Column(GlanceModifier.defaultWeight()) {
            Label(event.title, if (compact) WidgetType.bodySmall else WidgetType.body, WidgetColors.text, FontWeight.Medium)
            if (detail.isNotEmpty()) Label(detail, WidgetType.caption, WidgetColors.muted)
        }
        if (event.meeting && !compact) { Spacer(GlanceModifier.width(6.dp)); WidgetIcon(PPIcon.Video, 16.dp, WidgetColors.link, "Pigeonpost meeting") }
    }
}
