package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChevronLeft
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pigeonpost.core.calendar.*
import kotlinx.coroutines.delay
import java.time.Instant
import java.time.format.DateTimeFormatter
import java.util.Locale

private val HourHeight = 48.dp
private val GutterWidth = 44.dp

/** Agenda, Day and Week. The grid views show a red line at the current time on today. */
@Composable
internal fun CalendarViewPicker(view: CalendarView, choose: (CalendarView) -> Unit) {
    SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().testTag("calendar:views")) {
        listOf(CalendarView.AGENDA to "Agenda", CalendarView.DAY to "Day", CalendarView.WEEK to "Week").forEachIndexed { index, (value, label) ->
            SegmentedButton(view == value, { choose(value) }, SegmentedButtonDefaults.itemShape(index, 3), Modifier.testTag("calendar:view:" + value.name.lowercase())) { Text(label) }
        }
    }
}

@Composable
internal fun CalendarPeriodBar(view: CalendarView, s: CalendarState, store: CalendarStore) {
    val p = Pp.colors
    val days = CalendarGrid.days(view, s.selectedDay, s.weekStart)
    val step = if (view == CalendarView.WEEK) 7L else 1L
    val title = if (view == CalendarView.WEEK) {
        val first = days.first().date; val last = days.last().date
        if (first.month == last.month) first.format(DateTimeFormatter.ofPattern("d", Locale.getDefault())) + " – " + last.format(DateTimeFormatter.ofPattern("d MMMM yyyy", Locale.getDefault()))
        else first.format(DateTimeFormatter.ofPattern("d MMM", Locale.getDefault())) + " – " + last.format(DateTimeFormatter.ofPattern("d MMM yyyy", Locale.getDefault()))
    } else s.selectedDay.date.format(DateTimeFormatter.ofPattern("EEEE, d MMMM yyyy", Locale.getDefault()))
    Row(Modifier.fillMaxWidth().padding(horizontal = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        ActionIcon(if (view == CalendarView.WEEK) "Previous week" else "Previous day", Icons.Outlined.ChevronLeft) { store.select(s.selectedDay.plus(-step)) }
        Text(title, Modifier.weight(1f).semantics { heading() }.testTag("calendar:period"), style = geist(16.sp, FontWeight.SemiBold), color = p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
        TextButton({ store.today() }) { Text("Today") }
        ActionIcon(if (view == CalendarView.WEEK) "Next week" else "Next day", Icons.Outlined.ChevronRight) { store.select(s.selectedDay.plus(step)) }
    }
}

@Composable
internal fun CalendarTimeGrid(view: CalendarView, s: CalendarState, store: CalendarStore, modifier: Modifier = Modifier) {
    val p = Pp.colors
    val days = remember(view, s.selectedDay, s.weekStart) { CalendarGrid.days(view, s.selectedDay, s.weekStart) }
    val blocks = remember(s.occurrences, days, s.zone) { CalendarGrid.layout(s.occurrences, days, s.zone) }
    val allDay = remember(s.occurrences, days, s.zone) { CalendarGrid.allDay(s.occurrences, days, s.zone) }
    var now by remember { mutableLongStateOf(Instant.now().epochSecond) }
    LaunchedEffect(Unit) { while (true) { delay(30_000); now = Instant.now().epochSecond } }
    val nowLine = CalendarGrid.now(now, days, s.zone)
    val today = CalendarDay.at(Instant.ofEpochSecond(now), s.zone)
    val scroll = rememberScrollState()
    val hourPx = with(LocalDensity.current) { HourHeight.toPx() }
    // Open on the working day, or an hour before now when today is shown.
    LaunchedEffect(view, days.first()) { scroll.scrollTo(((nowLine?.minute?.div(60)?.minus(1) ?: 8).coerceIn(0, 23) * hourPx).toInt()) }
    Column(modifier.testTag("calendar:grid")) {
        Row(Modifier.fillMaxWidth()) {
            Spacer(Modifier.width(GutterWidth))
            days.forEachIndexed { index, day ->
                val isToday = day == today
                Column(Modifier.weight(1f).clickable(onClickLabel = "Show this day") { store.select(day) }.padding(vertical = 4.dp).testTag("calendar:column:" + day.text),
                    horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(day.date.format(DateTimeFormatter.ofPattern(if (view == CalendarView.WEEK) "EEE" else "EEEE", Locale.getDefault())), style = geist(11.sp), color = p.muted, maxLines = 1)
                    Box(Modifier.size(28.dp).clip(CircleShape).background(if (isToday) p.accent else Color.Transparent), contentAlignment = Alignment.Center) {
                        Text(day.date.dayOfMonth.toString(), style = geist(14.sp, if (isToday) FontWeight.Bold else FontWeight.Medium), color = if (isToday) Color.White else p.ink)
                    }
                    allDay[index].forEach { item ->
                        Text(item.title ?: "Event", Modifier.padding(horizontal = 2.dp, vertical = 1.dp).fillMaxWidth().clip(RoundedCornerShape(4.dp)).background(p.wash)
                            .clickable { store.openEvent(item.key) }.padding(horizontal = 4.dp, vertical = 2.dp).testTag("calendar:allday:" + item.eventId),
                            style = geist(11.sp, FontWeight.Medium), color = p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                }
            }
        }
        Seam(Modifier.fillMaxWidth().height(1.dp))
        Box(Modifier.weight(1f).verticalScroll(scroll)) {
            Row(Modifier.fillMaxWidth().height(HourHeight * 24)) {
                Column(Modifier.width(GutterWidth)) {
                    (0 until 24).forEach { hour ->
                        Box(Modifier.height(HourHeight).fillMaxWidth(), contentAlignment = Alignment.TopEnd) {
                            if (hour > 0) Text("%02d:00".format(hour), Modifier.padding(end = 6.dp).offset(y = (-7).dp), style = geist(10.sp), color = p.muted)
                        }
                    }
                }
                days.forEachIndexed { index, day ->
                    BoxWithConstraints(Modifier.weight(1f).fillMaxHeight()) {
                        val width = maxWidth
                        Column(Modifier.fillMaxSize()) { repeat(24) { Box(Modifier.height(HourHeight).fillMaxWidth()) { Seam(Modifier.fillMaxWidth().height(1.dp)) } } }
                        Box(Modifier.fillMaxHeight().width(1.dp).background(p.rule))
                        blocks.filter { it.dayIndex == index }.forEach { block -> GridBlock(block, width, s.zone.id) { store.openEvent(block.occurrence.key) } }
                        if (nowLine?.dayIndex == index) {
                            val y = HourHeight * (nowLine.minute / 60f)
                            Box(Modifier.offset(y = y - 1.dp).fillMaxWidth().height(2.dp).background(NowRed).testTag("calendar:now")
                                .semantics { contentDescription = "Now, " + "%02d:%02d".format(nowLine.minute / 60, nowLine.minute % 60) })
                            Box(Modifier.offset(x = (-4).dp, y = y - 4.dp).size(8.dp).clip(CircleShape).background(NowRed))
                        }
                    }
                }
            }
        }
    }
}

private val NowRed = Color(0xFFE5484D)

@Composable
private fun GridBlock(block: CalendarGridBlock, columnWidth: androidx.compose.ui.unit.Dp, zone: String, open: () -> Unit) {
    val p = Pp.colors
    val o = block.occurrence
    val laneWidth = columnWidth / block.lanes
    val top = HourHeight * (block.startMinute / 60f)
    val height = HourHeight * ((block.endMinute - block.startMinute) / 60f)
    val cancelled = o.status == "cancelled"
    val time = calendarTime(o.startsAt, zone) + " – " + calendarTime(o.endsAt, zone)
    Column(Modifier.offset(x = laneWidth * block.lane, y = top).width(laneWidth).height(height).padding(horizontal = 1.dp, vertical = 1.dp)
        .clip(RoundedCornerShape(5.dp)).background(if (cancelled) p.wash else p.accent.copy(alpha = .16f))
        .clickable(onClickLabel = "Open event", onClick = open).padding(horizontal = 4.dp, vertical = 2.dp)
        .semantics(mergeDescendants = true) { contentDescription = (o.title ?: "Event") + ", " + time + if (cancelled) ", cancelled" else "" }
        .testTag("calendar:block:" + o.eventId)) {
        Text(o.title ?: "Event", style = geist(11.sp, FontWeight.SemiBold), color = p.ink, maxLines = if (height > 40.dp) 2 else 1, overflow = TextOverflow.Ellipsis,
            textDecoration = if (cancelled) TextDecoration.LineThrough else null)
        if (height > 34.dp) Text(time, style = geist(10.sp), color = p.secondary, maxLines = 1, overflow = TextOverflow.Clip)
    }
}
