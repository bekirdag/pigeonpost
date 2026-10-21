@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class, androidx.compose.foundation.ExperimentalFoundationApi::class, androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.*
import dev.pigeonpost.inbox.InboxViewModel
import dev.pigeonpost.inbox.calls.CallHost
import kotlinx.serialization.json.JsonPrimitive
import java.text.DateFormat
import java.util.Date

private fun aliases(state: InboxState): Map<String, String> = state.contacts.filter { !it.wildcard && !it.alias.isNullOrBlank() }.associate { it.peer to it.alias!! }

/** "Jane" for a member, "You" for this mailbox; contact names win, as everywhere else. */
@Composable
private fun rememberNamer(groups: GroupsState, inbox: InboxState): (String?) -> String {
    val names = remember(inbox.contacts) { aliases(inbox) }
    return remember(groups.handles, groups.mailbox, names) {
        { address: String? ->
            when {
                address == null -> "Someone"
                address == groups.mailbox -> "You"
                else -> { val handle = groups.handles[address]; names[handle ?: address] ?: names[address] ?: displayName(handle ?: address) }
            }
        }
    }
}

private fun previewOf(history: List<HistoryItem>, me: String?, name: (String?) -> String): String? {
    for (x in history.asReversed()) {
        if (x.kind != "message" || x.replacedBy != null) continue
        if (x.deleted) return "Message deleted"
        if (x.undecryptable) return "Encrypted message"
        val who = if (x.sender == me) "You" else name(x.sender).split(' ', '/').firstOrNull { it.isNotBlank() } ?: "Someone"
        return "$who: " + (x.text?.takeIf { it.isNotBlank() }?.lineSequence()?.first() ?: x.files.firstOrNull()?.let { "📎 " + it.name } ?: "")
    }
    return null
}

/** Groups sit beside direct conversations: pending invitations first, then the groups themselves. */
fun LazyListScope.groupRows(groups: GroupsState, inbox: InboxState, store: GroupStore, phone: Boolean, invitesOpen: Boolean, toggleInvites: () -> Unit, open: (String) -> Unit) {
    if (!groups.enabled) return
    val filter = inbox.filter.trim()
    val invitations = groups.invitations
    if (invitations.isNotEmpty() && filter.isEmpty() && !inbox.viewingArchive) {
        item(key = "group-invitations") {
            val p = Pp.colors
            Row(Modifier.fillMaxWidth().clickable(onClickLabel = if (invitesOpen) "Hide invitations" else "Show invitations", onClick = toggleInvites)
                .padding(horizontal = 16.dp, vertical = 12.dp).testTag("groupInvitations"), verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.GroupAdd, null, Modifier.size(20.dp), tint = p.accent)
                Text("Invitations (${invitations.size})", Modifier.weight(1f).padding(start = 12.dp), style = geist(14.5.sp, FontWeight.SemiBold), color = p.ink)
                Icon(if (invitesOpen) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore, null, tint = p.muted)
            }
        }
        if (invitesOpen) items(invitations, key = { "invite:" + it.id }) { g -> InvitationCard(g, groups, inbox, store) }
    }
    val rows = groups.joined.filter { !inbox.viewingArchive && (filter.isEmpty() || it.name.contains(filter, true)) }
    items(rows, key = { "group:" + it.id }) { g -> GroupRow(g, groups, inbox, phone) { open(g.id) } }
}

@Composable
private fun InvitationCard(g: Group, groups: GroupsState, inbox: InboxState, store: GroupStore) {
    val p = Pp.colors
    val name = rememberNamer(groups, inbox)
    Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp).clip(RoundedCornerShape(12.dp)).background(p.wash)
        .border(1.dp, p.stroke, RoundedCornerShape(12.dp)).padding(10.dp).testTag("invitation:" + g.id), verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        GroupAvatar(g.id, g.name, 38.dp)
        Column(Modifier.weight(1f)) {
            Text(g.name, style = geist(14.5.sp, FontWeight.SemiBold), color = p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(listOfNotNull(g.my?.invitedBy?.let { "From " + name(it) }, "someone you don’t know".takeIf { g.my?.filtered == true }, "${g.memberCount} ${if (g.memberCount == 1) "member" else "members"}").joinToString(" · "),
                style = geist(12.sp), color = p.secondary, maxLines = 2)
        }
        TextButton({ store.answerInvitation(g, false) }, enabled = !groups.busy) { Text("Decline") }
        Button({ store.answerInvitation(g, true) }, enabled = !groups.busy, contentPadding = PaddingValues(horizontal = 14.dp)) { Text("Join") }
    }
}

