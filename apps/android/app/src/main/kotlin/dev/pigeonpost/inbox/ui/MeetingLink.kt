package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import dev.pigeonpost.core.InboxState
import dev.pigeonpost.core.calendar.*
import dev.pigeonpost.inbox.InboxViewModel
import java.time.Instant
import java.time.format.DateTimeFormatter

@Composable
internal fun MeetingLinkDialog(model: InboxViewModel, inbox: InboxState, code: String, s: CalendarMeetingLookupState, zoneId: String) {
    key(code, s.contextId) {
        var selected by remember { mutableStateOf<String?>(null) }
        val lookup = model.meetingLookup
        val choices = s.information?.choices.orEmpty()
        val information = s.information
        val zone = CalendarTime.displayZone(zoneId)
        val busy = s.loading || s.mailbox != inbox.acting?.address || s.code != code
        fun reload() { lookup?.load(code, s.contextId) }
        PageDialog("Meeting " + code.chunked(5).joinToString("-"), model::closeMeetingLink) {
            if (inbox.mailboxes.isNotEmpty()) CalendarChoice("Join as", inbox.acting?.address.orEmpty(), inbox.mailboxes.map { it.address to it.name }, !s.loading) { address ->
                inbox.mailboxes.firstOrNull { it.address == address }?.let { model.inbox.switchMailbox(it) }
            }
            Text("Choose the occurrence you want to join. The organizer’s lobby and admission settings apply.", color = Pp.colors.secondary)
            if (lookup == null) Text("Meeting lookup is unavailable right now.", color = Pp.colors.heldText)
            else if (busy) Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) { CircularProgressIndicator(Modifier.size(24.dp)); Text("Loading meeting…") }
            s.error?.let { Text(it, color = Pp.colors.heldText) }
            if (!busy && information != null && choices.isEmpty()) Text(if (information.state == "not_open") "This meeting is not open yet or its window has ended." else "This meeting is unavailable to the selected inbox.", color = Pp.colors.secondary)
            choices.forEach { choice ->
                val label = Instant.ofEpochSecond(choice.startsAt!!).atZone(zone).format(DateTimeFormatter.ofPattern("EEE, d MMM · HH:mm")) + " – " + calendarTime(choice.endsAt, zone.id)
                Row(Modifier.fillMaxWidth().clickable { selected = choice.occurrenceId }.padding(vertical = 8.dp).testTag("meeting:occurrence:" + choice.occurrenceId), verticalAlignment = Alignment.CenterVertically) {
                    RadioButton(selected == choice.occurrenceId, { selected = choice.occurrenceId })
                    Text(label, Modifier.weight(1f))
                }
            }
            Text(zone.id, color = Pp.colors.muted)
            val canJoin = !busy && model.calls != null && choices.any { it.occurrenceId == selected }
            Button({ selected?.let { model.joinMeetingLink(it, false, s.contextId, code) } }, enabled = canJoin, modifier = Modifier.fillMaxWidth().testTag("meeting:voice")) { Text("Join by voice") }
            OutlinedButton({ selected?.let { model.joinMeetingLink(it, true, s.contextId, code) } }, enabled = canJoin, modifier = Modifier.fillMaxWidth().testTag("meeting:video")) { Text("Join with video") }
            OutlinedButton(::reload, enabled = !s.loading && lookup != null) { Text("Reload meeting") }
        }
    }
}
