@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class, androidx.compose.foundation.ExperimentalFoundationApi::class)
package dev.pigeonpost.inbox.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Send
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.InboxViewModel
import dev.pigeonpost.inbox.R
import dev.pigeonpost.inbox.auth.SessionState
import kotlinx.coroutines.launch
import java.text.DateFormat
import java.util.Date

@Composable
fun PigeonpostApp(model: InboxViewModel, signIn: (String?, Boolean) -> Unit, chooseFile: () -> Unit,
    choosePhoto: () -> Unit, scan: () -> Unit, attachment: (Attachment, String) -> Unit, openLink: (String) -> Unit) {
    val session by model.session.state.collectAsStateWithLifecycle()
    val state by model.inbox.state.collectAsStateWithLifecycle()
    val handleState by model.handles.state.collectAsStateWithLifecycle()
    val store = model.inbox
    var sheet by rememberSaveable { mutableStateOf<String?>(null) }
    var editContact by remember { mutableStateOf<Contact?>(null) }
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(state.error) { state.error?.let { snackbar.showSnackbar(it); store.clearError() } }
    LaunchedEffect(session.signedIn, state.acting?.address) { sheet = null; editContact = null }
    val lifecycle by LocalLifecycleOwner.current.lifecycle.currentStateFlow.collectAsState()
    val visibleIds = state.subject?.messages.orEmpty().map { it.id }
    // A failed acknowledgement may revert read marks. That alone must not trigger a retry loop.
    LaunchedEffect(state.acting?.address, state.selectedPeer, state.subject?.id, visibleIds, lifecycle) {
        if (state.selectedPeer != null && lifecycle.isAtLeast(Lifecycle.State.RESUMED)) store.acknowledgeVisible()
    }
    BackHandler(enabled = state.selectedPeer != null && sheet == null) { store.selectPeer(null) }
    BackHandler(enabled = state.viewingArchive && state.selectedPeer == null && sheet == null) { store.showArchive(false) }
    PigeonpostTheme {
        Scaffold(snackbarHost = { SnackbarHost(snackbar) }, contentWindowInsets = WindowInsets.safeDrawing) { padding ->
            Box(Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding).imePadding()) {
                when {
                    session.loading -> Loading("Opening Pigeonpost…")
                    !session.signedIn -> SignIn(session, signIn)
                    state.accountLoading && !state.accountLoaded -> Loading("Opening your inboxes…")
                    state.acting == null -> FirstInbox(state, store, model::signOut)
                    else -> BoxWithConstraints(Modifier.fillMaxSize()) {
                        val wide = maxWidth >= 840.dp
                        Row(Modifier.fillMaxSize()) {
                            if (wide || state.selectedPeer == null) {
                                InboxList(state, store, Modifier.then(if (wide) Modifier.width(340.dp) else Modifier.fillMaxWidth()).fillMaxHeight(),
                                    mailbox = { sheet = "mailbox" }, new = { sheet = "new" }, settings = { sheet = "settings" })
                            }
                            if (wide) VerticalDivider()
                            if (state.selectedPeer != null) {
                                ConversationPane(state, store, Modifier.weight(1f), wide,
                                    info = { sheet = "peer" }, newSubject = { sheet = "subject" },
                                    chooseFile = chooseFile, choosePhoto = choosePhoto, attachment = attachment, openLink = openLink)
                            } else if (wide) EmptyPane("A place for your agents", "Choose a conversation or start a new one.", Modifier.weight(1f))
                        }
                    }
                }
            }
        }
        when (sheet) {
            "mailbox" -> MailboxDialog(state, { store.switchMailbox(it); sheet = null }, { sheet = null })
            "new" -> NewConversationDialog(state, { store.selectPeer(it); sheet = null }, { sheet = null })
            "subject" -> SubjectDialog(state.actionBusy, { store.openThread(it) { sheet = null } }, { sheet = null })
            "settings" -> SettingsDialog(state, session, model.graph.fixtures, handleState, model.handles,
                openInbox = { store.switchMailbox(it); sheet = null }, dismiss = { sheet = null },
                contacts = { sheet = "contacts" }, archive = { store.showArchive(true); sheet = null }, scan = scan,
                signOut = { sheet = "signout" }, openLink = openLink)
            "contacts" -> ContactsDialog(state, { editContact = it; sheet = "contact" }, { sheet = null })
            "contact" -> ContactDialog(state, editContact, { store.saveContact(it) { sheet = "contacts" } },
                { peer -> store.removeContact(peer) { sheet = "contacts" } }, { sheet = "contacts" })
            "peer" -> PeerDialog(state, store, { sheet = null }, { contact -> editContact = contact; sheet = "contact" })
            "signout" -> Confirm("Sign out?", "Your session and local drafts will be removed from this device.", "Sign out",
                { sheet = null; model.signOut() }, { sheet = "settings" })
        }
    }
}