@Composable
private fun GroupRow(g: Group, groups: GroupsState, inbox: InboxState, phone: Boolean, select: () -> Unit) {
    val p = Pp.colors
    val name = rememberNamer(groups, inbox)
    val selected = g.id == groups.open
    val preview = when {
        g.state == "keying" -> "Waiting for a member’s device to add you…"
        else -> previewOf(groups.history[g.id].orEmpty(), groups.mailbox, name) ?: if (g.activationState == "provisioning") "Setting up encryption…" else "No messages yet"
    }
    Column(Modifier.fillMaxWidth().then(if (phone) Modifier else Modifier.padding(horizontal = 8.dp, vertical = 1.dp))
        .clip(if (phone) RoundedCornerShape(0.dp) else RoundedCornerShape(10.dp)).background(if (selected) p.selected else Color.Transparent)
        .clickable(onClick = select).testTag("group:" + g.id)) {
        Row(Modifier.padding(horizontal = if (phone) 16.dp else 10.dp, vertical = if (phone) 13.dp else 10.dp), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(11.dp)) {
            GroupAvatar(g.id, g.name, if (phone) 44.dp else 38.dp)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(g.name, Modifier.weight(1f), style = geist(if (phone) 15.sp else 13.5.sp, FontWeight.SemiBold), color = p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    g.lastAt?.let { Text(shortTime(it), style = geist(11.5.sp), color = p.muted) }
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(preview, Modifier.weight(1f), style = geist(if (phone) 14.sp else 13.sp), maxLines = 1, overflow = TextOverflow.Ellipsis, color = p.secondary)
                    if (g.ephemeralSecs != null) Icon(Icons.Outlined.Timer, "Disappearing messages: ${timerLabel(g.ephemeralSecs)}", Modifier.size(15.dp), tint = p.muted)
                    if (g.muted(System.currentTimeMillis() / 1000)) Icon(Icons.Outlined.NotificationsOff, "Muted", Modifier.size(15.dp), tint = p.muted)
                    val unread = g.my?.unread ?: 0
                    if (unread > 0 && !selected) UnreadBadge(unread)
                }
            }
        }
        if (phone) Seam(Modifier.padding(start = 71.dp).fillMaxWidth().height(1.dp))
    }
}

/** New group: who, name, and settings with sensible defaults (SDS §4.2). */
@Composable
fun NewGroupDialog(inbox: InboxState, groups: GroupsState, store: GroupStore, dismiss: () -> Unit) {
    val p = Pp.colors
    var who by rememberSaveable { mutableStateOf("") }
    var chips by rememberSaveable { mutableStateOf(listOf<String>()) }
    var name by rememberSaveable { mutableStateOf("") }
    var description by rememberSaveable { mutableStateOf("") }
    var join by rememberSaveable { mutableStateOf("private") }
    var post by rememberSaveable { mutableStateOf("everyone") }
    var timer by rememberSaveable { mutableStateOf<Long?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    fun addChip(): Boolean {
        val v = conversationAddressInput(who.trim().trimEnd(','))
        if (v.length <= 1) return true
        if (!validAddress(v)) { error = "That does not look like a Pigeonpost address."; return false }
        if (v !in chips && chips.size < 200) chips = chips + v
        who = ""; error = null
        return true
    }
    PageDialog("New group", dismiss) {
        Text("Sending as ${inbox.acting?.name ?: ""}", style = geist(13.sp), color = p.secondary)
        Text("Who", style = geist(14.5.sp, FontWeight.Medium), color = p.ink)
        if (chips.isNotEmpty()) FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            chips.forEach { chip -> InputChip(false, { chips = chips - chip }, { Text(chip, style = geistMono(12.5.sp)) },
                trailingIcon = { Icon(Icons.Outlined.Close, "Remove $chip", Modifier.size(16.dp)) }) }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(who, { who = it.take(300); if (it.endsWith(',') || it.endsWith(' ')) addChip() }, Modifier.weight(1f).testTag("groupWho"), singleLine = true,
                label = { Text("Add a person") }, placeholder = { Text("/alp/main or /k/…") })
            TextButton({ addChip() }) { Text("Add") }
        }
        val known = inbox.contacts.filter { !it.wildcard && it.admission != "block" && it.peer !in chips }.take(8)
        if (known.isNotEmpty()) Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            known.forEach { c -> SuggestionChip({ chips = chips + c.peer }, { Text(c.alias ?: displayName(c.peer)) }) }
        }
        OutlinedTextField(name, { name = it.take(80) }, Modifier.fillMaxWidth().testTag("groupName"), singleLine = true, label = { Text("Name") }, placeholder = { Text("Garden club") })
        OutlinedTextField(description, { description = it.take(500) }, Modifier.fillMaxWidth(), label = { Text("Description (optional)") }, minLines = 2)
        Text("Who can join", style = geist(14.5.sp, FontWeight.Medium), color = p.ink)
        Choice(listOf("private" to "Private — invite only", "public" to "Public — anyone with the link"), join) { join = it }
        Text("Who can post", style = geist(14.5.sp, FontWeight.Medium), color = p.ink)
        Choice(listOf("everyone" to "Everyone", "admins" to "Only admins"), post) { post = it }
        if (groups.disappearing) TimerPicker("Disappearing messages", timer) { timer = it }
        Text("Group messages are end-to-end encrypted. People you add see messages from when they join, not before.", style = geist(12.5.sp), color = p.muted)
        error?.let { Text(it, color = p.blockedText) }
        Button({
            if (!addChip()) return@Button
            if (name.isBlank()) { error = "Give the group a name."; return@Button }
            store.create(name, description, chips, join, post, timer) { id -> if (id != null) dismiss() }
        }, Modifier.fillMaxWidth().testTag("createGroup"), enabled = !groups.busy) { Text(if (groups.busy) "Creating…" else "Create group") }
    }
}

