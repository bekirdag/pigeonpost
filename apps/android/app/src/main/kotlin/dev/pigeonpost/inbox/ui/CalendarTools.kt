package dev.pigeonpost.inbox.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.pigeonpost.core.calendar.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*
import java.io.IOException
import java.util.concurrent.atomic.AtomicBoolean

@Composable
internal fun CalendarTools(store: CalendarStore, s: CalendarState, dismiss: () -> Unit) {
    val resolver = LocalContext.current.contentResolver
    val live = remember { AtomicBoolean(true) }
    DisposableEffect(Unit) { onDispose { live.set(false) } }
    val context = s.contextId
    var page by remember { mutableStateOf("main") }
    var error by remember { mutableStateOf<String?>(null) }
    var secret by remember { mutableStateOf<String?>(null) }
    var preview by remember { mutableStateOf<CalendarImportPreview?>(null) }
    var delete by remember { mutableStateOf<CalendarFolder?>(null) }
    var selectedFolder by remember { mutableStateOf<CalendarFolder?>(null) }
    var result by remember { mutableStateOf<String?>(null) }
    val busy = s.busy.isNotEmpty()
    val chooseImport = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null && live.get() && store.state.value.contextId == context) store.previewLocalImport({
            withContext(Dispatchers.IO) {
                try { resolver.openInputStream(uri)?.use { input ->
                    val buffer = java.io.ByteArrayOutputStream()
                    val chunk = ByteArray(8192)
                    while (true) { val n = input.read(chunk); if (n < 0) break; if (buffer.size() + n > 512 * 1024) throw IOException("Calendar imports must be at most 512 KB."); buffer.write(chunk, 0, n) }
                    buffer.toByteArray()
                } ?: throw IOException("The selected calendar file could not be opened.") }
                catch (_: SecurityException) { throw IOException("The selected calendar file could not be opened.") }
            }
        }, null, context) { if (live.get()) preview = it }
    }
    PageDialog(if (page == "settings") "Calendar preferences" else if (page == "folders") "Your calendars" else "Calendar settings", dismiss,
        back = if (page == "main") null else ({ page = "main" })) {
        error?.let { CalendarError(it) { error = null } }
        s.error?.let { CalendarError(it, store::clearError) }
        result?.let { Text(it, color = Pp.colors.secondary) }
        when (page) {
            "settings" -> CalendarPreferences(s, store)
            "folders" -> {
                Text("Choose which calendars appear in your agenda. An empty selection shows no events.")
                TextButton({ store.selectFolders(null) }) { Text("Show all calendars") }
                TextButton({ store.selectFolders(emptySet()) }) { Text("Hide all calendars") }
                s.folders.forEach { folder ->
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Checkbox(s.selectedFolders?.contains(folder.id) ?: true, { on ->
                            val ids = s.selectedFolders ?: s.folders.map { it.id }.toSet()
                            store.selectFolders(if (on) ids + folder.id else ids - folder.id)
                        })
                        Column(Modifier.weight(1f)) {
                            Text(folder.name ?: "Calendar"); if (folder.isDefault == true) Text("Default", color = Pp.colors.muted)
                            TextButton({ selectedFolder = folder }) { Text("Edit calendar") }
                            if (folder.revision != null) TextButton({ delete = folder }, enabled = !busy) { Text("Delete calendar") }
                            OutlinedButton({ store.feed(folder.id, context) { if (live.get()) result = if (it.enabled == true) "Subscription is enabled (${it.projection})." else "Subscription is disabled." } }, enabled = !busy) { Text("Subscription status") }
                            OutlinedButton({ store.setFeed(folder.id, true, "busy", context) { if (live.get()) secret = it.url } }, enabled = !busy) { Text("Create or rotate busy-only link") }
                            OutlinedButton({ store.setFeed(folder.id, true, "personal_redacted", context) { if (live.get()) secret = it.url } }, enabled = !busy) { Text("Create or rotate personal link") }
                            TextButton({ store.setFeed(folder.id, false, "busy", context) { if (live.get()) result = "Subscription link revoked." } }, enabled = !busy) { Text("Revoke subscription link") }
                        }
                    }
                    HorizontalDivider()
                }
                CalendarFolderForm(null, s, store) {}
                Text("Rotating a subscription immediately stops the previous link. Personal links show your own titles and locations; private events and invitations remain Busy. Share links carefully.", color = Pp.colors.secondary)
            }
            else -> {
                OutlinedButton({ page = "folders" }, Modifier.fillMaxWidth()) { Text("Calendars and subscriptions") }
                OutlinedButton({ page = "settings" }, Modifier.fillMaxWidth()) { Text("Privacy, time zone and working hours") }
                OutlinedButton({ chooseImport.launch(arrayOf("text/calendar", "application/ics", "application/octet-stream")) }, enabled = !busy && s.canCreate && s.pending.none { it.kind == CalendarAttemptKind.IMPORT }, modifier = Modifier.fillMaxWidth()) { Text("Import a local .ics file") }
                Text("Imports use the file you choose. Calendar URLs inside it are never fetched. Review the entries before adding them.", color = Pp.colors.secondary)
                if (s.filteredInvitations.isNotEmpty()) {
                    Text("Filtered invitations", style = MaterialTheme.typography.titleMedium)
                    Text("Open an invitation to review it. Opening does not accept it or change your contacts.", color = Pp.colors.secondary)
                    s.filteredInvitations.forEach { invite -> TextButton({ store.openEvent(invite.key); dismiss() }) { Text(invite.title ?: "Invitation") } }
                }
            }
        }
    }
    selectedFolder?.let { folder -> PageDialog("Edit calendar", { selectedFolder = null }) { CalendarFolderForm(folder, s, store) { selectedFolder = null } } }
    delete?.let { folder -> Confirm("Delete ${folder.name ?: "calendar"}?", "Events you organize in this calendar will be cancelled and participants notified. Invitations move to your default calendar.", "Delete calendar", {
        folder.revision?.let { store.deleteCalendar(folder.id, it, context) }; delete = null
    }, { delete = null }, enabled = !busy) }
    secret?.let { url -> CalendarSecretDialog("Calendar subscription link", "Anyone with this link can read its calendar projection. Rotating or revoking it stops the previous link.", url) { secret = null } }
    preview?.let { draft -> key(draft.draftId) { CalendarImportDialog(draft, s, store, { preview = null }) { count -> preview = null; result = "Added $count events." } } }
}

