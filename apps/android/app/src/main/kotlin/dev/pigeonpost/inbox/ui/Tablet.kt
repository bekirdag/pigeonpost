@file:OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)
package dev.pigeonpost.inbox.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandHorizontally
import androidx.compose.animation.shrinkHorizontally
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.Group
import dev.pigeonpost.core.groups.GroupFile
import dev.pigeonpost.core.groups.GroupsState
import androidx.compose.ui.semantics.contentDescription
import dev.pigeonpost.inbox.BuildConfig
import dev.pigeonpost.inbox.InboxViewModel
import dev.pigeonpost.inbox.calls.CallHost

/**
 * Tablets, foldables and landscape windows: the iPad/desktop shape (DesktopInboxView.swift) —
 * conversations with the mailbox switcher, the subjects of the open conversation, and the thread.
 * The conversations column can be hidden and brought back from controls that are always on screen:
 * hide from the column's own bar, show from the thread's bar.
 */
@Composable
fun TabletInbox(state: InboxState, store: InboxStore, model: InboxViewModel, username: String?, callMode: CallHost.Mode,
    newConversation: () -> Unit, settings: () -> Unit, info: () -> Unit, newSubject: () -> Unit,
    chooseFile: () -> Unit, choosePhoto: () -> Unit, attachment: (Attachment, String) -> Unit, openLink: (String) -> Unit,
    groups: GroupsState = GroupsState(), newGroup: () -> Unit = {}, groupInfo: () -> Unit = {},
    openGroupFile: (Group, GroupFile, String) -> Unit = { _, _, _ -> }, openFiltered: (FilteredGroup) -> Unit = {}, openPeer: (String) -> Unit = store::selectPeer) {
    val p = Pp.colors
    var columns by rememberSaveable { mutableStateOf(true) }
    val groupOpen = groups.open != null && model.groups != null
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val sidebarWidth = if (maxWidth >= 1000.dp) 300.dp else 264.dp
        val subjectsWidth = if (maxWidth >= 1000.dp) 220.dp else 190.dp
        Row(Modifier.fillMaxSize()) {
            AnimatedVisibility(columns, enter = expandHorizontally(), exit = shrinkHorizontally()) {
                Row(Modifier.fillMaxHeight()) {
                    Sidebar(state, store, model, groups, username, Modifier.width(sidebarWidth).fillMaxHeight().background(p.sidebar),
                        hide = { columns = false }, newConversation = newConversation, newGroup = newGroup, settings = settings, openFiltered = openFiltered, openPeer = openPeer)
                    Seam(Modifier.width(1.dp).fillMaxHeight())
                    // A group has no subjects: its thread takes the space.
                    if (!groupOpen) {
                        SubjectsColumn(state, store, Modifier.width(subjectsWidth).fillMaxHeight().background(p.column), newSubject)
                        Seam(Modifier.width(1.dp).fillMaxHeight())
                    }
                }
            }
            val show = if (columns) null else ({ columns = true })
            Box(Modifier.weight(1f).fillMaxHeight().background(p.wash)) {
                when {
                    groupOpen -> GroupPane(model, groups, state, model.groups!!, Modifier.fillMaxSize(), wide = true, callMode = callMode, info = groupInfo,
                        chooseFile = chooseFile, choosePhoto = choosePhoto, openFile = openGroupFile, showSidebar = show)
                    state.selectedPeer != null -> ConversationPane(state, store, model, Modifier.fillMaxSize(), wide = true, callMode = callMode,
                        info = info, newSubject = newSubject, chooseFile = chooseFile, choosePhoto = choosePhoto,
                        attachment = attachment, openLink = openLink, showSidebar = show)
                    else -> Column(Modifier.fillMaxSize()) {
                        if (show != null) Row(Modifier.fillMaxWidth().height(60.dp).padding(start = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                            IconButton(show, Modifier.testTag("showSidebar")) { Icon(Icons.Outlined.ViewSidebar, "Show conversations", tint = p.secondary) }
                        }
                        if (state.conversations.isEmpty()) EmptyPane("Inbox is up to date", "Start a conversation or open another inbox.", Modifier.weight(1f))
                        else EmptyPane("A place for your agents", "Choose a conversation or start a new one.", Modifier.weight(1f))
                    }
                }
            }
        }
    }
}