@Composable
private fun Choice(options: List<Pair<String, String>>, value: String, change: (String) -> Unit) {
    Column {
        options.forEach { (key, label) ->
            Row(Modifier.fillMaxWidth().selectable(value == key, role = Role.RadioButton) { change(key) }.padding(vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                RadioButton(value == key, null); Text(label, Modifier.padding(start = 6.dp), style = geist(14.sp))
            }
        }
    }
}

/** Off · 5 minutes · 1 hour · 8 hours · 1 day · 1 week · 4 weeks. */
@Composable
fun TimerPicker(label: String, value: Long?, enabled: Boolean = true, change: (Long?) -> Unit) {
    var open by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(open, { if (enabled) open = it }) {
        OutlinedTextField(timerLabel(value), {}, Modifier.fillMaxWidth().menuAnchor(MenuAnchorType.PrimaryNotEditable).testTag("timerPicker"), readOnly = true, enabled = enabled,
            label = { Text(label) }, trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(open) })
        ExposedDropdownMenu(open, { open = false }) {
            DISAPPEARING_TIMERS.forEach { t -> DropdownMenuItem({ Text(timerLabel(t)) }, { open = false; if (t != value) change(t) }) }
        }
    }
}

/**
 * One group: its bar (name, members, calls, info), the end-to-end note, the history with system
 * lines for membership and device changes, and the composer — or the announcement bar.
 */
