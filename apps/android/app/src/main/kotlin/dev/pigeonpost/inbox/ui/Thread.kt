@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class, androidx.compose.foundation.ExperimentalFoundationApi::class)
package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.CallMissed
import androidx.compose.material.icons.automirrored.outlined.Reply
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.InboxViewModel
import dev.pigeonpost.inbox.calls.CallHost
import kotlinx.coroutines.delay

/**
 * One conversation: its bar, its subjects (a strip on a phone; the middle column on a tablet, see
 * Tablet.kt), the history and the composer.
 */
@Composable
fun ConversationPane(state: InboxState, store: InboxStore, model: InboxViewModel, modifier: Modifier, wide: Boolean,
    callMode: CallHost.Mode, info: () -> Unit, newSubject: () -> Unit, chooseFile: () -> Unit, choosePhoto: () -> Unit,
    attachment: (Attachment, String) -> Unit, openLink: (String) -> Unit, showSidebar: (() -> Unit)? = null) {
    val p = Pp.colors
    val staging by model.stagingAttachments.collectAsStateWithLifecycle()
    val loadingFiles = staging[state.draftKey]?.let { it > 0 } == true
    val peer = state.selectedPeer.orEmpty()
    val subjects = state.subjects
    val subject = state.subject
    val messages = subject?.messages.orEmpty()
    val name = peerLabel(peer, state.conversation?.contact?.alias, state.conversation?.name ?: displayName(peer))
    var searching by rememberSaveable(peer) { mutableStateOf(false) }
    var query by rememberSaveable(peer) { mutableStateOf("") }
    var match by rememberSaveable(peer) { mutableIntStateOf(0) }
    var quotedMessage by remember(state.acting?.address, peer) { mutableStateOf<String?>(null) }
    LaunchedEffect(quotedMessage) { if (quotedMessage != null) { delay(2200); quotedMessage = null } }
    var deleteSubject by remember { mutableStateOf(false) }
    var addMenu by remember { mutableStateOf(false) }
    val hits = remember(messages, query) { if (query.isBlank()) emptyList() else messages.filter { it.display.text.contains(query, true) }.map { it.id } }
    var latestRequest by remember(state.acting?.address, peer, subject?.id) { mutableIntStateOf(0) }
    val editorFocus = remember { FocusRequester() }
    LaunchedEffect(state.composerAction?.id) { if (state.composerAction != null) runCatching { editorFocus.requestFocus() } }
    // A conversation just started, or followed to where the server filed it: the field is ready for the next words.
    LaunchedEffect(state.composerFocus, state.draftKey) { if (state.composerFocus) { runCatching { editorFocus.requestFocus() }; store.composerFocused() } }
    var editorFocused by remember { mutableStateOf(false) }
    val calling = callMode != CallHost.Mode.HIDDEN
    val calls = model.calls
    val askBlock = LocalAskBlock.current
    var headerMenu by remember { mutableStateOf(false) }
    val timer = subject?.ephemeralSecs
    val showParent: (MessageReply) -> Unit = { reply ->
        val live = store.state.value
        if (live.acting?.address == state.acting?.address && live.selectedPeer == peer) {
            val found = live.subjects.mapNotNull { row -> row.messages.firstOrNull {
                it.id == reply.messageId || reply.msgRef != null && it.msgRef == reply.msgRef
            }?.let { row to it.id } }.firstOrNull()
            if (found == null) store.showError("The original message is no longer available in this inbox.")
            else { store.selectSubject(found.first.id); searching = false; query = ""; quotedMessage = found.second }
        }
    }
    Column(modifier.fillMaxHeight().background(p.wash).onPreviewKeyEvent {
        if (it.key == Key.Escape && it.type == KeyEventType.KeyUp && state.composerAction != null && !state.isSending && state.unconfirmedSend == null) {
            store.cancelMessageAction(); true
        } else false
    }) {
        // The bar: who this is with (the name is the button for "who is this"), then the actions.
        Row(Modifier.fillMaxWidth().heightIn(min = 60.dp).background(p.wash).padding(start = if (wide && showSidebar == null) 14.dp else 2.dp, end = 4.dp),
            verticalAlignment = Alignment.CenterVertically) {
            if (!wide) ActionIcon("Back to conversations", Icons.AutoMirrored.Outlined.ArrowBack, tint = p.accent) { store.selectPeer(null) }
            showSidebar?.let { show -> IconButton(show, Modifier.testTag("showSidebar")) { Icon(Icons.Outlined.ViewSidebar, "Show conversations", tint = p.secondary) } }
            Row(Modifier.weight(1f).clip(RoundedCornerShape(10.dp)).clickable(onClickLabel = "About $name", onClick = info).padding(horizontal = 6.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Avatar(peer, name, if (wide) 30.dp else 34.dp)
                Column(Modifier.weight(1f)) {
                    Text(name, style = geist(14.5.sp, FontWeight.SemiBold), color = if (name.startsWith("~")) p.secondary else p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(peer, style = geistMono(11.5.sp), color = p.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                if (timer != null) Icon(Icons.Outlined.Timer, "Messages disappear after ${timerLabel(timer)}", Modifier.size(16.dp).testTag("timerBadge"), tint = p.muted)
            }
            ActionIcon("Find in conversation", Icons.Outlined.Search) { searching = !searching; if (!searching) query = "" }
            ActionIcon("Voice call", Icons.Outlined.Call, enabled = !calling, tint = p.accent) { calls?.call(peer, video = false) }
            ActionIcon("Video call", Icons.Outlined.Videocam, enabled = !calling, tint = p.accent) { calls?.call(peer, video = true) }
            ActionIcon("Conversation info", Icons.Outlined.Info, action = info)
            Box {
                ActionIcon("More conversation actions", Icons.Outlined.MoreVert) { headerMenu = true }
                DropdownMenu(headerMenu, { headerMenu = false }, containerColor = p.ground) {
                    if (state.has(Features.DISAPPEARING)) DropdownMenuItem({ Text("Disappearing messages") }, { headerMenu = false; info() })
                    if (state.blockedPeer) DropdownMenuItem({ Text("Unblock") }, { headerMenu = false; state.conversation?.contact?.let { store.unblock(it.peer) } })
                    else if (state.conversation?.identity == null) DropdownMenuItem({ Text("Block", color = p.blockedText) }, { headerMenu = false; askBlock(BlockTarget(peer, name)) })
                }
            }
            if (wide && subject?.id?.isNotEmpty() == true) ActionIcon("Delete subject", Icons.Outlined.DeleteOutline) { deleteSubject = true }
        }
        Seam(Modifier.fillMaxWidth().height(1.dp))
        if (!wide) {
            Row(Modifier.fillMaxWidth().padding(end = 4.dp, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Row(Modifier.weight(1f).horizontalScroll(rememberScrollState()).padding(start = 12.dp, end = 8.dp), horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalAlignment = Alignment.CenterVertically) {
                    subjects.forEach { item -> SubjectChip(item, item.id == subject?.id) { store.selectSubject(item.id) } }
                    Box(Modifier.size(36.dp).clip(RoundedCornerShape(9.dp)).background(p.field).border(1.dp, p.strokeStrong, RoundedCornerShape(9.dp))
                        .clickable(onClickLabel = "New subject", role = Role.Button, onClick = newSubject), contentAlignment = Alignment.Center) {
                        Icon(Icons.Outlined.Add, "New subject", Modifier.size(18.dp), tint = p.ink)
                    }
                }
                if (subject?.id?.isNotEmpty() == true) ActionIcon("Delete subject", Icons.Outlined.DeleteOutline) { deleteSubject = true }
            }
            Seam(Modifier.fillMaxWidth().height(1.dp))
        }
        if (searching) Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(query, { query = it; match = 0 }, Modifier.weight(1f), label = { Text("Find in this subject") }, singleLine = true,
                shape = RoundedCornerShape(10.dp), textStyle = geist(14.5.sp))
            Text(if (hits.isEmpty()) "0" else "${match.mod(hits.size) + 1}/${hits.size}", Modifier.padding(8.dp), style = geist(12.sp), color = p.muted)
            ActionIcon("Next match", Icons.Outlined.ArrowDownward) { if (hits.isNotEmpty()) match++ }
        }
        if (messages.isEmpty()) EmptyPane("Start a conversation", timer?.let { "Messages disappear after ${timerLabel(it)}." } ?: "Send a message in ${subject?.name ?: "General"}.", Modifier.weight(1f))
        else key(state.acting?.address, peer, subject?.id) {
            ConversationHistory(messages, Modifier.weight(1f),
                searchTarget = quotedMessage ?: if (hits.isEmpty()) null else hits[match.mod(hits.size)], latestRequest = latestRequest) { message ->
                if (message.system) CallLine(message.body) else MessageBubble(message, query.isNotBlank() && message.display.text.contains(query, true),
                    quotedMessage == message.id || hits.isNotEmpty() && hits[match.mod(hits.size)] == message.id,
                    store, attachment, openLink, model, state.acting?.address.orEmpty(), state, showParent)
            }
        }
        // A blocked person: the banner and Unblock instead of the composer (SDS §15.2).
        if (state.blockedPeer) BlockedBanner(name, state.actionBusy) { state.conversation?.contact?.let { store.unblock(it.peer) } }
        // The composer: one card holding the attach button, the field and the send button.
        else Column(Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, top = 6.dp, bottom = 10.dp)
            .clip(RoundedCornerShape(16.dp)).background(p.composer).border(1.dp, p.stroke, RoundedCornerShape(16.dp))) {
            MessageComposerContext(state, store)
            if (state.attachments.isNotEmpty()) Row(Modifier.horizontalScroll(rememberScrollState()).padding(start = 10.dp, end = 10.dp, top = 10.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                state.attachments.forEach { file -> InputChip(false, { store.removeAttachment(file.id) }, enabled = !state.isSending,
                    colors = InputChipDefaults.inputChipColors(containerColor = p.field, labelColor = p.ink), border = BorderStroke(1.dp, p.stroke),
                    label = { Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        AttachmentPreview("draft:${file.id}", file.name, file.mediaType, compact = true) { model.files.preview(file) }
                        Text(file.name, maxLines = 1, style = geist(13.sp))
                    } }, trailingIcon = { Icon(Icons.Outlined.Close, "Remove ${file.name}", Modifier.size(16.dp)) }) }
            }
            if (loadingFiles) Row(Modifier.padding(start = 14.dp, top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp, color = p.accent)
                Text("Preparing attachments…", Modifier.padding(start = 8.dp), style = geist(12.sp), color = p.muted)
            }
            if (state.quota?.full == true) Text("Storage is full. Delete messages or attachments to make space.", Modifier.padding(horizontal = 14.dp, vertical = 8.dp), color = p.blockedText, style = geist(13.sp))
            Row(Modifier.fillMaxWidth().padding(start = 2.dp, end = 8.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.Bottom) {
                Box(Modifier.padding(bottom = 4.dp)) {
                    ActionIcon("Add attachment", Icons.Outlined.AttachFile, enabled = !state.isSending && state.unconfirmedSend == null && state.composerAction?.editing != true) { addMenu = true }
                    DropdownMenu(addMenu, { addMenu = false }, containerColor = p.ground) {
                        DropdownMenuItem({ Text("Choose document") }, { addMenu = false; chooseFile() })
                        DropdownMenuItem({ Text("Choose photo") }, { addMenu = false; choosePhoto() })
                    }
                }
                key(state.draftKey) {
                    MessageEditor(state.draft, store::draft, !state.isSending && state.unconfirmedSend == null,
                        Modifier.weight(1f).testTag("messageEditor").onFocusChanged { editorFocused = it.isFocused; if (it.isFocused) latestRequest++ }, model::pasteAttachments, editorFocus,
                        placeholder = if (state.composerAction?.editing == true) "Edit message" else "Message")
                }
                val canSend = store.canSubmitMessage() && !loadingFiles
                IconButton({
                    val keepFocus = editorFocused
                    latestRequest++; store.send()
                    // The next message can be typed at once: the field keeps focus and the keyboard stays up.
                    if (keepFocus) runCatching { editorFocus.requestFocus() }
                }, enabled = canSend, modifier = Modifier.padding(bottom = 8.dp).size(40.dp).clip(RoundedCornerShape(11.dp))
                    .background(if (canSend) p.accent else p.chip)) {
                    if (state.isSending) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = p.onAccent)
                    else Icon(if (state.composerAction?.editing == true) Icons.Outlined.Check else Icons.Outlined.ArrowUpward,
                        if (state.composerAction?.editing == true) "Save edit" else "Send message", Modifier.size(20.dp), tint = if (canSend) p.onAccent else p.muted)
                }
            }
        }
    }
    if (deleteSubject) Confirm("Delete subject?", "This removes the subject and its messages from this inbox. Other inboxes keep their copies.", "Delete",
        { subject?.id?.let { store.deleteThread(it) { deleteSubject = false } } }, { deleteSubject = false }, enabled = !state.actionBusy)
    state.messageInfo?.let { MessageInfoDialog(it, store, state) }
}

@Composable
private fun SubjectChip(subject: Subject, selected: Boolean, select: () -> Unit) {
    val p = Pp.colors
    Row(Modifier.height(36.dp).clip(RoundedCornerShape(9.dp)).background(if (selected) p.selected else Color.Transparent)
        .selectable(selected = selected, role = Role.Tab, onClick = select).testTag("subject:" + subject.id).padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(subject.name + if (subject.unread > 0) " (${subject.unread})" else "", style = geist(14.sp, if (selected) FontWeight.Medium else FontWeight.Normal),
            color = if (selected) p.ink else p.secondary, maxLines = 1)
    }
}

/**
 * A card rather than a bubble: an optional header for the server's decision, the words, any files,
 * and a footer with copy, the time and the actions, each band divided by a hairline. The words are
 * selectable in part; the long-press menu lives on the header and footer bands and the ⋯ button.
 */
@Suppress("DEPRECATION")
@Composable
fun MessageBubble(message: ThreadMessage, highlighted: Boolean, current: Boolean, store: InboxStore, attachment: (Attachment, String) -> Unit,
    openLink: (String) -> Unit, model: InboxViewModel, identity: String, state: InboxState, showParent: (MessageReply) -> Unit) {
    val p = Pp.colors
    var menu by remember { mutableStateOf(false) }
    var original by remember { mutableStateOf(false) }
    var confirm by remember { mutableStateOf<String?>(null) }
    var copied by remember { mutableStateOf(false) }
    val askBlock = LocalAskBlock.current
    LaunchedEffect(copied) { if (copied) { delay(1400); copied = false } }
    val clipboard = LocalClipboardManager.current
    val mine = message.outgoing
    val editable = message.canEditMessage(state.features, System.currentTimeMillis() / 1000)
    val replyable = state.has(Features.MESSAGE_REPLY) && message.status == Delivery.SENT && !state.blockedPeer
    val pendingId = message.pendingEditId ?: message.id
    val canChangeMode = !state.isSending && state.unconfirmedSend == null && !state.blockedPeer
    val heldRequest = !mine && message.verb != null && message.autonomy != "auto"
    val fill = if (mine) p.mineFill else if (heldRequest) p.heldCardFill else p.ground
    val stroke = if (mine) p.mineStroke else if (heldRequest) p.heldCardStroke else p.stroke
    val divider = if (mine) p.mineRule else if (heldRequest) p.heldCardStroke else p.tile
    val meta = if (mine) p.mineMeta else if (heldRequest) p.heldCardText else p.muted
    val shape = RoundedCornerShape(14.dp)
    val openMenu = { menu = true }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = if (mine) Arrangement.End else Arrangement.Start) {
        if (mine) Spacer(Modifier.width(36.dp))
        Column(Modifier.weight(1f, fill = false).widthIn(max = 620.dp)) {
        Column(Modifier.fillMaxWidth().clip(shape).background(fill)
            .border(if (current) 2.dp else 1.dp, if (current) p.found else if (highlighted) p.found.copy(alpha = .6f) else stroke, shape)) {
            val decision = !mine && message.verb != null
            if (decision || message.display.unattended) {
                Row(Modifier.fillMaxWidth().combinedClickable(onClick = {}, onLongClick = openMenu, onLongClickLabel = "Message actions")
                    .padding(horizontal = 16.dp, vertical = 9.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (decision) {
                        if (message.autonomy == "auto") {
                            Icon(Icons.Outlined.Bolt, null, Modifier.size(16.dp), tint = p.accent)
                            BadgeText("Auto · ${message.verb}", MaterialTheme.colorScheme.secondary)
                        } else {
                            Icon(Icons.Outlined.PauseCircle, null, Modifier.size(16.dp), tint = p.heldText)
                            BadgeText("Held for review · ${message.verb}", MaterialTheme.colorScheme.tertiary)
                        }
                    }
                    if (message.display.unattended) BadgeText(if (message.display.failed) "Unattended · failed" else "Unattended reply", MaterialTheme.colorScheme.onSurfaceVariant)
                }
                Seam(Modifier.fillMaxWidth().height(1.dp), divider)
            }
            Column(Modifier.padding(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                message.replyTo?.let { reply -> MessageReplyQuote(reply, state.conversation?.name ?: "Sender", mine) { showParent(reply) } }
                message.heldBecause?.takeIf { message.autonomy == "review" }?.let {
                    Text(it.replace('_', ' '), style = geist(12.5.sp), color = if (heldRequest) p.heldCardText else p.muted)
                }
                if (message.body.isNotEmpty()) Markdown(message.display.text, openLink, mine = mine, held = heldRequest)
                message.attachments.forEach { file -> AttachmentTile(file, identity, model, mine, attachment) }
            }
            Seam(Modifier.fillMaxWidth().height(1.dp), divider)
            Row(Modifier.fillMaxWidth().combinedClickable(onClick = {}, onLongClick = openMenu, onLongClickLabel = "Message actions")
                .padding(start = 6.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                Spacer(Modifier.weight(1f))
                if ((message.editCount ?: 0) > 0 || (message.editedAt ?: 0) > 0) Text("edited", Modifier.padding(end = 6.dp), style = geist(11.sp), color = meta)
                message.expiresAt?.let { at -> Icon(Icons.Outlined.Timer, "Disappears " + java.text.DateFormat.getDateTimeInstance(java.text.DateFormat.SHORT, java.text.DateFormat.SHORT).format(java.util.Date(at * 1000)),
                    Modifier.padding(end = 4.dp).size(13.dp), tint = meta) }
                Text(when (message.status) {
                    Delivery.SENDING -> if (message.pendingEditId != null) "Saving…" else "Sending…"
                    Delivery.FAILED -> if (message.pendingEditId != null) "Edit unconfirmed" else "Delivery unconfirmed"
                    Delivery.SENT -> shortTime(message.at)
                },
                    style = geist(12.sp, if (message.status == Delivery.FAILED) FontWeight.SemiBold else FontWeight.Normal),
                    color = if (message.status == Delivery.FAILED) p.blockedText else meta)
                Spacer(Modifier.width(5.dp))
                MessageReceiptMark(message, state, meta)
                Box {
                    IconButton(openMenu, Modifier.size(36.dp)) { Icon(Icons.Outlined.MoreHoriz, "Message actions", Modifier.size(18.dp), tint = meta) }
                    DropdownMenu(menu, { menu = false }, containerColor = p.ground) {
                        if (replyable) DropdownMenuItem({ Text("Reply") }, { menu = false; store.beginReply(message.id) }, enabled = canChangeMode)
                        if (editable) DropdownMenuItem({ Text("Edit") }, { menu = false; store.beginEdit(message.id) }, enabled = canChangeMode)
                        DropdownMenuItem({ Text("Copy text") }, { menu = false; clipboard.setText(AnnotatedString(message.display.text)) })
                        if (state.features.any { it in Features.MESSAGE_ACTIONS } && state.messages.any { it.id == message.id })
                            DropdownMenuItem({ Text("Message info") }, { menu = false; store.showMessageInfo(message.id) })
                        DropdownMenuItem({ Text("Original message") }, { menu = false; original = true })
                        if (message.status != Delivery.SENDING) DropdownMenuItem({ Text(if (message.status == Delivery.FAILED) "Stop retrying" else "Delete message") }, { menu = false; confirm = "delete" })
                        if (!message.outgoing) DropdownMenuItem({ Text("Report message") }, { menu = false; confirm = "spam" })
                        if (!message.outgoing) DropdownMenuItem({ Text("Block sender", color = p.blockedText) }, { menu = false
                            store.state.value.selectedPeer?.let { peer -> askBlock(BlockTarget(peer, store.state.value.conversation?.name ?: displayName(peer), messageId = message.id)) } })
                    }
                }
            }
        }
        Row(Modifier.fillMaxWidth().testTag("messageActions:" + message.id), horizontalArrangement = if (mine) Arrangement.End else Arrangement.Start,
            verticalAlignment = Alignment.CenterVertically) {
            if (!mine && replyable) IconButton({ store.beginReply(message.id) }, enabled = canChangeMode, modifier = Modifier.size(36.dp).testTag("reply:" + message.id)) {
                Icon(Icons.AutoMirrored.Outlined.Reply, "Reply to message", Modifier.size(18.dp), tint = p.secondary)
            }
            if (mine && editable) IconButton({ store.beginEdit(message.id) }, enabled = canChangeMode, modifier = Modifier.size(36.dp).testTag("edit:" + message.id)) {
                Icon(Icons.Outlined.Edit, "Edit message", Modifier.size(18.dp), tint = p.secondary)
            }
            IconButton({ clipboard.setText(AnnotatedString(message.display.text)); copied = true }, Modifier.size(36.dp)) {
                Icon(if (copied) Icons.Outlined.Check else Icons.Outlined.ContentCopy, "Copy message", Modifier.size(18.dp), tint = p.secondary)
            }
            if (message.status == Delivery.FAILED && state.unconfirmedSend != pendingId && store.canRetrySend(pendingId))
                TextButton({ store.retrySend(pendingId) }) { Text("Retry", style = geist(12.sp)) }
        }
        }
        if (!mine) Spacer(Modifier.width(36.dp))
    }
    if (original) PageDialog("Original message", { original = false }) { SelectionContainer { Text(message.body, style = geistMono(13.sp), color = p.body) } }
    if (confirm != null) Confirm(if (confirm == "spam") "Report this message?" else if (message.status == Delivery.FAILED) "Stop retrying?" else "Delete message?",
        if (confirm == "spam") "Report this message and its sender for spam, abuse, or other prohibited content. This does not delete your copy."
            else if (message.status == Delivery.FAILED) "The message may already have arrived. This stops retrying the pending message."
            else "This removes your copy only.",
        if (confirm == "spam") "Report" else if (message.status == Delivery.FAILED) "Stop retrying" else "Delete",
        { if (confirm == "spam") store.reportSpam(message.id) else store.deleteMessage(if (message.status == Delivery.FAILED) pendingId else message.id); confirm = null }, { confirm = null })
}

/** A file in a message: its preview, its name and size, and a download icon. Tapping opens it in the app when it can be shown safely. */
@Composable
private fun AttachmentTile(file: Attachment, identity: String, model: InboxViewModel, mine: Boolean, attachment: (Attachment, String) -> Unit) {
    val p = Pp.colors
    var fileMenu by remember { mutableStateOf(false) }
    val viewable = AttachmentKinds.viewer(file.filename, file.mediaType) != null
    val tap = { if (viewable) attachment(file, "view") else fileMenu = true }
    Box {
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Box(Modifier.clip(RoundedCornerShape(8.dp)).combinedClickable(onClick = tap, onLongClick = { fileMenu = true })) {
                AttachmentPreview("$identity:${file.id}", file.filename, file.mediaType) { model.files.preview(identity, file) }
            }
            Row(Modifier.widthIn(max = 360.dp).clip(RoundedCornerShape(10.dp)).background(if (mine) p.mineRule else p.field)
                .border(1.dp, if (mine) p.mineStroke else p.stroke, RoundedCornerShape(10.dp))
                .combinedClickable(onClick = tap, onLongClick = { fileMenu = true }, onClickLabel = if (viewable) "View ${file.filename}" else "Open ${file.filename}")
                .padding(start = 10.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Box(Modifier.size(30.dp).background(p.tile, RoundedCornerShape(7.dp)), contentAlignment = Alignment.Center) {
                    Icon(Icons.Outlined.Description, null, Modifier.size(17.dp), tint = p.secondary)
                }
                Column(Modifier.weight(1f, fill = false).padding(vertical = 8.dp)) {
                    Text(file.filename, style = geist(13.5.sp, FontWeight.Medium), color = p.ink, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    Text(bytes(file.bytes), style = geist(12.sp), color = p.muted)
                }
                IconButton({ attachment(file, "download") }, Modifier.size(40.dp)) { Icon(Icons.Outlined.Download, "Download ${file.filename}", Modifier.size(18.dp), tint = p.secondary) }
            }
        }
        DropdownMenu(fileMenu, { fileMenu = false }, containerColor = p.ground) {
            listOf("Open" to "open", "Share" to "share", "Save a copy" to "save").forEach { (label, action) ->
                DropdownMenuItem({ Text(label) }, { fileMenu = false; attachment(file, action) })
            }
        }
    }
}

@Composable
internal fun Dot(color: Color) = Box(Modifier.size(6.dp).background(color, CircleShape))

/** "Missed video call · 14:32": a call this conversation had, drawn as a quiet line (SDS §12.1). */
@Composable
fun CallLine(text: String) {
    val p = Pp.colors
    Box(Modifier.fillMaxWidth().padding(vertical = 2.dp), contentAlignment = Alignment.Center) {
        Row(Modifier.clip(RoundedCornerShape(10.dp)).background(p.tile).padding(horizontal = 10.dp, vertical = 5.dp).testTag("missedCall"), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.AutoMirrored.Outlined.CallMissed, null, Modifier.size(14.dp), tint = p.blockedText)
            Text(" $text", style = geist(12.sp, FontWeight.Medium), color = p.secondary)
        }
    }
}