@Composable
private fun CalendarFolderForm(folder: CalendarFolder?, s: CalendarState, store: CalendarStore, done: () -> Unit) {
    var name by remember(folder) { mutableStateOf(folder?.name.orEmpty()) }
    var color by remember(folder) { mutableStateOf(folder?.color ?: "blue") }
    var makeDefault by remember(folder) { mutableStateOf(folder?.isDefault == true) }
    var error by remember { mutableStateOf<String?>(null) }
    CalendarField(if (folder == null) "New calendar name" else "Calendar name", name) { name = it }
    CalendarChoice("Color", color, listOf("blue", "green", "orange", "red", "purple", "teal", "gray").map { it to it.replaceFirstChar(Char::uppercase) }) { color = it }
    if (folder != null) CalendarToggle("Default calendar", makeDefault) { makeDefault = it }
    error?.let { Text(it, color = Pp.colors.heldText) }
    Button({
        if (name.isBlank()) error = "Give the calendar a name."
        else if (folder == null) store.createCalendar(name.trim(), color, s.contextId) { done() }
        else folder.revision?.let { store.updateCalendar(folder.id, it, buildJsonObject { put("name", name.trim()); put("color", color); put("is_default", makeDefault) }, s.contextId); done() }
    }, enabled = s.busy.isEmpty() && (folder != null || s.canCreate && s.pending.none { it.kind == CalendarAttemptKind.CALENDAR })) { Text(if (folder == null) "Create calendar" else "Save calendar") }
}