@Composable
fun GroupPane(model: InboxViewModel, groups: GroupsState, inbox: InboxState, store: GroupStore, modifier: Modifier, wide: Boolean, callMode: CallHost.Mode,
    info: () -> Unit, chooseFile: () -> Unit, choosePhoto: () -> Unit, openFile: (Group, GroupFile, String) -> Unit, showSidebar: (() -> Unit)? = null) {
    val p = Pp.colors
    val g = groups.current ?: return
    val name = rememberNamer(groups, inbox)
    val now = remember(groups.history) { System.currentTimeMillis() / 1000 }
    val history = groups.history[g.id].orEmpty().filter { (it.expiresAt ?: Long.MAX_VALUE) > now }
    val staged by model.groupStaged.collectAsState()
    val files = staged[g.id].orEmpty()
    var draft by rememberSaveable(g.id) { mutableStateOf("") }
    var addMenu by remember { mutableStateOf(false) }
    val calling = callMode != CallHost.Mode.HIDDEN
    val active = g.active
    val inGroup = g.id in groups.inGroup
    val blocked = remember(inbox.contacts) { inbox.contacts.filter { it.admission == "block" } }
    Column(modifier.fillMaxHeight().background(p.wash).testTag("groupPane")) {
        Row(Modifier.fillMaxWidth().heightIn(min = 60.dp).padding(start = if (wide && showSidebar == null) 14.dp else 2.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            if (!wide) ActionIcon("Back to conversations", Icons.AutoMirrored.Outlined.ArrowBack, tint = p.accent) { store.close() }
            showSidebar?.let { show -> IconButton(show, Modifier.testTag("showSidebar")) { Icon(Icons.Outlined.ViewSidebar, "Show conversations", tint = p.secondary) } }
            Row(Modifier.weight(1f).clip(RoundedCornerShape(10.dp)).clickable(onClickLabel = "Group info", onClick = info).padding(horizontal = 6.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                GroupAvatar(g.id, g.name, if (wide) 30.dp else 34.dp)
                Column(Modifier.weight(1f)) {
                    Text(g.name, style = geist(15.sp, FontWeight.SemiBold), color = p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.testTag("groupTitle"))
                    Text(listOfNotNull("${g.memberCount} ${if (g.memberCount == 1) "member" else "members"}", g.ephemeralSecs?.let { "⏱ " + timerLabel(it) }).joinToString(" · "),
                        style = geist(12.sp), color = p.muted, maxLines = 1)
                }
            }
            if (active && model.calls != null) {
                ActionIcon("Group voice call", Icons.Outlined.Call, enabled = !calling, tint = p.accent) { model.calls.callGroup(g.id, g.name, video = false, join = false) }
                ActionIcon("Group video call", Icons.Outlined.Videocam, enabled = !calling, tint = p.accent) { model.calls.callGroup(g.id, g.name, video = true, join = false) }
            }
            ActionIcon("Group info", Icons.Outlined.Info, action = info)
        }
        Seam(Modifier.fillMaxWidth().height(1.dp))
        if (active && groups.call?.active == true && model.calls != null) {
            Row(Modifier.fillMaxWidth().background(p.autoFill).clickable(enabled = !calling) { model.calls.callGroup(g.id, g.name, video = groups.call?.kind == "video", join = true) }
                .padding(horizontal = 16.dp, vertical = 10.dp).testTag("joinGroupCall"), verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.Call, null, Modifier.size(18.dp), tint = p.autoText)
                Text("Call in progress — Join", Modifier.padding(start = 10.dp), style = geist(14.sp, FontWeight.SemiBold), color = p.autoText)
            }
        }
        val status = when {
            g.state == "keying" -> "You have joined. Waiting for a member’s device to add you to the encryption — this happens automatically when one is online."
            g.activationState == "provisioning" -> "Setting up encryption…"
            active && !inGroup -> "This device is being added to the group’s encryption. New messages will appear once it is."
            else -> null
        }
        val list = rememberLazyListState()
        val shown = history.filter { it.kind != "message" || it.replacedBy == null }
        LaunchedEffect(shown.size) { if (shown.isNotEmpty()) list.animateScrollToItem(0) }
        LazyColumn(Modifier.weight(1f).fillMaxWidth().testTag("groupHistory"), state = list, reverseLayout = true,
            contentPadding = PaddingValues(horizontal = 14.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            val rows = shown.asReversed()
            items(rows.size, key = { i -> rows[i].let { "${it.kind}:${it.seq}:${it.after}:${it.cmid}" } }) { i ->
                val x = rows[i]
                val older = rows.getOrNull(i + 1)
                val day = { t: Long -> java.time.Instant.ofEpochSecond(t).atZone(java.time.ZoneId.systemDefault()).toLocalDate() }
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    if (x.createdAt > 0 && (older == null || older.createdAt <= 0 || day(older.createdAt) != day(x.createdAt)))
                        SystemLine(DateFormat.getDateInstance(DateFormat.FULL).format(Date(x.createdAt * 1000)))
                    when (x.kind) {
                        "notice" -> groupNoticeText(x, name)?.let { SystemLine(it) }
                        "commit" -> commitLines(history, x, groups.mailbox.orEmpty(), groups.deviceId, name).forEach { line ->
                            SystemLine(line.text + (line.fingerprint?.let { " · " + fingerprintGroups(it) } ?: ""), security = line.security)
                        }
                        "warning" -> SystemLine(groupWarningText(x.warning), warning = true)
                        "message" -> {
                            val grouped = older?.kind == "message" && older.sender == x.sender && older.replacedBy == null
                            GroupBubble(x, g, groups, name(x.sender), showSender = !grouped, blocked = Conversations.contact(name(x.sender), blocked) != null || blocked.any { it.peer == x.sender || it.peer == groups.handles[x.sender] },
                                store = store, openFile = { f, action -> openFile(g, f, action) })
                        }
                    }
                }
            }
            item(key = "e2ee-note") {
                Column(Modifier.fillMaxWidth().padding(vertical = 10.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(Icons.Outlined.Lock, null, Modifier.size(14.dp), tint = p.muted)
                        Text(" Messages in this group are end-to-end encrypted. The postbox cannot read them.", style = geist(12.sp), color = p.muted, textAlign = TextAlign.Center)
                    }
                    if (history.none { it.kind == "message" }) Text(g.ephemeralSecs?.let { "Messages disappear after ${timerLabel(it)}." } ?: "No messages yet. Say hello.",
                        style = geist(13.sp), color = p.secondary)
                }
            }
        }
        status?.let { Text(it, Modifier.fillMaxWidth().background(p.heldPillFill).padding(horizontal = 16.dp, vertical = 10.dp).testTag("groupStatus"), style = geist(13.sp), color = p.heldText) }
        if (!groups.persistent) Text("This device cannot keep group keys between launches.", Modifier.padding(horizontal = 16.dp, vertical = 6.dp), style = geist(12.sp), color = p.heldText)
        when {
            !active -> {}
            !g.canPost -> Text(if (g.my?.role == "restricted") "You can read this group but not send messages." else "Only admins can send messages",
                Modifier.fillMaxWidth().background(p.tile).padding(16.dp).testTag("announcementBar"), style = geist(14.sp, FontWeight.Medium), color = p.secondary, textAlign = TextAlign.Center)
            inGroup -> Column(Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, top = 6.dp, bottom = 10.dp)
                .clip(RoundedCornerShape(16.dp)).background(p.composer).border(1.dp, p.stroke, RoundedCornerShape(16.dp))) {
                if (files.isNotEmpty()) Row(Modifier.horizontalScroll(rememberScrollState()).padding(start = 10.dp, end = 10.dp, top = 10.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    files.forEach { f -> InputChip(false, { model.unstageGroupFile(g.id, f.id) }, { Text(f.name, maxLines = 1, style = geist(13.sp)) }, enabled = !groups.sending,
                        trailingIcon = { Icon(Icons.Outlined.Close, "Remove ${f.name}", Modifier.size(16.dp)) }, border = BorderStroke(1.dp, p.stroke)) }
                }
                Row(Modifier.fillMaxWidth().padding(start = 2.dp, end = 8.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.Bottom) {
                    Box(Modifier.padding(bottom = 4.dp)) {
                        ActionIcon("Add attachment", Icons.Outlined.AttachFile) { addMenu = true }
                        DropdownMenu(addMenu, { addMenu = false }, containerColor = p.ground) {
                            DropdownMenuItem({ Text("Choose document") }, { addMenu = false; model.chooseGroupAttachments(g.id); chooseFile() })
                            DropdownMenuItem({ Text("Choose photo") }, { addMenu = false; model.chooseGroupAttachments(g.id); choosePhoto() })
                        }
                    }
                    MessageEditor(draft, { draft = it }, !groups.sending, Modifier.weight(1f).testTag("groupComposer"), { _, _ -> }, remember { androidx.compose.ui.focus.FocusRequester() }, placeholder = "Write to the group")
                    val canSend = !groups.sending && (draft.isNotBlank() || files.isNotEmpty())
                    IconButton({
                        val text = draft
                        draft = ""
                        model.sendGroup(g.id, text) { ok -> if (!ok && draft.isEmpty()) draft = text }
                    }, enabled = canSend, modifier = Modifier.padding(bottom = 8.dp).size(40.dp).clip(RoundedCornerShape(11.dp)).background(if (canSend) p.accent else p.chip)) {
                        if (groups.sending) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = p.onAccent)
                        else Icon(Icons.Outlined.ArrowUpward, "Send to the group", Modifier.size(20.dp), tint = if (canSend) p.onAccent else p.muted)
                    }
                }
            }
        }
    }
}

fun fingerprintGroups(hex: String): String = hex.chunked(2).take(10).joinToString("") { it }.uppercase().chunked(4).joinToString(" ")

@Composable
private fun SystemLine(text: String, security: Boolean = false, warning: Boolean = false) {
    val p = Pp.colors
    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
        Row(Modifier.clip(RoundedCornerShape(10.dp)).background(if (warning) p.blockedFill else if (security) p.heldPillFill else p.tile).padding(horizontal = 10.dp, vertical = 5.dp)
            .testTag(if (warning) "groupWarning" else if (security) "deviceChange" else "systemLine"), verticalAlignment = Alignment.CenterVertically) {
            if (security || warning) Icon(if (warning) Icons.Outlined.GppMaybe else Icons.Outlined.Devices, null, Modifier.size(14.dp).padding(end = 2.dp), tint = if (warning) p.blockedText else p.heldText)
            Text(text, style = geist(12.sp, if (security || warning) FontWeight.Medium else FontWeight.Normal), color = if (warning) p.blockedText else if (security) p.heldText else p.secondary, textAlign = TextAlign.Center)
        }
    }
}

