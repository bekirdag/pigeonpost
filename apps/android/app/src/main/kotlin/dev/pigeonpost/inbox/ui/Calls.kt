@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.CallMade
import androidx.compose.material.icons.automirrored.outlined.CallMissed
import androidx.compose.material.icons.automirrored.outlined.CallReceived
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.pigeonpost.core.*

/** Who to block, where the request came from, and what a report would point at. */
data class BlockTarget(val peer: String, val name: String, val messageId: String? = null, val callId: String? = null, val report: Boolean = false)

/** Every Block action (§15.2) opens the same confirmation; screens ask for it through this. */
val LocalAskBlock = staticCompositionLocalOf<(BlockTarget) -> Unit> { {} }

/**
 * "Block /k/abcd…? They won't be able to message or call you, and won't be told." One option,
 * Also report as spam (on when blocking from Filtered), and "everyone at /ns" where a namespace
 * has an owner.
 */
@Composable
fun BlockSheet(target: BlockTarget, busy: Boolean, confirm: (wide: Boolean, report: Boolean) -> Unit, dismiss: () -> Unit) {
    var report by rememberSaveable(target) { mutableStateOf(target.report) }
    var wide by rememberSaveable(target) { mutableStateOf(false) }
    val wildcard = wildcardFor(target.peer)
    AlertDialog(onDismissRequest = dismiss, modifier = Modifier.testTag("blockSheet"),
        title = { Text("Block ${target.name}?") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("They won’t be able to message or call you, and won’t be told.")
                Row(Modifier.fillMaxWidth().toggleable(report, role = Role.Checkbox, onValueChange = { report = it }), verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(report, null); Text("Also report as spam", Modifier.padding(start = 6.dp))
                }
                if (wildcard != null) Row(Modifier.fillMaxWidth().toggleable(wide, role = Role.Checkbox, onValueChange = { wide = it }), verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(wide, null); Text("Block everyone at ${wildcard.removeSuffix("/*")}", Modifier.padding(start = 6.dp))
                }
            }
        },
        confirmButton = { TextButton({ confirm(wide, report) }, enabled = !busy) { Text(if (busy) "Blocking…" else "Block", color = Pp.colors.blockedText) } },
        dismissButton = { TextButton(dismiss) { Text("Cancel") } })
}

/** "You blocked this person — Unblock", in place of the composer. */
@Composable
fun BlockedBanner(name: String, busy: Boolean, unblock: () -> Unit) {
    val p = Pp.colors
    Row(Modifier.fillMaxWidth().padding(12.dp).clip(RoundedCornerShape(14.dp)).background(p.blockedFill).padding(start = 16.dp, end = 6.dp, top = 6.dp, bottom = 6.dp)
        .testTag("blockedBanner"), verticalAlignment = Alignment.CenterVertically) {
        Icon(Icons.Outlined.Block, null, Modifier.size(18.dp), tint = p.blockedText)
        Text("You blocked $name.", Modifier.weight(1f).padding(horizontal = 10.dp), style = geist(14.sp, FontWeight.Medium), color = p.blockedText)
        TextButton(unblock, enabled = !busy) { Text("Unblock", color = p.blockedText, style = geist(14.sp, FontWeight.SemiBold)) }
    }
}

private fun callName(entry: CallLogEntry, contacts: List<Contact>): String {
    val key = entry.key ?: return entry.participants.orEmpty().size.let { if (it > 0) "$it people" else "Call" }
    return Conversations.contact(key, contacts)?.alias?.takeIf { it.isNotBlank() } ?: displayName(key)
}

/**
 * The Calls page (SDS §12.1): newest first, All · Missed, call back, delete, block, report, and the
 * calls the filtering settings silenced in a collapsed section at the bottom.
 */
@Composable
fun CallsPane(state: InboxState, store: InboxStore, modifier: Modifier, phone: Boolean, open: (String) -> Unit, callBack: (String, Boolean) -> Unit, back: (() -> Unit)? = null) {
    val p = Pp.colors
    val calls = state.calls
    var confirmClear by remember { mutableStateOf(false) }
    var filteredOpen by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(state.acting?.address) { if (!calls.loaded) store.loadCalls() }
    LaunchedEffect(calls.unseen, calls.loaded) { if (calls.loaded && calls.unseen > 0) store.markCallsSeen() }
    Column(modifier.testTag("callsPane")) {
        Row(Modifier.fillMaxWidth().height(if (phone) 56.dp else 60.dp).padding(start = if (back != null) 4.dp else 16.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            back?.let { ActionIcon("Back to conversations", Icons.AutoMirrored.Outlined.ArrowBack, tint = p.accent, action = it) }
            Text("Calls", Modifier.weight(1f), style = geist(if (phone) 22.sp else 17.sp, if (phone) FontWeight.Bold else FontWeight.SemiBold), color = p.ink)
            ActionIcon("Refresh calls", Icons.Outlined.Refresh, enabled = !calls.loading) { store.loadCalls() }
            ActionIcon("Clear all calls", Icons.Outlined.DeleteSweep, enabled = calls.entries.isNotEmpty()) { confirmClear = true }
        }
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp).testTag("callsFilter")) {
            listOf("all" to "All", "missed" to "Missed").forEachIndexed { i, (value, label) ->
                SegmentedButton(calls.filter == value, { if (calls.filter != value) store.callFilter(value) }, SegmentedButtonDefaults.itemShape(i, 2),
                    colors = SegmentedButtonDefaults.colors(activeContainerColor = p.chip, activeContentColor = p.ink, inactiveContainerColor = p.wash, inactiveContentColor = p.secondary,
                        activeBorderColor = p.strokeStrong, inactiveBorderColor = p.strokeStrong), icon = {}) { Text(label, style = geist(14.sp, FontWeight.Medium)) }
            }
        }
        if (calls.loading && !calls.loaded) LinearProgressIndicator(Modifier.fillMaxWidth().height(2.dp), color = p.accent, trackColor = p.rule)
        else Seam(Modifier.fillMaxWidth().height(1.dp))
        calls.error?.let { Text(it, Modifier.padding(16.dp), color = p.blockedText, style = geist(13.sp)) }
        LazyColumn(Modifier.weight(1f)) {
            if (calls.loaded && calls.shown.isEmpty() && calls.error == null) item {
                Text(if (calls.filter == "missed") "No missed calls." else "No calls yet.", Modifier.fillMaxWidth().padding(28.dp), color = p.muted, style = geist(14.sp))
            }
            items(calls.shown, key = { it.id }) { entry -> CallRow(entry, state.contacts, phone, open, callBack, store) }
            if (calls.next != null) item {
                TextButton({ store.loadCalls(more = true) }, Modifier.fillMaxWidth(), enabled = !calls.loading) { Text("Show older calls") }
            }
            if (state.has(Features.CALL_ADMISSION) || state.has(Features.MESSAGE_FILTERING)) item {
                Seam(Modifier.fillMaxWidth().height(1.dp))
                Row(Modifier.fillMaxWidth().clickable(onClickLabel = if (filteredOpen) "Hide filtered calls" else "Show filtered calls") {
                    filteredOpen = !filteredOpen; if (filteredOpen && calls.filtered == null) store.loadFilteredCalls()
                }.padding(horizontal = 16.dp, vertical = 14.dp).testTag("filteredCalls"), verticalAlignment = Alignment.CenterVertically) {
                    Icon(Icons.Outlined.FilterAlt, null, Modifier.size(18.dp), tint = p.secondary)
                    Text("Filtered calls" + (calls.filtered?.size?.takeIf { it > 0 }?.let { " ($it)" } ?: ""), Modifier.weight(1f).padding(start = 10.dp), style = geist(14.sp, FontWeight.Medium), color = p.secondary)
                    Icon(if (filteredOpen) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore, null, tint = p.muted)
                }
            }
            if (filteredOpen) {
                val rows = calls.filtered
                if (rows == null) item { Text("Loading…", Modifier.padding(16.dp), color = p.muted) }
                else if (rows.isEmpty()) item { Text("No filtered calls. Calls your settings silence are listed here.", Modifier.padding(16.dp), color = p.muted, style = geist(13.sp)) }
                else items(rows, key = { "f:" + it.id }) { entry -> CallRow(entry, state.contacts, phone, open, callBack, store) }
            }
        }
    }
    if (confirmClear) Confirm("Clear all calls?", "Your whole call history in this inbox is deleted. The other people keep theirs.", "Clear all",
        { confirmClear = false; store.clearCalls() }, { confirmClear = false })
}

@Composable
private fun CallRow(entry: CallLogEntry, contacts: List<Contact>, phone: Boolean, open: (String) -> Unit, callBack: (String, Boolean) -> Unit, store: InboxStore) {
    val p = Pp.colors
    val key = entry.key
    val name = callName(entry, contacts)
    var menu by remember { mutableStateOf(false) }
    val askBlock = LocalAskBlock.current
    val kind = if (entry.video) "video" else "voice"
    Row(Modifier.fillMaxWidth().clickable(enabled = key != null) { key?.let(open) }
        .semantics(mergeDescendants = true) { contentDescription = "$name, ${entry.label.lowercase()} $kind call, ${shortTime(entry.startedAt)}" }
        .padding(start = 16.dp, end = 4.dp, top = 10.dp, bottom = 10.dp).testTag("call:" + entry.id),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(11.dp)) {
        Avatar(key ?: "/guest", name, if (phone) 42.dp else 36.dp)
        Column(Modifier.weight(1f)) {
            Text(name, style = geistMono(14.sp, FontWeight.SemiBold), color = if (entry.missed) p.blockedText else p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(when { entry.missed -> Icons.AutoMirrored.Outlined.CallMissed; entry.incoming -> Icons.AutoMirrored.Outlined.CallReceived; else -> Icons.AutoMirrored.Outlined.CallMade },
                    null, Modifier.size(14.dp), tint = if (entry.missed) p.blockedText else p.muted)
                Text(entry.summary, Modifier.padding(start = 4.dp), style = geist(12.5.sp), color = if (entry.missed) p.blockedText else p.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        Text(shortTime(entry.startedAt), style = geist(11.5.sp), color = p.muted)
        if (key != null) ActionIcon("Call $name back, $kind", if (entry.video) Icons.Outlined.Videocam else Icons.Outlined.Call, tint = p.accent) { callBack(key, entry.video) }
        Box {
            ActionIcon("More actions for this call", Icons.Outlined.MoreVert) { menu = true }
            DropdownMenu(menu, { menu = false }, containerColor = p.ground) {
                DropdownMenuItem({ Text("Delete from history") }, { menu = false; store.deleteCall(entry) })
                if (key != null) DropdownMenuItem({ Text("Block", color = p.blockedText) }, { menu = false; askBlock(BlockTarget(key, name, callId = entry.id.takeIf { entry.incoming })) })
                if (entry.incoming && key != null) DropdownMenuItem({ Text("Report spam") }, { menu = false; store.reportCall(entry) })
            }
        }
    }
}

/** The bottom-of-list "Filtered (3)" row (SDS §13.4). */
@Composable
fun FilteredEntry(count: Int, open: () -> Unit) {
    val p = Pp.colors
    Row(Modifier.fillMaxWidth().clickable(onClickLabel = "Open filtered", onClick = open).padding(horizontal = 16.dp, vertical = 14.dp)
        .semantics { contentDescription = "Filtered, $count ${if (count == 1) "conversation" else "conversations"}" }.testTag("filteredEntry"),
        verticalAlignment = Alignment.CenterVertically) {
        Icon(Icons.Outlined.FilterAlt, null, Modifier.size(20.dp), tint = p.secondary)
        Text("Filtered ($count)", Modifier.weight(1f).padding(start = 12.dp), style = geist(14.5.sp, FontWeight.Medium), color = p.secondary)
        Icon(Icons.Outlined.ChevronRight, null, tint = p.muted)
    }
}

/** Senders the filtering settings kept out of the inbox, one row each. */
@Composable
fun FilteredPane(state: InboxState, modifier: Modifier, phone: Boolean, back: () -> Unit, open: (FilteredGroup) -> Unit) {
    val p = Pp.colors
    Column(modifier.testTag("filteredPane")) {
        Row(Modifier.fillMaxWidth().height(if (phone) 56.dp else 60.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            ActionIcon("Back to conversations", Icons.AutoMirrored.Outlined.ArrowBack, tint = p.accent, action = back)
            Text("Filtered", Modifier.weight(1f), style = geist(if (phone) 22.sp else 17.sp, FontWeight.SemiBold), color = p.ink)
        }
        Text("Messages your Privacy & filtering settings moved out of the inbox. They don’t notify you or count as unread.",
            Modifier.padding(horizontal = 16.dp, vertical = 4.dp), style = geist(12.5.sp), color = p.muted)
        Seam(Modifier.fillMaxWidth().height(1.dp))
        val groups = state.filteredGroups
        if (groups.isEmpty()) EmptyPane("Nothing filtered", "Messages that your settings filter appear here.", Modifier.weight(1f))
        else LazyColumn(Modifier.weight(1f)) {
            items(groups, key = { it.peer }) { group ->
                Row(Modifier.fillMaxWidth().clickable { open(group) }.padding(horizontal = 16.dp, vertical = 12.dp).testTag("filtered:" + group.peer),
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(11.dp)) {
                    Avatar(group.peer, displayName(group.peer), 40.dp)
                    Column(Modifier.weight(1f)) {
                        Row { Text(displayName(group.peer), Modifier.weight(1f), style = geistMono(14.sp, FontWeight.SemiBold), color = p.ink, maxLines = 1)
                            Text(shortTime(group.last), style = geist(11.5.sp), color = p.muted) }
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            Text(displayBody(group.messages.last().body).text.lineSequence().firstOrNull().orEmpty().ifBlank { "Attachment" }, Modifier.weight(1f),
                                style = geist(13.sp), color = p.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            Pill(group.messages.size.toString(), PillKind.HELD, dot = false)
                        }
                    }
                }
            }
        }
    }
}

/** One filtered sender: read what they sent, then Accept, Block (report on) or Report. */
@Composable
fun FilteredSheet(group: FilteredGroup, state: InboxState, store: InboxStore, dismiss: () -> Unit) {
    val p = Pp.colors
    val askBlock = LocalAskBlock.current
    var reported by remember(group.peer) { mutableStateOf(setOf<String>()) }
    PageDialog(displayName(group.peer), dismiss) {
        val last = group.messages.last()
        Text("From ${group.from}" + (last.standing?.let { " · $it" } ?: "") + ". Accepting adds them as a contact and moves these messages to your inbox.",
            style = geist(13.5.sp), color = p.secondary)
        group.messages.forEach { m ->
            Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(p.wash).padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(shortTime(m.at), Modifier.weight(1f), style = geist(12.sp), color = p.muted)
                    TextButton({ store.reportMessage(m.id) { reported = reported + m.id } }, enabled = m.id !in reported) { Text(if (m.id in reported) "Reported" else "Report spam") }
                }
                Text(displayBody(m.body).text, style = geist(14.sp), color = p.body)
            }
        }
        Button({ store.acceptFiltered(group, dismiss) }, Modifier.fillMaxWidth().testTag("acceptFiltered"), enabled = !state.actionBusy) { Text("Accept") }
        OutlinedButton({ dismiss(); askBlock(BlockTarget(group.peer, displayName(group.peer), messageId = last.id, report = true)) }, Modifier.fillMaxWidth()) {
            Text("Block", color = p.blockedText)
        }
    }
}
