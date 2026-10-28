package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.pigeonpost.core.meetings.MeetingBrief
import dev.pigeonpost.core.meetings.MeetingNotesText
import dev.pigeonpost.core.meetings.MeetingRow
import dev.pigeonpost.inbox.InboxViewModel
import java.text.DateFormat
import java.util.Date

/**
 * Meeting notes (Phase 7): the web inbox's reader (`site-inbox/meeting-notes.js`) as a native page. A
 * brief is decrypted on this device with its Keystore-held notes key; everything shown came from
 * speech and is drawn as plain text.
 */
@Composable
fun MeetingNotesDialog(model: InboxViewModel) {
    val store = model.meetingNotes ?: return
    val s by store.state.collectAsStateWithLifecycle()
    val p = Pp.colors
    val clipboard = @Suppress("DEPRECATION") LocalClipboardManager.current
    var confirmDelete by remember(s.contextId) { mutableStateOf<String?>(null) }
    LaunchedEffect(s.notice) { if (s.notice != null) { kotlinx.coroutines.delay(2500); store.clearNotice() } }
    val opened = s.opened
    val title = opened?.brief?.title ?: "Meeting notes"
    PageDialog(title, model::closeMeetingNotes, back = if (opened != null) store::back else null) {
        s.notice?.let { Text(it, color = p.secondary, modifier = Modifier.testTag("notes:notice")) }
        if (opened == null) {
            Text(MeetingNotesText.INTRO, style = geist(13.sp), color = p.secondary)
            when {
                s.loading && !s.loaded -> Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) { CircularProgressIndicator(Modifier.size(22.dp)); Text("Loading…") }
                s.error != null -> Text(s.error!!, color = p.heldText, modifier = Modifier.testTag("notes:error"))
                s.loaded && s.meetings.isEmpty() -> Text("No meeting notes yet.", color = p.secondary, modifier = Modifier.testTag("notes:empty"))
                else -> Column(Modifier.testTag("notes:list")) { s.meetings.forEach { MeetingRowItem(it) { store.open(it) } } }
            }
        } else {
            val brief = opened.brief
            when {
                opened.loading -> Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) { CircularProgressIndicator(Modifier.size(22.dp)); Text("Opening on this device…") }
                brief != null -> BriefView(brief)
                else -> Text(opened.problem ?: MeetingNotesText.CANNOT_OPEN, color = p.secondary, modifier = Modifier.testTag("notes:problem"))
            }
            if (!opened.loading) Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (brief != null) OutlinedButton({ clipboard.setText(AnnotatedString(brief.asText())) }, Modifier.testTag("notes:copy")) { Text("Copy as text") }
                TextButton({ confirmDelete = opened.row.id }, enabled = !s.busy, modifier = Modifier.testTag("notes:delete")) { Text("Delete my copy", color = p.blockedText) }
            }
        }
    }
    confirmDelete?.let { id ->
        Confirm("Delete your copy?", "Other attendees keep theirs.", "Delete", { confirmDelete = null; store.delete(id) }, { confirmDelete = null })
    }
}

private fun whenText(seconds: Long) = if (seconds <= 0) "" else DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(seconds * 1000))

@Composable
private fun MeetingRowItem(row: MeetingRow, open: () -> Unit) {
    val p = Pp.colors
    Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(10.dp)).clickable(onClick = open).padding(vertical = 10.dp, horizontal = 4.dp).testTag("notes:row:" + row.id)) {
        Text(whenText(row.createdAt), style = geist(14.5.sp, FontWeight.SemiBold), color = p.ink)
        val kept = if (row.state == "ready" && row.expiresAt > 0) " · kept until " + DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(row.expiresAt * 1000)) else ""
        Text("${MeetingNotesText.state(row)} · host ${row.host}$kept", style = geist(12.5.sp), color = p.muted)
    }
}

@Composable
private fun BriefView(b: MeetingBrief) {
    val p = Pp.colors
    @Composable fun heading(text: String) = Text(text, style = geist(15.sp, FontWeight.SemiBold), color = p.ink)
    @Composable fun bullets(items: List<String>) = Column(verticalArrangement = Arrangement.spacedBy(4.dp)) { items.forEach { Text("• $it", style = geist(14.sp), color = p.ink) } }
    SelectionContainer {
        Column(Modifier.testTag("notes:brief"), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            val meta = listOfNotNull(b.startedAt?.let { runCatching { DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date.from(java.time.Instant.parse(it))) }.getOrNull() },
                b.durationSeconds?.let { "${maxOf(1, Math.round(it / 60.0))} min" }, b.participants.takeIf { it.isNotEmpty() }?.joinToString(", "))
            if (meta.isNotEmpty()) Text(meta.joinToString(" · "), style = geist(12.5.sp), color = p.muted)
            if (b.draft) Text("A summary couldn’t be generated for this meeting. The transcript is below.", style = geist(13.sp), color = p.heldText)
            b.summary?.let { heading("Summary"); Text(it, style = geist(14.sp), color = p.ink) }
            if (b.decisions.isNotEmpty()) { heading("Decisions"); bullets(b.decisions) }
            if (b.actionItems.isNotEmpty()) { heading("Action items"); bullets(b.actionItems.map(MeetingBrief::actionText)) }
            if (b.openQuestions.isNotEmpty()) { heading("Open questions"); bullets(b.openQuestions) }
            if (b.transcript.isNotEmpty()) {
                heading("Transcript")
                b.transcript.forEach { t -> Text("${t.at}  ${t.speaker}: ${t.text}", style = geist(13.sp), color = p.secondary) }
            }
            Text("Transcribed and summarized automatically on Pigeonpost’s server" +
                (b.generatedBy?.stt?.let { " ($it${b.generatedBy?.llm?.let { l -> ", $l" } ?: ""})" } ?: "") +
                ". Speaker names come from each person’s own microphone; a microphone can pick up other voices.", style = geist(12.sp), color = p.muted)
        }
    }
}