@Suppress("DEPRECATION")
@Composable
private fun GroupBubble(x: HistoryItem, g: Group, groups: GroupsState, sender: String, showSender: Boolean, blocked: Boolean, store: GroupStore, openFile: (GroupFile, String) -> Unit) {
    val p = Pp.colors
    val mine = x.sender == groups.mailbox
    var menu by remember { mutableStateOf(false) }
    var confirm by remember { mutableStateOf(false) }
    var reveal by remember { mutableStateOf(false) }
    val clipboard = LocalClipboardManager.current
    val askBlock = LocalAskBlock.current
    val shape = RoundedCornerShape(14.dp)
    Row(Modifier.fillMaxWidth(), horizontalArrangement = if (mine) Arrangement.End else Arrangement.Start, verticalAlignment = Alignment.Top) {
        if (!mine) {
            if (showSender) Avatar(x.sender, sender, 30.dp) else Spacer(Modifier.width(30.dp))
            Spacer(Modifier.width(8.dp))
        } else Spacer(Modifier.width(40.dp))
        Column(Modifier.weight(1f, fill = false).widthIn(max = 560.dp).clip(shape).background(if (mine) p.mineFill else p.ground)
            .border(1.dp, if (mine) p.mineStroke else p.stroke, shape).combinedClickable(onClick = {}, onLongClick = { menu = true }, onLongClickLabel = "Message actions")
            .padding(start = 14.dp, end = 6.dp, top = 8.dp, bottom = 4.dp).testTag("groupMessage:" + (x.seq ?: x.cmid))) {
            if (!mine && showSender) Text(sender, style = geist(12.5.sp, FontWeight.SemiBold), color = p.accent)
            when {
                x.deleted -> Text("This message was deleted", style = geist(14.sp).copy(fontStyle = FontStyle.Italic), color = p.muted)
                x.undecryptable -> Text("Encrypted message — not readable on this device", style = geist(14.sp).copy(fontStyle = FontStyle.Italic), color = p.muted)
                blocked && !reveal -> TextButton({ reveal = true }, contentPadding = PaddingValues(0.dp)) { Text("Message from someone you blocked · Show", style = geist(13.5.sp), color = p.muted) }
                else -> {
                    if (!x.text.isNullOrEmpty()) SelectionContainer { Text(x.text!!, style = geist(15.sp), color = if (mine) p.mineText else p.body, modifier = Modifier.padding(end = 8.dp)) }
                    x.files.forEach { f ->
                        Row(Modifier.padding(top = 6.dp, end = 8.dp).clip(RoundedCornerShape(10.dp)).background(if (mine) p.mineRule else p.field)
                            .clickable(onClickLabel = "Open ${f.name}") { openFile(f, "view") }.padding(start = 10.dp).testTag("groupFile:" + f.name),
                            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Icon(Icons.Outlined.Description, null, Modifier.size(17.dp), tint = p.secondary)
                            Column(Modifier.weight(1f, fill = false).padding(vertical = 8.dp)) {
                                Text(f.name, style = geist(13.5.sp, FontWeight.Medium), color = p.ink, maxLines = 2, overflow = TextOverflow.Ellipsis)
                                Text(bytes(f.size), style = geist(12.sp), color = p.muted)
                            }
                            IconButton({ openFile(f, "download") }, Modifier.size(40.dp)) { Icon(Icons.Outlined.Download, "Download ${f.name}", Modifier.size(18.dp), tint = p.secondary) }
                        }
                    }
                }
            }
            Row(Modifier.align(Alignment.End), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                if (x.expiresAt != null) Icon(Icons.Outlined.Timer, "Disappears " + DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(x.expiresAt!! * 1000)),
                    Modifier.size(13.dp), tint = if (mine) p.mineMeta else p.muted)
                Text((if (x.pending) "Sending…" else shortTime(x.createdAt)) + if (x.editOf != null) " · edited" else "", style = geist(11.5.sp), color = if (mine) p.mineMeta else p.muted)
                Box {
                    IconButton({ menu = true }, Modifier.size(30.dp)) { Icon(Icons.Outlined.MoreHoriz, "Message actions", Modifier.size(16.dp), tint = if (mine) p.mineMeta else p.muted) }
                    DropdownMenu(menu, { menu = false }, containerColor = p.ground) {
                        if (!x.text.isNullOrEmpty()) DropdownMenuItem({ Text("Copy text") }, { menu = false; clipboard.setText(AnnotatedString(x.text!!)) })
                        if (!x.deleted && x.seq != null && !x.pending && (mine || g.admin)) DropdownMenuItem({ Text("Delete for everyone") }, { menu = false; confirm = true })
                        if (!mine && x.sender != null) DropdownMenuItem({ Text("Block sender", color = p.blockedText) }, { menu = false; askBlock(BlockTarget(x.sender!!, sender)) })
                    }
                }
            }
        }
        if (!mine) Spacer(Modifier.width(32.dp))
    }
    if (confirm) Confirm("Delete this message for everyone?", "It is removed from the postbox. Copies people already saw on their screens stay where they are.", "Delete",
        { confirm = false; x.seq?.let { store.deleteMessage(g.id, it) } }, { confirm = false })
}