@Composable
private fun Sidebar(state: InboxState, store: InboxStore, model: InboxViewModel, groups: GroupsState, username: String?, modifier: Modifier, hide: () -> Unit,
    newConversation: () -> Unit, newGroup: () -> Unit, settings: () -> Unit, openFiltered: (FilteredGroup) -> Unit, openPeer: (String) -> Unit) {
    val p = Pp.colors
    var searching by rememberSaveable { mutableStateOf(false) }
    var invitesOpen by rememberSaveable { mutableStateOf(true) }
    if (state.leftView == LeftView.CALLS) {
        CallsPane(state, store, modifier, phone = false, open = openPeer, callBack = { key, video -> openPeer(key); model.calls?.call(key, video) },
            back = { store.showView(LeftView.INBOX) })
        return
    }
    if (state.leftView == LeftView.FILTERED) {
        FilteredPane(state, modifier, phone = false, back = { store.showView(LeftView.INBOX) }, open = openFiltered)
        return
    }
    var switching by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(state.acting?.address) { switching = false }
    Column(modifier) {
        Row(Modifier.fillMaxWidth().height(60.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(hide, Modifier.testTag("hideSidebar")) { Icon(Icons.Outlined.ViewSidebar, "Hide conversations", tint = p.secondary) }
            if (state.viewingArchive) ActionIcon("Back to inbox", Icons.AutoMirrored.Outlined.ArrowBack, tint = p.accent) { store.showArchive(false) }
            Text(if (state.viewingArchive) "Archive" else "Inbox", Modifier.weight(1f).padding(start = 4.dp), style = geist(17.sp, FontWeight.SemiBold), color = p.ink)
            ActionIcon("Search conversations", Icons.Outlined.Search) { searching = !searching; if (!searching) store.filter("") }
            ActionIcon("New conversation", Icons.Outlined.Edit, action = newConversation)
            if (groups.enabled) ActionIcon("New group", Icons.Outlined.GroupAdd, action = newGroup)
            ActionIcon("Settings", Icons.Outlined.Settings, action = settings)
        }
        Seam(Modifier.fillMaxWidth().height(1.dp))
        MailboxSwitcher(state, username, switching, { switching = !switching }) { mailbox -> switching = false; if (mailbox.address != state.acting?.address) store.switchMailbox(mailbox) }
        if (groups.enabled && "orgs" in state.features && model.organizationRooms != null) Row(Modifier.padding(horizontal = 8.dp).fillMaxWidth().clip(RoundedCornerShape(9.dp))
            .clickable(onClick = model::openOrganizationRooms).padding(horizontal = 10.dp, vertical = 12.dp).testTag("sidebar:organizationRooms"),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Icon(Icons.Outlined.Business, null, Modifier.size(18.dp), tint = p.secondary)
            Text("Organization rooms", style = geist(13.sp), color = p.secondary)
        }
        if (model.meetingNotes != null && Features.MEETING_SUMMARIES in state.features) Row(Modifier.padding(horizontal = 8.dp).fillMaxWidth().clip(RoundedCornerShape(9.dp))
            .clickable(onClickLabel = "Open meeting notes", onClick = model::openMeetingNotes).padding(horizontal = 10.dp, vertical = 12.dp).testTag("sidebar:meetingNotes"),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Icon(Icons.Outlined.Description, null, Modifier.size(18.dp), tint = p.secondary)
            Text("Meeting notes", style = geist(14.sp, FontWeight.Medium), color = p.ink)
        }
        if (model.calendar != null && "calendar" in state.features) Row(Modifier.padding(horizontal = 8.dp).fillMaxWidth().clip(RoundedCornerShape(9.dp))
            .clickable(onClickLabel = "Open calendar", onClick = model::openCalendar).padding(horizontal = 10.dp, vertical = 12.dp).testTag("sidebar:calendar"),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Icon(Icons.Outlined.CalendarMonth, null, Modifier.size(18.dp), tint = p.secondary)
            Text("Calendar", style = geist(14.sp, FontWeight.Medium), color = p.ink)
        }
        // Calls, under the mailbox switcher (SDS §12.1), with the unseen-missed badge.
        if (state.has(Features.CALL_HISTORY)) Row(Modifier.padding(horizontal = 8.dp).fillMaxWidth().clip(RoundedCornerShape(9.dp))
            .clickable(onClickLabel = "Open calls") { store.showView(LeftView.CALLS) }.padding(horizontal = 10.dp, vertical = 9.dp).testTag("sidebar:calls")
            .semantics(mergeDescendants = true) { contentDescription = if (state.calls.unseen > 0) "Calls, ${state.calls.unseen} missed" else "Calls" },
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Icon(Icons.Outlined.Call, null, Modifier.size(18.dp), tint = p.secondary)
            Text("Calls", Modifier.weight(1f), style = geist(14.sp, FontWeight.Medium), color = p.ink)
            if (state.calls.unseen > 0) UnreadBadge(state.calls.unseen)
        }
        if (state.loading) Row(Modifier.padding(horizontal = 14.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp, color = p.muted)
            Text(if (state.loaded) "Updating inbox…" else "Loading inbox…", style = geist(12.sp), color = p.muted)
        }
        if (searching) SearchField(state.filter, store::filter, "Search conversations", Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, bottom = 10.dp))
        if (state.offline) Text("Offline · showing the last loaded conversations", Modifier.padding(horizontal = 16.dp, vertical = 6.dp), color = p.heldText, style = geist(12.sp, FontWeight.Medium))
        val filtered = if (!state.viewingArchive && state.filter.isEmpty()) state.filteredGroups.size else 0
        val hasGroups = groups.enabled && (groups.joined.isNotEmpty() || groups.invitations.isNotEmpty()) && !state.viewingArchive
        if (state.visible.isEmpty() && !hasGroups && filtered == 0) EmptyPane(if (state.filter.isNotEmpty()) "No matches" else if (state.viewingArchive) "Archive is empty" else "No conversations yet",
            if (state.filter.isNotEmpty()) "Try a name or address." else "Your conversations will appear here.", Modifier.weight(1f))
        else LazyColumn(Modifier.weight(1f), contentPadding = PaddingValues(vertical = 2.dp)) {
            model.groups?.let { gs -> groupRows(groups, state, gs, phone = false, invitesOpen = invitesOpen, toggleInvites = { invitesOpen = !invitesOpen }, open = model::openGroup) }
            items(state.visible, key = { it.peer }) { conversation ->
                ConversationRow(conversation, conversation.peer == state.selectedPeer && groups.open == null, phone = false) { openPeer(conversation.peer) }
            }
            if (filtered > 0) item(key = "filtered") { FilteredEntry(filtered) { store.showView(LeftView.FILTERED) } }
        }
        Seam(Modifier.fillMaxWidth().height(1.dp))
        Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("Pigeonpost ${BuildConfig.VERSION_NAME}", Modifier.weight(1f), style = geist(12.sp), color = p.muted)
            ActionIcon("Refresh", Icons.Outlined.Refresh, enabled = !state.loading) { store.refresh(); model.groups?.poke() }
        }
    }
}

