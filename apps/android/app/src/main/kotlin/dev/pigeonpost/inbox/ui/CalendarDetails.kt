package dev.pigeonpost.inbox.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.pigeonpost.core.calendar.*
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.IOException

@Composable
internal fun CalendarEventDetails(key: CalendarEventKey, s: CalendarState, store: CalendarStore, dismiss: () -> Unit, edit: (CalendarEventDraft) -> Unit) {
    val response = s.details[key]
    val context = s.contextId
    val live = remember { AtomicBoolean(true) }
    DisposableEffect(Unit) { onDispose { live.set(false) } }
    var editSnapshot by remember { mutableStateOf<CalendarEventResponse?>(null) }
    var cancelSnapshot by remember { mutableStateOf<CalendarEventResponse?>(null) }
    var cancelScope by remember { mutableStateOf<CalendarEventScope?>(null) }
    var secret by remember { mutableStateOf<String?>(null) }
    var addPerson by remember { mutableStateOf<CalendarEventResponse?>(null) }
    var removePerson by remember { mutableStateOf<Pair<CalendarEventResponse, CalendarAttendee>?>(null) }
    var reminderDraft by remember { mutableStateOf<String?>(null) }
    var rsvpScope by remember { mutableStateOf(CalendarAnswerScope.ALL) }
    var localError by remember { mutableStateOf<String?>(null) }
    var export by remember { mutableStateOf<CalendarIcsExport?>(null) }
    var exported by remember { mutableStateOf(false) }
    val resolver = LocalContext.current.contentResolver
    val jobs = rememberCoroutineScope()
    DisposableEffect(Unit) { onDispose { export?.close() } }
    val saveCalendar = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("text/calendar")) { uri ->
        val packet = export; export = null
        if (packet != null && uri != null && live.get() && store.state.value.contextId == context) jobs.launch {
            try {
                withContext(Dispatchers.IO) { packet.write { resolver.openOutputStream(uri, "w") ?: throw IOException("No output stream") } }
                if (live.get()) exported = true
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { if (live.get()) localError = "The calendar file could not be saved. Export the event again to retry." }
            finally { packet.close() }
        } else packet?.close()
    }
    val busy = s.busy.isNotEmpty()
    PageDialog("Event", dismiss) {
        s.error?.let { CalendarError(it, store::clearError) }
        localError?.let { CalendarError(it) { localError = null } }
        if (response == null) {
            Text("Loading event…")
            OutlinedButton({ store.loadDetail(key) }) { Text("Reload event") }
        } else {
            val e = response.event
            val o = response.occurrence
            val recurring = !e.rrule.isNullOrBlank()
            val organizer = e.canEdit == true
            Text(o?.title ?: e.title ?: "Event", style = MaterialTheme.typography.headlineSmall)
            if ((o?.allDay ?: e.allDay) == true) Text("${o?.startDate ?: e.startDate} · all day")
            else Text("${o?.start ?: e.start} – ${o?.end ?: e.end}\n${o?.tzid ?: e.tzid ?: s.zone.id}")
            Text("Organizer: ${e.organizer?.handle ?: e.organizer?.name ?: e.organizer?.address ?: "Unavailable"}", color = Pp.colors.secondary)
            (o?.description ?: e.description)?.takeIf(String::isNotBlank)?.let { SelectionContainer { Text(it) } }
            (o?.location ?: e.location)?.takeIf(String::isNotBlank)?.let { Text("Location: $it") }
            if (recurring) Text("Repeats: ${e.rrule}", color = Pp.colors.secondary)
            if (e.status == "cancelled" || o?.status == "cancelled") Text("This event is cancelled.", color = Pp.colors.heldText)
            else {
                if (organizer) {
                    Button({ editSnapshot = response }, enabled = !busy, modifier = Modifier.fillMaxWidth()) { Text("Edit event") }
                    OutlinedButton({ cancelSnapshot = response; cancelScope = if (recurring) null else CalendarEventScope.ALL }, enabled = !busy, modifier = Modifier.fillMaxWidth()) { Text("Cancel event") }
                } else {
                    Text("Your answer: ${o?.myRsvp ?: e.my?.rsvp ?: "Not answered"}")
                    if (recurring && key.occurrenceId != null) CalendarChoice("Answer for", rsvpScope.wire, listOf("all" to "The whole series", "this" to "Only this occurrence"), !busy) { rsvpScope = CalendarAnswerScope.entries.first { scope -> scope.wire == it } }
                    CalendarAnswer.entries.forEach { answer -> OutlinedButton({ store.rsvp(key, answer, rsvpScope, context) }, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
                        Text(when (answer) { CalendarAnswer.ACCEPTED -> "Accept"; CalendarAnswer.TENTATIVE -> "Maybe"; CalendarAnswer.DECLINED -> "Decline" })
                    } }
                    if (s.folders.isNotEmpty()) CalendarChoice("Show invitation in", e.my?.calendarId ?: e.calendarId.orEmpty(), s.folders.map { it.id to (it.name ?: "Calendar") }, !busy) { store.moveInvitation(key, it, context) }
                }
                OutlinedButton({ reminderDraft = e.reminders.orEmpty().joinToString(", ") { it.minutesBefore.toString() } }, enabled = !busy, modifier = Modifier.fillMaxWidth()) { Text("My push reminders") }
            }
            e.meeting?.code?.let { code ->
                Text("Scheduled meeting", style = MaterialTheme.typography.titleMedium)
                SelectionContainer { Text(code.chunked(5).joinToString("-")) }
                Text("Joining follows this event's time window, lobby and admission settings.", color = Pp.colors.secondary)
            }
            Text("Participants", style = MaterialTheme.typography.titleMedium)
            e.attendees.orEmpty().forEach { person ->
                ListItem(headlineContent = { Text(person.name ?: person.handle ?: person.address ?: "Guest") }, supportingContent = {
                    Text(listOfNotNull(person.role, person.rsvp, if (organizer) person.contact else null, if (organizer) person.delivery else null).joinToString(" · "))
                })
                if (organizer && person.kind == "external") OutlinedButton({ store.guestLink(e.id, person.attendeeId, context) { link -> if (live.get()) secret = link.url } }, enabled = !busy) { Text("Get personal invitation link") }
                if (organizer && e.revision != null && e.status != "cancelled") TextButton({ removePerson = response to person }, enabled = !busy) { Text("Remove participant") }
            }
            if (organizer && e.revision != null && e.status != "cancelled") OutlinedButton({ addPerson = response }, enabled = !busy, modifier = Modifier.fillMaxWidth()) { Text("Add participant") }
            OutlinedButton({ store.prepareIcsExport(e.id, context) { packet ->
                if (live.get()) {
                    export?.close(); export = packet
                    try { saveCalendar.launch("pigeonpost-event.ics") }
                    catch (_: Exception) { packet.close(); export = null; localError = "No file picker is available. Try exporting again." }
                } else packet.close()
            } }, enabled = !busy) { Text("Export .ics file") }
            if (exported) Text("Calendar file saved.", color = Pp.colors.secondary)
            OutlinedButton({ store.loadDetail(key) }, enabled = !busy) { Text("Reload latest event") }
        }
    }
    editSnapshot?.let { snapshot ->
        val choose: (CalendarEventScope) -> Unit = { scope ->
            try { edit(CalendarEventDraft.from(CalendarEditTarget(snapshot, key, scope, context), s.zone.id)); editSnapshot = null }
            catch (failure: IllegalArgumentException) { localError = failure.message; editSnapshot = null }
        }
        if (snapshot.event.rrule.isNullOrBlank()) LaunchedEffect(snapshot) { choose(CalendarEventScope.ALL) }
        else CalendarScopeDialog("Edit which events?", key.occurrenceId != null, { editSnapshot = null }, choose)
    }
    cancelSnapshot?.let { snapshot ->
        val chosen = cancelScope
        if (chosen == null) CalendarScopeDialog("Cancel which events?", key.occurrenceId != null, { cancelSnapshot = null }) { cancelScope = it }
        else Confirm("Cancel ${if (chosen == CalendarEventScope.THIS) "this occurrence" else if (chosen == CalendarEventScope.FOLLOWING) "this and following occurrences" else "the event or series"}?",
            "Participants will see the cancellation. This uses the version you reviewed.", "Cancel event", {
                snapshot.event.revision?.let { store.cancel(key, it, chosen, context) }; cancelSnapshot = null
            }, { cancelSnapshot = null }, enabled = !busy)
    }
    reminderDraft?.let { text -> PageDialog("My push reminders", { reminderDraft = null }) {
        CalendarField("Minutes before (comma separated)", text, !busy) { reminderDraft = it }
        Text("Up to five reminders, from 0 to 40320 minutes before. Leave empty to remove yours. Other participants' reminders stay private.")
        Button({ try { store.reminders(key.eventId, CalendarReminderInput.parse(text).map { CalendarReminder(it, "push") }, context); reminderDraft = null } catch (failure: IllegalArgumentException) { localError = failure.message } }, enabled = !busy) { Text("Save my reminders") }
    } }
    addPerson?.let { snapshot -> CalendarParticipantDialog(snapshot, key, s, store) { addPerson = null } }
    removePerson?.let { (snapshot, person) -> Confirm("Remove this participant?", "This removes ${person.name ?: person.handle ?: "the participant"} from the whole event or series.", "Remove", {
        snapshot.event.revision?.let { store.attendees(key, it, CalendarAnswerScope.ALL, emptyList(), listOf(person.attendeeId), context) }; removePerson = null
    }, { removePerson = null }, enabled = !busy) }
    secret?.let { url -> CalendarSecretDialog("Personal invitation link", "Share this link only with its intended guest. It can grant access to the invitation.", url) { secret = null } }
}

