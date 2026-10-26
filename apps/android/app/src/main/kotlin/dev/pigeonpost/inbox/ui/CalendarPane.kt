package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pigeonpost.core.calendar.*
import java.time.Instant
import java.time.format.DateTimeFormatter
import java.util.Locale

@Composable
fun CalendarPane(store: CalendarStore, state: CalendarState, back: () -> Unit, joinMeeting: ((CalendarEventKey, Boolean, String) -> Unit)? = null) {
    key(state.contextId) { CalendarContents(store, state, back, joinMeeting) }
}

@Composable
private fun CalendarContents(store: CalendarStore, s: CalendarState, back: () -> Unit, joinMeeting: ((CalendarEventKey, Boolean, String) -> Unit)?) {
    val p = Pp.colors
    val editor = s.editor
    var tools by remember { mutableStateOf(false) }
    var discard by remember { mutableStateOf<CalendarPendingAttempt?>(null) }
    val context = s.contextId
    Column(Modifier.fillMaxSize().background(p.ground).testTag("calendar")) {
        Row(Modifier.fillMaxWidth().heightIn(min = 60.dp).padding(horizontal = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            ActionIcon("Back to inbox", Icons.AutoMirrored.Outlined.ArrowBack, action = back)
            Column(Modifier.weight(1f)) {
                Text("Calendar", style = geist(22.sp, FontWeight.Bold), color = p.ink)
                Text(s.mailbox.orEmpty(), style = geistMono(11.sp), color = p.muted, maxLines = 1)
            }
            ActionIcon("Calendar settings", Icons.Outlined.Tune) { tools = true }
            IconButton({ store.beginEditor(CalendarEventDraft(context, s.selectedDay.text, tzid = s.zone.id, calendarId = s.folders.firstOrNull { it.isDefault == true }?.id)) }, enabled = s.canCreate && s.pending.none { it.kind == CalendarAttemptKind.EVENT }, modifier = Modifier.testTag("calendar:new")) {
                Icon(Icons.Outlined.Add, "New event", tint = if (s.canCreate) p.accent else p.muted)
            }
        }
        if (s.loading) LinearProgressIndicator(Modifier.fillMaxWidth().height(2.dp)) else Seam(Modifier.fillMaxWidth().height(1.dp))
        if (!s.enabled) { EmptyPane("Calendar is unavailable", "Open an inbox with calendar access.", Modifier.weight(1f)); return@Column }
        LazyColumn(Modifier.weight(1f).testTag("calendar:agenda"), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            item { MonthPicker(s, store) }
            s.error?.let { message -> item { CalendarError(message, store::clearError) } }
            if (s.attemptsUnreadable) item {
                Text("Saved changes could not be opened. Creating another item is paused to avoid duplicates.", color = p.heldText)
                OutlinedButton({ store.loadAttempts() }) { Text("Reload saved changes") }
            }
            items(s.pending, key = { "pending:" + it.id }) { attempt ->
                Surface(color = p.wash, shape = MaterialTheme.shapes.medium) {
                    Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text("Unconfirmed ${when (attempt.kind) { CalendarAttemptKind.EVENT -> "event"; CalendarAttemptKind.CALENDAR -> "calendar"; CalendarAttemptKind.IMPORT -> "import" }}", style = geist(15.sp, FontWeight.SemiBold), color = p.ink)
                        Text("This change may already be saved. Inspect the calendar or retry the original; its content and identity stay the same.", style = geist(13.sp), color = p.secondary)
                        if (!attempt.canRetry(Instant.now().epochSecond)) Text("The retry window has ended. Inspect the calendar before discarding this record.", color = p.heldText)
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button({ store.retry(attempt.id, context) { created -> created.eventId?.let { store.openEvent(CalendarEventKey(it)) } } }, enabled = s.busy.isEmpty() && attempt.canRetry(Instant.now().epochSecond)) { Text("Retry original") }
                            TextButton({ discard = attempt }, enabled = s.busy.isEmpty()) { Text("Discard record") }
                        }
                    }
                }
            }
            if (s.invitations.isNotEmpty()) item {
                Text("Invitations", style = geist(16.sp, FontWeight.SemiBold), color = p.ink)
                s.invitations.forEach { invite ->
                    ListItem(headlineContent = { Text(invite.title ?: "Event invitation") }, supportingContent = { Text(invite.organizer?.handle ?: invite.organizer?.name ?: "Calendar invitation") },
                        trailingContent = { Icon(Icons.Outlined.ChevronRight, null) }, modifier = Modifier.clickable { store.openEvent(invite.key) })
                }
            }
            item {
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(s.selectedDay.date.format(DateTimeFormatter.ofPattern("EEEE, d MMMM", Locale.getDefault())), style = geist(17.sp, FontWeight.SemiBold), color = p.ink)
                        Text(s.zone.id, style = geist(12.sp), color = p.muted)
                    }
                    ActionIcon("Refresh calendar", Icons.Outlined.Refresh) { store.refresh() }
                }
            }
            if (s.agenda.isEmpty()) item { Text(if (s.selectedFolders?.isEmpty() == true) "No calendars selected." else "No events on this day.", color = p.secondary, modifier = Modifier.padding(vertical = 18.dp)) }
            items(s.agenda, key = { "event:" + it.occurrenceId }) { row ->
                Surface(color = p.wash, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth().clickable { store.openEvent(row.key) }.testTag("calendar:event:" + row.eventId)) {
                    Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        Icon(if (row.meeting != null) Icons.Outlined.Videocam else Icons.Outlined.Event, null, tint = p.accent)
                        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text(row.title ?: "Event", style = geist(15.sp, FontWeight.SemiBold), color = p.ink)
                            Text(if (row.allDay == true) "All day" else calendarTime(row.startsAt, s.zone.id) + " – " + calendarTime(row.endsAt, s.zone.id), style = geist(13.sp), color = p.secondary)
                            row.location?.takeIf { it.isNotBlank() }?.let { Text(it, style = geist(12.sp), color = p.muted) }
                            if (row.status == "cancelled") Text("Cancelled", color = p.heldText)
                            else row.myRsvp?.let { Text("Your answer: $it", style = geist(12.sp), color = p.muted) }
                        }
                        Icon(Icons.Outlined.ChevronRight, null, tint = p.muted)
                    }
                }
            }
            item { Text("Calendar data is encrypted at rest and readable by Pigeonpost for invitations and reminders.", style = geist(11.sp), color = p.muted) }
        }
    }
    if (tools) CalendarTools(store, s) { tools = false }
    editor?.let { session -> CalendarEventEditor(session, s, store, { store.closeEditor(session.id) }) { key ->
        // Closing a sent form must not let its late success dismiss a replacement editor.
        if (store.state.value.editor?.id == session.id) { store.closeEditor(session.id); store.openEvent(key) }
    } }
    if (editor == null) s.openEvent?.let { event -> key(event) { CalendarEventDetails(event, s, store, { store.closeEvent() }, store::beginEditor,
        join = joinMeeting?.let { action -> { video -> action(event, video, s.contextId) } }) } }
    discard?.let { record -> Confirm("Discard unconfirmed record?", "Discarding this record does not undo an event or import that the server already accepted. Inspect your calendar first to avoid creating a duplicate.", "Discard record",
        { store.discard(record.id, context); discard = null }, { discard = null }, enabled = s.busy.isEmpty()) }
}