/** Which mailbox is being read, and the way to change it: a card that opens a list inside the column. */
@Composable
private fun MailboxSwitcher(state: InboxState, username: String?, open: Boolean, toggle: () -> Unit, select: (Mailbox) -> Unit) {
    val p = Pp.colors
    val acting = state.acting ?: return
    val many = state.mailboxes.size > 1
    Row(Modifier.padding(12.dp).fillMaxWidth().clip(RoundedCornerShape(10.dp)).background(p.field.copy(alpha = .9f))
        .border(1.dp, p.stroke, RoundedCornerShape(10.dp)).clickable(enabled = many, role = Role.Button, onClickLabel = "Switch inbox", onClick = toggle)
        .testTag("mailboxSwitcher").padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        Avatar(acting.key, acting.name, 30.dp)
        Column(Modifier.weight(1f)) {
            Text("MAILBOX", style = geist(10.5.sp, FontWeight.Medium).copy(letterSpacing = 0.6.sp), color = p.muted)
            Text(acting.name, style = geistMono(13.sp, FontWeight.Medium), color = p.ink, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
        }
        if (many) Icon(Icons.Outlined.ExpandMore, null, Modifier.size(20.dp).rotate(if (open) 180f else 0f), tint = p.secondary)
    }
    if (open) Column(Modifier.heightIn(max = 260.dp).verticalScroll(rememberScrollState()).padding(start = 8.dp, end = 8.dp, bottom = 10.dp)) {
        orderedMailboxes(state.mailboxes, username).forEach { mailbox ->
            val current = mailbox.address == acting.address
            Row(verticalAlignment = Alignment.CenterVertically) {
                Row(Modifier.weight(1f).clip(RoundedCornerShape(8.dp)).background(if (current) p.selected else Color.Transparent)
                    .clickable { select(mailbox) }.semantics { selected = current }.testTag("mailbox:" + mailbox.address)
                    .padding(horizontal = 12.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Avatar(mailbox.key, mailbox.name, 24.dp)
                    Text(mailbox.name, Modifier.weight(1f), style = geistMono(12.5.sp), color = p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    if (current) Icon(Icons.Outlined.Check, "Selected", Modifier.size(14.dp), tint = p.accent)
                }
                CopyAddressButton(mailbox.key)
            }
        }
    }
}

/** The subjects of the open conversation, as a column of their own (the web app's "Threads"). */
@Composable
private fun SubjectsColumn(state: InboxState, store: InboxStore, modifier: Modifier, newSubject: () -> Unit) {
    val p = Pp.colors
    val conversation = state.conversation
    var deleting by remember { mutableStateOf<Subject?>(null) }
    Column(modifier) {
        if (conversation == null) return@Column
        Row(Modifier.fillMaxWidth().height(60.dp).padding(start = 18.dp, end = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("THREADS", style = geist(11.sp).copy(letterSpacing = 0.66.sp), color = p.muted)
                Text(conversation.name, style = geistMono(12.5.sp), color = p.quiet, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
            }
            Box(Modifier.size(32.dp).clip(RoundedCornerShape(8.dp)).background(p.field).border(1.dp, p.strokeStrong, RoundedCornerShape(8.dp))
                .clickable(role = Role.Button, onClickLabel = "New subject", onClick = newSubject), contentAlignment = Alignment.Center) {
                Icon(Icons.Outlined.Add, "New subject", Modifier.size(18.dp), tint = p.ink)
            }
        }
        Seam(Modifier.fillMaxWidth().height(1.dp))
        LazyColumn(Modifier.weight(1f), contentPadding = PaddingValues(top = 10.dp)) {
            items(state.subjects, key = { it.id }) { subject ->
                val selected = subject.id == state.subject?.id
                val unnamed = subject.title.isNullOrBlank() && !subject.isDefault && subject.id.isNotEmpty()
                Row(Modifier.padding(horizontal = 8.dp, vertical = 1.dp).fillMaxWidth().clip(RoundedCornerShape(9.dp))
                    .background(if (selected) p.selected else Color.Transparent)
                    .combinedClickable(onClick = { store.selectSubject(subject.id) }, onLongClick = { if (subject.id.isNotEmpty()) deleting = subject })
                    .semantics { this.selected = selected }.testTag("subject:" + subject.name)
                    .padding(horizontal = 10.dp, vertical = 9.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(subject.name, Modifier.weight(1f), style = geist(13.5.sp, if (selected) FontWeight.Medium else FontWeight.Normal)
                        .copy(fontStyle = if (unnamed) FontStyle.Italic else FontStyle.Normal), color = if (unnamed) p.secondary else p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    if (subject.unread > 0) Dot(p.accent)
                    if (subject.last > 0) Text(shortTime(subject.last), style = geist(11.5.sp), color = p.muted)
                }
            }
        }
    }
    deleting?.let { subject ->
        Confirm("Delete subject?", "${subject.messages.size} message${if (subject.messages.size == 1) "" else "s"} in “${subject.name}” will be deleted from this inbox. Other inboxes keep their copies.", "Delete",
            { store.deleteThread(subject.id) { deleting = null } }, { deleting = null }, enabled = !state.actionBusy)
    }
}