private val ROLE_LABEL = mapOf("owner" to "Owner", "admin" to "Admin", "member" to "Member", "restricted" to "Restricted")

/** Group info: members and roles, adding people, the invite link, settings, timer, notifications, encryption, leaving. */
@Composable
fun GroupInfoDialog(groups: GroupsState, inbox: InboxState, store: GroupStore, dismiss: () -> Unit) {
    val p = Pp.colors
    val g = groups.current ?: run { LaunchedEffect(Unit) { dismiss() }; return }
    val name = rememberNamer(groups, inbox)
    val members = groups.members[g.id].orEmpty()
    var confirm by remember { mutableStateOf<Pair<String, () -> Unit>?>(null) }
    var link by remember(g.id) { mutableStateOf<String?>(null) }
    var add by rememberSaveable(g.id) { mutableStateOf("") }
    var editName by rememberSaveable(g.id) { mutableStateOf(g.name) }
    var editDescription by rememberSaveable(g.id) { mutableStateOf(g.description.orEmpty()) }
    var deviceFor by remember { mutableStateOf<String?>(null) }
    val askBlock = LocalAskBlock.current
    val clipboard = @Suppress("DEPRECATION") LocalClipboardManager.current
    LaunchedEffect(g.id) { store.loadMembers(g.id) }
    PageDialog("Group info", dismiss) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            GroupAvatar(g.id, g.name, 52.dp)
            Column { Text(g.name, style = geist(19.sp, FontWeight.SemiBold), color = p.ink); g.description?.let { Text(it, style = geist(13.sp), color = p.secondary) } }
        }
        if (g.admin) Section("Name and description") {
            OutlinedTextField(editName, { editName = it.take(80) }, Modifier.fillMaxWidth(), singleLine = true, label = { Text("Name") })
            OutlinedTextField(editDescription, { editDescription = it.take(500) }, Modifier.fillMaxWidth(), label = { Text("Description") })
            OutlinedButton({ store.patch(g.id, mapOf("name" to JsonPrimitive(editName.trim()), "description" to (editDescription.trim().takeIf { it.isNotEmpty() }?.let { JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull))) },
                enabled = editName.isNotBlank() && !groups.busy) { Text("Save name") }
        }
        Section("Members (${members.count { it.state == "active" }})") {
            members.filter { it.state == "active" || it.state == "keying" || (g.admin && it.state == "invited") }.forEach { m ->
                MemberRow(g, m, groups, name(m.address), store, devices = { deviceFor = if (deviceFor == m.address) null else m.address.also(store::loadDevices) },
                    showDevices = deviceFor == m.address, confirm = { text, action -> confirm = text to action }, block = { askBlock(BlockTarget(m.handle ?: m.address, name(m.address))) })
            }
            if (g.mayInvite) Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(add, { add = it.take(300) }, Modifier.weight(1f).testTag("groupAdd"), singleLine = true, label = { Text("Add people") }, placeholder = { Text("/alp/main") })
                TextButton({ store.invite(g.id, add); add = "" }, enabled = add.isNotBlank() && !groups.busy) { Text("Add") }
            }
            if (g.admin && members.any { it.state == "invited" }) Text("Invited people appear here until they answer. Revoke takes the invitation back.", style = geist(12.sp), color = p.muted)
        }
        if (g.admin && g.joinPolicy == "public") Section("Invite link") {
            Text(if (g.hasLink) "Anyone with the current link can join. Resetting makes the old link stop working." else "Create a link anyone can use to join this public group.",
                style = geist(13.sp), color = p.secondary)
            link?.let { url ->
                SelectionContainer { Text(url, style = geistMono(12.5.sp), color = p.ink, modifier = Modifier.testTag("groupLink")) }
                Row { TextButton({ clipboard.setText(AnnotatedString(url)) }) { Text("Copy link") } }
                Text("Shown once. Anyone with this link can join while it is on.", style = geist(12.sp), color = p.muted)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton({ store.link(g.id, true) { link = it } }, enabled = !groups.busy) { Text(if (g.hasLink) "Reset link" else "Create link") }
                if (g.hasLink) TextButton({ store.link(g.id, false) { link = null } }, enabled = !groups.busy) { Text("Turn off link") }
            }
        }
        if (g.admin) Section("Settings") {
            Text("Who can join", style = geist(13.5.sp, FontWeight.Medium), color = p.ink)
            Choice(listOf("private" to "Private — invite only", "public" to "Public — anyone with the link"), g.joinPolicy) { store.patch(g.id, mapOf("join_policy" to JsonPrimitive(it))) }
            Text("Who can post", style = geist(13.5.sp, FontWeight.Medium), color = p.ink)
            Choice(listOf("everyone" to "Everyone", "admins" to "Only admins"), g.postPolicy) { store.patch(g.id, mapOf("post_policy" to JsonPrimitive(it))) }
            SwitchRow("Members can add people", g.membersCanInvite) { store.patch(g.id, mapOf("members_can_invite" to JsonPrimitive(it))) }
            if (groups.disappearing) SwitchRow("Members can change the timer", g.membersCanChangeTimer) { store.patch(g.id, mapOf("members_can_change_timer" to JsonPrimitive(it))) }
        }
        if (groups.disappearing) Section("Disappearing messages") {
            TimerPicker("New messages disappear after", g.ephemeralSecs, enabled = g.mayChangeTimer && !groups.busy) { store.setTimer(g.id, it) }
            Text("The clock starts when a message is sent. $DISAPPEARING_LIMITS", style = geist(12.sp), color = p.muted)
        }
        Section("Notifications") {
            Choice(listOf("all" to "All messages", "mentions" to "Only mentions", "none" to "Nothing"), g.my?.notify ?: "all") { store.setNotifications(g.id, it, g.muted(System.currentTimeMillis() / 1000)) }
            SwitchRow("Mute", g.muted(System.currentTimeMillis() / 1000)) { store.setNotifications(g.id, g.my?.notify ?: "all", it) }
            Text("Notifications say only “New message”: the postbox cannot read group messages.", style = geist(12.sp), color = p.muted)
        }
        Section("Encryption") {
            Text("Compare this safety number with another member in person or on a call. If it matches, you both see the same devices in this group.", style = geist(13.sp), color = p.secondary)
            Text(groups.safety[g.id]?.filter(Char::isDigit)?.chunked(5)?.joinToString(" ") ?: "Not available on this device yet.",
                style = geistMono(15.sp, FontWeight.Medium), color = p.ink, modifier = Modifier.testTag("safetyNumber"))
            Text("This device: " + (groups.fingerprint?.filter(Char::isDigit)?.chunked(5)?.joinToString(" ") ?: "—"), style = geistMono(12.5.sp), color = p.secondary)
            Text("Hosted mailboxes’ keys are held by the postbox, so it could add a device to one. Every new device is shown here and in the conversation; check fingerprints with people you trust.",
                style = geist(12.sp), color = p.heldText)
        }
        Section("Membership") {
            if (g.owner) {
                val admins = members.filter { it.role == "admin" && it.state == "active" }
                if (admins.isEmpty()) Text("To leave, first make another member an admin and transfer ownership to them.", style = geist(12.5.sp), color = p.muted)
                else admins.forEach { a -> OutlinedButton({ confirm = "Transfer ownership to ${name(a.address)}? You stay an admin." to { store.transfer(g.id, a.address) } }) { Text("Make ${name(a.address)} the owner") } }
            }
            TextButton({ confirm = "Leave this group? You stop receiving its messages. To come back you need a new invitation." to { store.leave(g.id); dismiss() } }, Modifier.testTag("leaveGroup")) {
                Text("Leave group", color = p.blockedText)
            }
            if (g.owner) TextButton({ confirm = "Delete this group for everyone? Members see that it was deleted and its messages are removed from the postbox." to { store.delete(g.id); dismiss() } }) {
                Text("Delete group", color = p.blockedText)
            }
        }
    }
    confirm?.let { (text, action) -> Confirm("Are you sure?", text, "Confirm", { confirm = null; action() }, { confirm = null }) }
}