@Composable
private fun MonthPicker(s: CalendarState, store: CalendarStore) {
    val p = Pp.colors
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        ActionIcon("Previous month", Icons.Outlined.ChevronLeft) { store.select(s.selectedDay.plusMonths(-1)) }
        Text(s.selectedDay.date.format(DateTimeFormatter.ofPattern("MMMM yyyy", Locale.getDefault())), Modifier.weight(1f), style = geist(18.sp, FontWeight.SemiBold), color = p.ink)
        TextButton({ store.today() }) { Text("Today") }
        ActionIcon("Next month", Icons.Outlined.ChevronRight) { store.select(s.selectedDay.plusMonths(1)) }
    }
    val days = remember(s.selectedDay, s.weekStart) { CalendarMonthWindow(s.selectedDay, s.weekStart).days }
    Row(Modifier.fillMaxWidth()) {
        days.take(7).forEach { day -> Box(Modifier.weight(1f).height(30.dp), contentAlignment = Alignment.Center) { Text(day.date.format(DateTimeFormatter.ofPattern("EEEEE")), color = p.muted, style = geist(12.sp)) } }
    }
    days.chunked(7).forEach { week -> Row(Modifier.fillMaxWidth()) {
        week.forEach { day ->
            val selected = day == s.selectedDay
            val hasEvents = s.occurrences.any { it.occursOn(day, s.zone) }
            Surface(onClick = { store.select(day) }, modifier = Modifier.weight(1f).heightIn(min = 48.dp).testTag("calendar:day:" + day.text),
                color = if (selected) p.accent else p.ground, shape = MaterialTheme.shapes.small) {
                Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
                    Text(day.date.dayOfMonth.toString(), style = geist(14.sp, if (selected) FontWeight.Bold else FontWeight.Normal), color = if (selected) androidx.compose.ui.graphics.Color.White else if (day.date.month == s.selectedDay.date.month) p.ink else p.muted)
                    Text(if (hasEvents) "•" else " ", style = geist(9.sp), color = if (selected) androidx.compose.ui.graphics.Color.White else p.accent)
                }
            }
        }
    } }
}

internal fun calendarTime(instant: Long?, zone: String): String = instant?.let {
    runCatching { Instant.ofEpochSecond(it).atZone(CalendarTime.displayZone(zone)).format(DateTimeFormatter.ofPattern("HH:mm")) }.getOrNull()
} ?: "Time unavailable"

@Composable
internal fun CalendarError(message: String, dismiss: () -> Unit) {
    Surface(color = Pp.colors.wash, shape = MaterialTheme.shapes.small) {
        Row(Modifier.padding(start = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(message, Modifier.weight(1f), color = Pp.colors.heldText, style = geist(13.sp))
            ActionIcon("Dismiss calendar notice", Icons.Outlined.Close, action = dismiss)
        }
    }
}