@Composable
private fun CalendarPreferences(s: CalendarState, store: CalendarStore) {
    // Preserve this person's draft and its baseline while background metadata refreshes.
    val baseline = remember { s.settings }
    var zone by remember { mutableStateOf(baseline.timezone ?: s.zone.id) }
    var previews by remember { mutableStateOf(baseline.previews ?: false) }
    var week by remember { mutableStateOf((baseline.weekStart ?: 1).toString()) }
    var start by remember { mutableStateOf(baseline.workingHours?.start ?: "09:00") }
    var end by remember { mutableStateOf(baseline.workingHours?.end ?: "17:00") }
    var days by remember { mutableStateOf((baseline.workingHours?.days ?: listOf(1, 2, 3, 4, 5)).toSet()) }
    var hoursChanged by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    CalendarToggle("Show event previews in notifications", previews) { previews = it }
    CalendarField("Time zone", zone) { zone = it }
    CalendarChoice("First day of week", week, listOf("0" to "Sunday", "1" to "Monday", "6" to "Saturday")) { week = it }
    CalendarField("Work starts (HH:mm)", start) { start = it; hoursChanged = true }
    CalendarField("Work ends (HH:mm)", end) { end = it; hoursChanged = true }
    listOf("Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday").forEachIndexed { index, day -> CalendarToggle(day, index in days) { on -> days = if (on) days + index else days - index; hoursChanged = true } }
    error?.let { Text(it, color = Pp.colors.heldText) }
    Button({ try {
        CalendarTime.zone(zone)
        if (hoursChanged) require(Regex("[0-9]{2}:[0-9]{2}").matches(start) && Regex("[0-9]{2}:[0-9]{2}").matches(end) && java.time.LocalTime.parse(end) > java.time.LocalTime.parse(start)) { "Choose valid working hours with an end after the start." }
        val fields = buildJsonObject {
            if (zone != baseline.timezone) put("timezone", zone)
            if (previews != (baseline.previews ?: false)) put("previews", previews)
            if (week.toInt() != (baseline.weekStart ?: 1)) put("week_start", week.toInt())
            if (hoursChanged) put("working_hours", buildJsonObject { put("start", start); put("end", end); put("days", JsonArray(days.sorted().map(::JsonPrimitive))) })
        }
        store.saveSettings(fields, s.contextId)
    } catch (failure: IllegalArgumentException) { error = failure.message ?: "Review the calendar preferences." } }, enabled = s.busy.isEmpty()) { Text("Save preferences") }
}

@Composable
private fun CalendarImportDialog(draft: CalendarImportPreview, s: CalendarState, store: CalendarStore, dismiss: () -> Unit, done: (Int) -> Unit) {
    var selected by remember { mutableStateOf(draft.events.orEmpty().filter { it.importable == true }.take(200).map { it.index }.toSet()) }
    var calendar by remember { mutableStateOf(draft.calendarId) }
    PageDialog("Review calendar import", dismiss) {
        draft.warnings.orEmpty().forEach { Text(it, color = Pp.colors.heldText) }
        if (s.folders.isNotEmpty()) CalendarChoice("Import into", calendar ?: "", listOf("" to "Default calendar") + s.folders.map { it.id to (it.name ?: "Calendar") }) { calendar = it.takeIf(String::isNotEmpty) }
        draft.events.orEmpty().forEach { event -> Row(Modifier.fillMaxWidth()) {
            Checkbox(event.index in selected, { on -> selected = if (on) selected + event.index else selected - event.index }, enabled = event.importable == true && s.busy.isEmpty())
            Column(Modifier.weight(1f)) { Text(event.title ?: "Untitled event"); Text(event.start ?: event.startDate ?: "Time unavailable", color = Pp.colors.secondary); event.warnings.orEmpty().forEach { Text(it, color = Pp.colors.heldText) } }
        } }
        s.error?.let { CalendarError(it, store::clearError) }
        if (s.pending.any { it.kind == CalendarAttemptKind.IMPORT }) Text("The original import is unconfirmed. Close this preview and retry it from the calendar.", color = Pp.colors.heldText)
        Button({ store.confirmImport(draft.draftId, selected.toList(), calendar, s.contextId, done) }, enabled = selected.size in 1..200 && s.canCreate && s.busy.isEmpty() && s.pending.none { it.kind == CalendarAttemptKind.IMPORT }) { Text("Import ${selected.size} events") }
    }
}