@Composable
private fun CalendarScopeDialog(title: String, occurrence: Boolean, dismiss: () -> Unit, choose: (CalendarEventScope) -> Unit) {
    PageDialog(title, dismiss) {
        if (occurrence) {
            OutlinedButton({ choose(CalendarEventScope.THIS) }, Modifier.fillMaxWidth()) { Text("Only this occurrence") }
            OutlinedButton({ choose(CalendarEventScope.FOLLOWING) }, Modifier.fillMaxWidth()) { Text("This and following occurrences") }
        }
        OutlinedButton({ choose(CalendarEventScope.ALL) }, Modifier.fillMaxWidth()) { Text("The whole event or series") }
    }
}

@Composable
private fun CalendarParticipantDialog(snapshot: CalendarEventResponse, key: CalendarEventKey, s: CalendarState, store: CalendarStore, dismiss: () -> Unit) {
    var person by remember { mutableStateOf(CalendarInviteeDraft()) }
    var scope by remember { mutableStateOf(CalendarAnswerScope.ALL) }
    var error by remember { mutableStateOf<String?>(null) }
    PageDialog("Add participant", dismiss) {
        CalendarField("Handle or contact email", person.destination) { person = person.copy(destination = it) }
        CalendarField("Name (optional)", person.name) { person = person.copy(name = it) }
        CalendarToggle("Optional attendance", person.optional) { person = person.copy(optional = it) }
        if (person.destination.trim().startsWith('/')) CalendarToggle("Cohost", person.cohost) { person = person.copy(cohost = it) }
        if (key.occurrenceId != null && !snapshot.event.rrule.isNullOrBlank()) CalendarChoice("Invite for", scope.wire, listOf("all" to "The whole series", "this" to "Only this occurrence")) { scope = CalendarAnswerScope.entries.first { value -> value.wire == it } }
        if (!s.emailEnabled) Text("Email delivery is unavailable. You can copy a contact guest's personal link after adding them.")
        error?.let { Text(it, color = Pp.colors.heldText) }
        Button({ try {
            val fields = person.fields() ?: throw IllegalArgumentException("Enter a participant.")
            snapshot.event.revision?.let { store.attendees(key, it, scope, listOf(fields), emptyList(), s.contextId) }; dismiss()
        } catch (failure: IllegalArgumentException) { error = failure.message } }, enabled = s.busy.isEmpty()) { Text("Add participant") }
    }
}

@Suppress("DEPRECATION")
@Composable
internal fun CalendarSecretDialog(title: String, explanation: String, url: String, dismiss: () -> Unit) {
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    var copied by remember { mutableStateOf(false) }
    PageDialog(title, dismiss) {
        Text(explanation)
        SelectionContainer { Text(url) }
        Button({ clipboard.setText(androidx.compose.ui.text.AnnotatedString(url)); copied = true }) { Text(if (copied) "Copied" else "Copy link") }
    }
}