@Composable
private fun Section(title: String, content: @Composable ColumnScope.() -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        HorizontalDivider()
        Text(title, style = geist(15.sp, FontWeight.SemiBold), color = Pp.colors.ink)
        content()
    }
}

@Composable
private fun SwitchRow(title: String, checked: Boolean, change: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth().toggleable(checked, role = Role.Switch, onValueChange = change), verticalAlignment = Alignment.CenterVertically) {
        Text(title, Modifier.weight(1f), style = geist(14.sp)); Switch(checked, null)
    }
}

@Composable
private fun MemberRow(g: Group, m: GroupMember, groups: GroupsState, name: String, store: GroupStore, devices: () -> Unit, showDevices: Boolean,
    confirm: (String, () -> Unit) -> Unit, block: () -> Unit) {
    val p = Pp.colors
    val self = m.address == groups.mailbox
    var menu by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().testTag("member:" + m.address)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Avatar(m.handle ?: m.address, name, 34.dp)
            Column(Modifier.weight(1f)) {
                Text(name + if (self) " (you)" else "", style = geist(14.sp, FontWeight.Medium), color = p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(m.handle ?: m.address, style = geistMono(11.5.sp), color = p.muted, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
            }
            Pill(when (m.state) { "active" -> ROLE_LABEL[m.role] ?: m.role; "invited" -> "Invited"; else -> "Joining" }, if (m.role == "owner" || m.role == "admin") PillKind.AUTO else PillKind.NEUTRAL, dot = false)
            Box {
                ActionIcon("Actions for $name", Icons.Outlined.MoreVert) { menu = true }
                DropdownMenu(menu, { menu = false }, containerColor = p.ground) {
                    @Composable fun act(label: String, role: String?, state: String?) {
                        DropdownMenuItem({ Text(label) }, { menu = false
                            if (state == "removed" || state == "banned") confirm("$name loses access at once. Messages they already saw stay on their devices.") { store.changeMember(g.id, m, role, state) }
                            else store.changeMember(g.id, m, role, state) })
                    }
                    if (m.state == "active") DropdownMenuItem({ Text("Devices and fingerprints") }, { menu = false; devices() })
                    if (m.state == "invited" && g.admin) act("Revoke invitation", null, "removed")
                    if (m.state == "active" && !self && m.role != "owner") {
                        if (g.admin && m.role != "admin") act("Make admin", "admin", null)
                        if (g.owner && m.role == "admin") act("Remove admin", "member", null)
                        if (g.admin && m.role == "member") act("Restrict", "restricted", null)
                        if (g.admin && m.role == "restricted") act("Unrestrict", "member", null)
                        if (g.admin && (m.role != "admin" || g.owner)) { act("Remove from group", null, "removed"); act("Remove and ban", null, "banned") }
                    }
                    if (self && m.role == "admin") act("Step down as admin", "member", null)
                    if (!self) DropdownMenuItem({ Text("Block", color = p.blockedText) }, { menu = false; block() })
                }
            }
        }
        if (showDevices) Column(Modifier.padding(start = 44.dp, top = 4.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            val list = groups.devices[m.address]
            when {
                list == null -> Text("Loading…", style = geist(12.sp), color = p.muted)
                list.isEmpty() -> Text("No devices visible.", style = geist(12.sp), color = p.muted)
                else -> list.forEach { d -> Text("${d.deviceId}: ${d.fingerprintDigits?.filter(Char::isDigit)?.chunked(5)?.joinToString(" ") ?: d.fingerprint}",
                    style = geistMono(11.5.sp), color = p.secondary, modifier = Modifier.testTag("fingerprint:" + d.deviceId)) }
            }
        }
    }
}

/** A `/g/<id>#token` link, shown before anything is joined. */
@Composable
fun JoinGroupDialog(groups: GroupsState, store: GroupStore, inbox: InboxState) {
    val request = groups.join ?: return
    val p = Pp.colors
    AlertDialog(onDismissRequest = store::dismissJoin, modifier = Modifier.testTag("joinGroup"), title = { Text(request.preview?.name ?: "Join group") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                val preview = request.preview
                when {
                    !groups.enabled && !groups.unavailable -> Text("Opening groups…")
                    groups.unavailable -> Text("Groups are not available on this device.")
                    preview == null && request.error == null -> Text("Looking up the group…")
                    preview != null -> {
                        preview.description?.let { Text(it) }
                        Text("${preview.memberCount} members · joining as ${inbox.acting?.name ?: ""}", style = geist(13.sp), color = p.secondary)
                        Text("Group messages are end-to-end encrypted. You will see messages from when you join, not before.", style = geist(12.5.sp), color = p.muted)
                        if (preview.myState == "active") Text("You are already a member.")
                        else if (!preview.canJoin) Text(if (preview.needs == "invitation") "You have an invitation to this group: accept it from Invitations in your conversation list." else "This group is invite only.")
                    }
                }
                request.error?.let { Text(it, color = p.blockedText) }
            }
        },
        confirmButton = {
            val preview = request.preview
            if (preview != null && preview.canJoin && preview.myState != "active") TextButton(store::confirmJoin, enabled = !request.joining) { Text(if (request.joining) "Joining…" else "Join") }
            else if (preview?.myState == "active") TextButton({ store.dismissJoin(); store.open(preview.id) }) { Text("Open") }
        },
        dismissButton = { TextButton(store::dismissJoin) { Text("Not now") } })
}
