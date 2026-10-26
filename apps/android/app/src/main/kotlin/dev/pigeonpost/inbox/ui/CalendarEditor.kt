package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import dev.pigeonpost.core.calendar.*

@Composable
internal fun CalendarEventEditor(session: CalendarEditorSession, s: CalendarState, store: CalendarStore, dismiss: () -> Unit, saved: (CalendarEventKey) -> Unit) {
    val initial = session.initial
    var draft by remember(session.id) { mutableStateOf(session.draft) }
    DisposableEffect(session.id) { onDispose { store.retainEditorDraft(session.id, draft) } }
    var problem by remember { mutableStateOf<String?>(null) }
    var closeConfirm by remember { mutableStateOf(false) }
    val pending = if (initial.target == null) s.pending.firstOrNull { it.kind == CalendarAttemptKind.EVENT } else null
    val enabled = s.busy.isEmpty() && pending == null && draft.contextId == s.contextId
    val close = { if (draft != initial && pending == null) closeConfirm = true else dismiss() }
    PageDialog(if (initial.target == null) "New event" else "Edit event", close) {
        initial.target?.let { target -> Text(when (target.scope) { CalendarEventScope.ALL -> "Editing the whole event or series"; CalendarEventScope.THIS -> "Editing only this occurrence"; CalendarEventScope.FOLLOWING -> "Editing this and following occurrences" }, color = Pp.colors.secondary) }
        pending?.let { record ->
            Text("This event may already be saved. Its original content is locked until the result is confirmed.", color = Pp.colors.heldText)
            Button({ store.retry(record.id, draft.contextId) { created -> created.eventId?.let { saved(CalendarEventKey(it)) } } }, enabled = s.busy.isEmpty() && record.canRetry(java.time.Instant.now().epochSecond)) { Text("Retry original event") }
            TextButton(dismiss) { Text("Inspect calendar") }
        }
        problem?.let { CalendarError(it) { problem = null } }
        s.error?.let { CalendarError(it, store::clearError) }
        if (s.noticeWaiting) Text("A calendar notification is waiting. Finish this draft and close the event to open it.", color = Pp.colors.secondary)
        CalendarField("Event title", draft.title, enabled, "calendar:title") { draft = draft.copy(title = it) }
        CalendarField("Description", draft.description, enabled, singleLine = false) { draft = draft.copy(description = it) }
        CalendarField("Location", draft.location, enabled) { draft = draft.copy(location = it) }
        CalendarToggle("All day", draft.allDay, enabled) { draft = draft.copy(allDay = it, fold = null) }
        CalendarField("Start date (YYYY-MM-DD)", draft.startDay, enabled, "calendar:start-date") { draft = draft.copy(startDay = it, fold = null) }
        CalendarField(if (draft.allDay) "Last day (inclusive)" else "End date (YYYY-MM-DD)", draft.lastDay, enabled, "calendar:end-date") { draft = draft.copy(lastDay = it, fold = null) }
        if (!draft.allDay) {
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                OutlinedTextField(draft.startTime, { draft = draft.copy(startTime = it, fold = null) }, Modifier.weight(1f).testTag("calendar:start-time"), enabled = enabled, label = { Text("Start (HH:mm)") }, singleLine = true)
                OutlinedTextField(draft.endTime, { draft = draft.copy(endTime = it, fold = null) }, Modifier.weight(1f).testTag("calendar:end-time"), enabled = enabled, label = { Text("End (HH:mm)") }, singleLine = true)
            }
            CalendarField("Time zone", draft.tzid, enabled, "calendar:zone") { draft = draft.copy(tzid = it, fold = null) }
            Text("Times stay in the chosen time zone, including repeated meetings.", color = Pp.colors.muted)
            val ambiguous = listOf(draft.startDay + "T" + draft.startTime, draft.lastDay + "T" + draft.endTime).any { time ->
                runCatching { CalendarWallTime(time, draft.tzid).resolve() }.exceptionOrNull() is CalendarTimeError.Ambiguous
            }
            if (ambiguous) {
                Text("This time happens twice when the clocks change. Choose its offset.")
                CalendarChoice("Repeated time", draft.fold?.wire ?: "Choose offset", listOf("earlier" to "Earlier offset", "later" to "Later offset"), enabled) { draft = draft.copy(fold = CalendarFold.entries.first { fold -> fold.wire == it }) }
            }
        }
        if (!draft.isOccurrenceEdit) {
            if (s.folders.isNotEmpty()) CalendarChoice("Calendar", draft.calendarId ?: "", listOf("" to "Default calendar") + s.folders.map { it.id to (it.name ?: "Calendar") }, enabled) { draft = draft.copy(calendarId = it.takeIf(String::isNotEmpty)) }
            CalendarChoice("Repeat", if (draft.rrule.isEmpty()) "" else draft.rrule, listOf("" to "Does not repeat", "FREQ=DAILY" to "Every day", "FREQ=WEEKLY" to "Every week", "FREQ=MONTHLY" to "Every month"), enabled) { draft = draft.copy(rrule = it) }
            CalendarField("Recurrence rule (optional)", draft.rrule, enabled) { draft = draft.copy(rrule = it) }
            CalendarChoice("Availability", draft.transparency, listOf("opaque" to "Busy", "transparent" to "Free"), enabled) { draft = draft.copy(transparency = it) }
            CalendarChoice("Event privacy", draft.privacy, listOf("default" to "Default", "private" to "Private"), enabled) { draft = draft.copy(privacy = it) }
            CalendarChoice("Guest invitation language", draft.locale, listOf("en" to "English", "tr" to "Türkçe"), enabled) { draft = draft.copy(locale = it) }
            CalendarToggle("Include a voice/video meeting", draft.meeting.enabled, enabled) { draft = draft.copy(meeting = draft.meeting.copy(enabled = it)) }
            if (draft.meeting.enabled) {
                val m = draft.meeting
                CalendarChoice("Call size", m.transport, listOf("mesh" to "Up to 6 people", "sfu" to "Up to 30 people"), enabled) { draft = draft.copy(meeting = m.copy(transport = it)) }
                CalendarChoice("Who waits in the lobby?", m.waiting, listOf("guests" to "Guests", "everyone_except_hosts" to "Everyone except hosts", "none" to "No invited participants"), enabled) { draft = draft.copy(meeting = m.copy(waiting = it)) }
                CalendarChoice("Who admits participants?", m.admit, listOf("organizer" to "Organizer", "hosts" to "Organizer and cohosts", "delegated" to "Hosts and selected attendees"), enabled) { draft = draft.copy(meeting = m.copy(admit = it)) }
                if (m.admit == "delegated") CalendarField("Admitters (one handle or attendee ID per line)", m.admitters.joinToString("\n"), enabled, singleLine = false) { draft = draft.copy(meeting = m.copy(admitters = it.lines().map(String::trim).filter(String::isNotEmpty))) }
                CalendarToggle("Allow people with the link to knock", m.allowKnock, enabled) { draft = draft.copy(meeting = m.copy(allowKnock = it)) }
                CalendarToggle("Guests can see the participant list", m.guestsSeeList, enabled) { draft = draft.copy(meeting = m.copy(guestsSeeList = it)) }
                CalendarChoice("Open before the start", m.joinBeforeMinutes.toString(), (0..10).map { it.toString() to "$it minutes" }, enabled) { draft = draft.copy(meeting = m.copy(joinBeforeMinutes = it.toInt())) }
            }
            if (draft.target == null) {
                CalendarField("Invitees (one handle or email per line)", draft.invitees.joinToString("\n") { it.destination }, enabled, singleLine = false) { draft = draft.copy(invitees = it.lines().map { line -> CalendarInviteeDraft(line) }) }
                if (!s.emailEnabled) Text("Contact guests will not receive email yet. After saving, copy each guest's personal invitation link.", color = Pp.colors.secondary)
                CalendarField("Push reminders (minutes before)", draft.reminderText, enabled) { draft = draft.copy(reminderText = it) }
            }
        }
        Button({
            try {
                problem = null
                if (draft.target == null) store.createEvent(draft.fields(), draft.contextId, saved)
                else store.edit(draft.target!!.key, draft.editPayload(), draft.contextId, saved)
            } catch (failure: IllegalArgumentException) { problem = failure.message ?: "Review the event details." }
        }, enabled = enabled && (draft.target != null || s.canCreate), modifier = Modifier.fillMaxWidth().testTag("calendar:save")) {
            Text(if (s.busy.isNotEmpty()) "Saving…" else if (draft.target == null) "Create event" else "Save changes")
        }
    }
    if (closeConfirm) Confirm("Discard this draft?", "Your unsaved changes will be removed.", "Discard draft", dismiss, { closeConfirm = false })
}

@Composable
internal fun CalendarField(label: String, value: String, enabled: Boolean = true, tag: String = "", singleLine: Boolean = true, changed: (String) -> Unit) {
    OutlinedTextField(value, changed, Modifier.fillMaxWidth().then(if (tag.isNotEmpty()) Modifier.testTag(tag) else Modifier), enabled = enabled, label = { Text(label) }, singleLine = singleLine, minLines = if (singleLine) 1 else 2)
}

@Composable
internal fun CalendarToggle(label: String, value: Boolean, enabled: Boolean = true, changed: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(label, Modifier.weight(1f)); Switch(value, changed, enabled = enabled)
    }
}

@Composable
internal fun CalendarChoice(label: String, value: String, choices: List<Pair<String, String>>, enabled: Boolean = true, changed: (String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Column {
        Text(label, color = Pp.colors.secondary)
        Box {
            OutlinedButton({ expanded = true }, enabled = enabled, modifier = Modifier.fillMaxWidth()) { Text(choices.firstOrNull { it.first == value }?.second ?: value) }
            DropdownMenu(expanded, { expanded = false }) {
                choices.forEach { (key, title) -> DropdownMenuItem({ Text(title) }, { expanded = false; changed(key) }) }
            }
        }
    }
}