@Composable
private fun SignIn(state: SessionState, signIn: (String?, Boolean) -> Unit) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(Modifier.widthIn(max = 420.dp).verticalScroll(rememberScrollState()).padding(28.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Image(painterResource(R.drawable.ic_pigeonpost), "Pigeonpost", Modifier.size(104.dp).clip(RoundedCornerShape(24.dp)))
            Text("Pigeonpost", style = MaterialTheme.typography.headlineLarge, fontWeight = FontWeight.Bold)
            Text("A direct line to your agents.", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.height(20.dp))
            Button({ signIn(null, false) }, Modifier.fillMaxWidth(), enabled = !state.busy) { Text("Sign in") }
            OutlinedButton({ signIn("google", false) }, Modifier.fillMaxWidth(), enabled = !state.busy) { Text("Continue with Google") }
            OutlinedButton({ signIn("github", false) }, Modifier.fillMaxWidth(), enabled = !state.busy) { Text("Continue with GitHub") }
            OutlinedButton({ signIn("apple", false) }, Modifier.fillMaxWidth(), enabled = !state.busy) { Text("Continue with Apple") }
            TextButton({ signIn(null, true) }, enabled = !state.busy) { Text("Use a different account") }
            if (state.busy) CircularProgressIndicator(Modifier.size(24.dp))
            state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            Text("Your conversations, subjects, and contacts stay with your account.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun FirstInbox(state: InboxState, store: InboxStore, signOut: () -> Unit) {
    Column(Modifier.fillMaxSize().padding(28.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
        Image(painterResource(R.drawable.ic_pigeonpost), null, Modifier.size(96.dp))
        Text(if (state.accountLoaded) "Your first inbox" else "Couldn’t open your inboxes", style = MaterialTheme.typography.headlineSmall)
        Text(if (state.accountLoaded) "Create an inbox to start a conversation with your agents." else "Check your connection and try again.", Modifier.padding(vertical = 16.dp))
        Button({ if (state.accountLoaded) store.createFirstInbox() else store.loadAccount() }, enabled = !state.creating && !state.accountLoading) {
            Text(if (state.creating) "Creating…" else if (state.accountLoaded) "Create inbox" else "Try again")
        }
        TextButton(signOut) { Text("Sign out") }
    }
}

@Composable
private fun InboxList(state: InboxState, store: InboxStore, modifier: Modifier, mailbox: () -> Unit, new: () -> Unit, settings: () -> Unit) {
    Column(modifier) {
        Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 4.dp, top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            if (state.viewingArchive) ActionIcon("Back to inbox", Icons.AutoMirrored.Outlined.ArrowBack) { store.showArchive(false) }
            Column(Modifier.weight(1f).clip(RoundedCornerShape(8.dp)).clickable(onClick = mailbox).padding(vertical = 8.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(if (state.viewingArchive) "Archive" else "Inbox", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold)
                    Icon(Icons.Outlined.ExpandMore, null, Modifier.size(20.dp))
                }
                Text(state.acting?.key.orEmpty(), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            ActionIcon("New conversation", Icons.Outlined.Edit, new)
            ActionIcon("Settings", Icons.Outlined.Settings, settings)
        }
        OutlinedTextField(state.filter, store::filter, Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
            placeholder = { Text("Search conversations") }, leadingIcon = { Icon(Icons.Outlined.Search, null) }, singleLine = true, shape = RoundedCornerShape(16.dp))
        if (state.loading) LinearProgressIndicator(Modifier.fillMaxWidth())
        if (state.offline) Text("Offline · showing the last loaded conversations", Modifier.padding(16.dp), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelMedium)
        if (state.visible.isEmpty()) {
            EmptyPane(if (state.filter.isNotEmpty()) "No matches" else if (state.viewingArchive) "Archive is empty" else "No conversations yet",
                if (state.filter.isNotEmpty()) "Try a name or address." else "Your conversations will appear here.", Modifier.weight(1f))
        } else LazyColumn(Modifier.weight(1f)) {
            items(state.visible, key = { it.peer }) { conversation ->
                ConversationRow(conversation, conversation.peer == state.selectedPeer, { store.selectPeer(conversation.peer) })
            }
        }
        Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
            Text("${state.visible.size} conversations", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            TextButton({ store.refresh() }, enabled = !state.loading) { Icon(Icons.Outlined.Refresh, null, Modifier.size(18.dp)); Spacer(Modifier.width(6.dp)); Text("Refresh") }
        }
    }
}

@Composable
private fun ConversationRow(row: Conversation, selected: Boolean, select: () -> Unit) {
    Column(Modifier.fillMaxWidth().background(if (selected) MaterialTheme.colorScheme.primary.copy(alpha = .08f) else Color.Transparent).clickable(onClick = select)) {
        Row(Modifier.padding(horizontal = 16.dp, vertical = 16.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Avatar(row.name)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(row.name, Modifier.weight(1f), fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(shortTime(row.last), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(row.messages.lastOrNull()?.display?.text?.lineSequence()?.firstOrNull { it.isNotBlank() }?.trimStart('#', ' ', '>')?.take(140)?.ifBlank { "Attachment" }
                        ?: if (row.messages.lastOrNull()?.attachments?.isNotEmpty() == true) "Attachment" else "Start a conversation",
                        Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium, maxLines = 2, overflow = TextOverflow.Ellipsis, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    if (row.blocked) BadgeText("Blocked", MaterialTheme.colorScheme.error)
                    else if (row.held > 0) BadgeText("Held", MaterialTheme.colorScheme.tertiary)
                    if (row.unread > 0) Badge(containerColor = MaterialTheme.colorScheme.primary) { Text(row.unread.toString()) }
                }
            }
        }
        HorizontalDivider(Modifier.padding(start = 72.dp), color = MaterialTheme.colorScheme.outline)
    }
}

@Composable
private fun ConversationPane(state: InboxState, store: InboxStore, modifier: Modifier, wide: Boolean, info: () -> Unit,
    newSubject: () -> Unit, chooseFile: () -> Unit, choosePhoto: () -> Unit, attachment: (Attachment, String) -> Unit, openLink: (String) -> Unit) {
    val peer = state.selectedPeer.orEmpty()
    val subjects = state.subjects
    val subject = state.subject
    val messages = subject?.messages.orEmpty()
    var searching by rememberSaveable(peer) { mutableStateOf(false) }
    var query by rememberSaveable(peer) { mutableStateOf("") }
    var match by rememberSaveable(peer) { mutableIntStateOf(0) }
    var deleteSubject by remember { mutableStateOf(false) }
    var addMenu by remember { mutableStateOf(false) }
    val list = rememberLazyListState()
    val scope = rememberCoroutineScope()
    val hits = remember(messages, query) { if (query.isBlank()) emptyList() else messages.indices.filter { messages[it].display.text.contains(query, true) } }
    var previousSize by remember(peer, subject?.id) { mutableIntStateOf(0) }
    LaunchedEffect(peer, subject?.id, messages.lastOrNull()?.id) {
        val atEnd = previousSize == 0 || (list.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0) >= previousSize - 2
        // Schedule the jump for the next measure pass; this pane is itself subcomposed.
        if (messages.isNotEmpty() && (atEnd || messages.last().status == Delivery.SENDING)) list.requestScrollToItem(messages.lastIndex)
        previousSize = messages.size
    }
    LaunchedEffect(query, match) { if (hits.isNotEmpty()) { withFrameNanos { }; list.animateScrollToItem(hits[match.mod(hits.size)]) } }
    Column(modifier.fillMaxHeight()) {
        Row(Modifier.fillMaxWidth().padding(end = 4.dp, start = if (wide) 12.dp else 0.dp), verticalAlignment = Alignment.CenterVertically) {
            if (!wide) ActionIcon("Back to conversations", Icons.AutoMirrored.Outlined.ArrowBack) { store.selectPeer(null) }
            Column(Modifier.weight(1f).clickable(onClick = info).padding(vertical = 14.dp)) {
                Text(state.conversation?.name ?: displayName(peer), style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(peer, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            ActionIcon("Find in conversation", Icons.Outlined.Search) { searching = !searching; if (!searching) query = "" }
            ActionIcon("Conversation info", Icons.Outlined.Info, info)
        }
        Row(Modifier.fillMaxWidth().padding(end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Row(Modifier.weight(1f).horizontalScroll(rememberScrollState()).padding(start = 12.dp, end = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                subjects.forEach { item -> FilterChip(item.id == subject?.id, { store.selectSubject(item.id) }, label = { Text(item.name + if (item.unread > 0) " (${item.unread})" else "") }) }
            }
            ActionIcon("New subject", Icons.Outlined.Add, newSubject)
            if (subject?.id?.isNotEmpty() == true) ActionIcon("Delete subject", Icons.Outlined.DeleteOutline) { deleteSubject = true }
        }
        if (searching) Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(query, { query = it; match = 0 }, Modifier.weight(1f), label = { Text("Find in this subject") }, singleLine = true)
            Text(if (hits.isEmpty()) "0" else "${match.mod(hits.size) + 1}/${hits.size}", Modifier.padding(8.dp), style = MaterialTheme.typography.labelSmall)
            ActionIcon("Next match", Icons.Outlined.ArrowDownward) { if (hits.isNotEmpty()) match++ }
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outline)
        if (messages.isEmpty()) EmptyPane("Start a conversation", "Send a message in ${subject?.name ?: "General"}.", Modifier.weight(1f))
        else Box(Modifier.weight(1f)) {
            LazyColumn(state = list, modifier = Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                items(messages, key = { it.id }) { message -> MessageBubble(message, query.isNotBlank() && message.display.text.contains(query, true), store, attachment, openLink) }
            }
            if (list.canScrollForward) SmallFloatingActionButton({ scope.launch { list.animateScrollToItem(messages.lastIndex) } }, Modifier.align(Alignment.BottomEnd).padding(12.dp)) { Icon(Icons.Outlined.ArrowDownward, "Latest messages") }
        }
        if (state.attachments.isNotEmpty()) Row(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            state.attachments.forEach { file -> InputChip(false, { store.removeAttachment(file.id) }, enabled = !state.isSending,
                label = { Text(file.name, maxLines = 1) }, trailingIcon = { Icon(Icons.Outlined.Close, "Remove ${file.name}", Modifier.size(16.dp)) }) }
        }
        if (state.quota?.full == true) Text("Storage is full. Delete messages or attachments to make space.", Modifier.padding(12.dp), color = MaterialTheme.colorScheme.error)
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = 8.dp, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.Bottom) {
            Box {
                ActionIcon("Add attachment", Icons.Outlined.AttachFile) { addMenu = true }
                DropdownMenu(addMenu, { addMenu = false }) {
                    DropdownMenuItem({ Text("Choose document") }, { addMenu = false; chooseFile() })
                    DropdownMenuItem({ Text("Choose photo") }, { addMenu = false; choosePhoto() })
                }
            }
            OutlinedTextField(state.draft, store::draft, Modifier.weight(1f), placeholder = { Text("Message") }, maxLines = 5,
                enabled = !state.isSending, shape = RoundedCornerShape(20.dp))
            IconButton({ store.send() }, enabled = !state.isSending && state.quota?.full != true && (state.draft.isNotBlank() || state.attachments.isNotEmpty())) {
                if (state.isSending) CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp)
                else Icon(Icons.AutoMirrored.Outlined.Send, "Send message", tint = MaterialTheme.colorScheme.primary)
            }
        }
    }
    if (deleteSubject) Confirm("Delete subject?", "This removes the subject and its messages from this inbox. Other inboxes keep their copies.", "Delete",
        { subject?.id?.let { store.deleteThread(it) { deleteSubject = false } } }, { deleteSubject = false }, enabled = !state.actionBusy)
}

@Suppress("DEPRECATION")
@Composable
private fun MessageBubble(message: ThreadMessage, highlighted: Boolean, store: InboxStore, attachment: (Attachment, String) -> Unit, openLink: (String) -> Unit) {
    var menu by remember { mutableStateOf(false) }
    var original by remember { mutableStateOf(false) }
    var confirm by remember { mutableStateOf<String?>(null) }
    val clipboard = LocalClipboardManager.current
    Column(Modifier.fillMaxWidth(), horizontalAlignment = if (message.outgoing) Alignment.End else Alignment.Start) {
        Surface(shape = RoundedCornerShape(18.dp), color = when {
            highlighted -> MaterialTheme.colorScheme.tertiaryContainer
            message.outgoing -> MaterialTheme.colorScheme.primary.copy(alpha = .08f)
            else -> MaterialTheme.colorScheme.surfaceVariant
        }, modifier = Modifier.widthIn(max = 680.dp).combinedClickable(onClick = {}, onLongClick = { menu = true })) {
            Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                if (!message.outgoing && message.verb != null) BadgeText(if (message.autonomy == "auto") "Auto · ${message.verb}" else "Held for review · ${message.verb}",
                    if (message.autonomy == "auto") MaterialTheme.colorScheme.secondary else MaterialTheme.colorScheme.tertiary)
                message.heldBecause?.takeIf { message.autonomy == "review" }?.let { Text(it.replace('_', ' '), style = MaterialTheme.typography.labelSmall) }
                if (message.display.unattended) BadgeText(if (message.display.failed) "Unattended · failed" else "Unattended reply", MaterialTheme.colorScheme.onSurfaceVariant)
                if (message.body.isNotEmpty()) Markdown(message.display.text, openLink)
                message.attachments.forEach { file ->
                    var fileMenu by remember { mutableStateOf(false) }
                    Box {
                        OutlinedButton({ fileMenu = true }) { Icon(Icons.Outlined.Description, null, Modifier.size(18.dp)); Spacer(Modifier.width(6.dp)); Text(file.filename + " · " + bytes(file.bytes), maxLines = 2) }
                        DropdownMenu(fileMenu, { fileMenu = false }) {
                            listOf("Open" to "open", "Share" to "share", "Save a copy" to "save").forEach { (label, action) ->
                                DropdownMenuItem({ Text(label) }, { fileMenu = false; attachment(file, action) })
                            }
                        }
                    }
                }
                Row(Modifier.align(Alignment.End), verticalAlignment = Alignment.CenterVertically) {
                    Text(when (message.status) { Delivery.SENDING -> "Sending…"; Delivery.FAILED -> "Delivery unconfirmed"; Delivery.SENT -> shortTime(message.at) }, style = MaterialTheme.typography.labelSmall,
                        color = if (message.status == Delivery.FAILED) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)
                    IconButton({ menu = true }, Modifier.size(32.dp)) { Icon(Icons.Outlined.MoreHoriz, "Message actions", Modifier.size(18.dp)) }
                }
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem({ Text("Copy text") }, { menu = false; clipboard.setText(AnnotatedString(message.display.text)) })
                    DropdownMenuItem({ Text("Original message") }, { menu = false; original = true })
                    if (message.status != Delivery.SENDING) DropdownMenuItem({ Text(if (message.status == Delivery.FAILED) "Dismiss failed message" else "Delete message") }, { menu = false; confirm = "delete" })
                    if (!message.outgoing) DropdownMenuItem({ Text("Report spam") }, { menu = false; confirm = "spam" })
                }
            }
        }
    }
    if (original) PageDialog("Original message", { original = false }) { SelectionContainer { Text(message.body, style = MaterialTheme.typography.bodyMedium) } }
    if (confirm != null) Confirm(if (confirm == "spam") "Report spam?" else "Delete message?",
        if (confirm == "spam") "This reports the sender’s message as spam to the postbox." else "This removes your copy only.",
        if (confirm == "spam") "Report" else "Delete", { if (confirm == "spam") store.reportSpam(message.id) else store.deleteMessage(message.id); confirm = null }, { confirm = null })
}

@Composable fun ActionIcon(label: String, icon: ImageVector, action: () -> Unit) { IconButton(action) { Icon(icon, label, tint = MaterialTheme.colorScheme.primary) } }
@Composable fun Avatar(name: String) {
    val palette = listOf(0xFF16326B, 0xFF2563EB, 0xFF0F766E, 0xFF7C3AED, 0xFFB45309, 0xFFBE123C)
    Box(Modifier.size(44.dp).background(Color(palette[name.hashCode().mod(palette.size)]), CircleShape), contentAlignment = Alignment.Center) {
        Text(name.filter { it.isLetterOrDigit() }.take(2).uppercase(), color = Color.White, style = MaterialTheme.typography.labelLarge)
    }
}
@Composable fun BadgeText(text: String, color: Color) { Surface(color = color.copy(alpha = .08f), shape = RoundedCornerShape(5.dp)) { Text(text, Modifier.padding(horizontal = 6.dp, vertical = 3.dp), color = color, style = MaterialTheme.typography.labelSmall) } }
@Composable fun EmptyPane(title: String, detail: String, modifier: Modifier = Modifier) {
    Box(modifier.fillMaxSize().padding(28.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Icon(Icons.Outlined.Forum, null, Modifier.size(40.dp), tint = MaterialTheme.colorScheme.primary)
            Text(title, style = MaterialTheme.typography.titleMedium)
            Text(detail, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
        }
    }
}
@Composable fun Loading(label: String) { Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) { CircularProgressIndicator(); Text(label, Modifier.padding(20.dp)) } }
fun shortTime(epoch: Long): String = if (epoch <= 0) "" else {
    val today = java.time.Instant.now().atZone(java.time.ZoneId.systemDefault()).toLocalDate()
    val date = java.time.Instant.ofEpochSecond(epoch).atZone(java.time.ZoneId.systemDefault()).toLocalDate()
    (if (today == date) DateFormat.getTimeInstance(DateFormat.SHORT) else DateFormat.getDateInstance(DateFormat.SHORT)).format(Date(epoch * 1000))
}
fun bytes(value: Long): String = when { value >= 1024 * 1024 -> "%.1f MB".format(value / (1024.0 * 1024)); value >= 1024 -> "%.0f KB".format(value / 1024.0); else -> "$value B" }
