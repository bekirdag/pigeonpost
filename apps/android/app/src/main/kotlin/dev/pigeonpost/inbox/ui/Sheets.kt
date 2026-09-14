@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.BuildConfig
import dev.pigeonpost.inbox.auth.SessionState

@Composable
fun PageDialog(title: String, dismiss: () -> Unit, back: (() -> Unit)? = null, content: @Composable ColumnScope.() -> Unit) {
    Dialog({ (back ?: dismiss)() }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.padding(16.dp).widthIn(max = 600.dp).fillMaxWidth().fillMaxHeight(.92f), shape = MaterialTheme.shapes.extraLarge) {
            Column {
                Row(Modifier.fillMaxWidth().padding(start = 24.dp, end = 8.dp, top = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                    if (back != null) ActionIcon("Back", Icons.AutoMirrored.Outlined.ArrowBack, back)
                    Text(title, Modifier.weight(1f), style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.Bold)
                    ActionIcon("Close $title", Icons.Outlined.Close, dismiss)
                }
                key(title) {
                    Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(24.dp), verticalArrangement = Arrangement.spacedBy(16.dp), content = content)
                }
            }
        }
    }
}
@Composable
fun Confirm(title: String, detail: String, action: String, confirm: () -> Unit, dismiss: () -> Unit, enabled: Boolean = true) {
    AlertDialog(onDismissRequest = dismiss, title = { Text(title) }, text = { Text(detail) },
        confirmButton = { TextButton(confirm, enabled = enabled) { Text(action) } }, dismissButton = { TextButton(dismiss) { Text("Cancel") } })
}

@Composable
fun MailboxDialog(state: InboxState, select: (Mailbox) -> Unit, dismiss: () -> Unit) {
    PageDialog("Your inboxes", dismiss) {
        state.mailboxes.forEach { mailbox ->
            ListItem(headlineContent = { Text(mailbox.name) }, supportingContent = { Text(mailbox.key) }, leadingContent = { Avatar(mailbox.name) },
                trailingContent = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (mailbox.address == state.acting?.address) Icon(Icons.Outlined.Check, "Selected", Modifier.size(18.dp))
                        CopyAddressButton(mailbox.key)
                    }
                }, modifier = Modifier.clickable { select(mailbox) })
        }
    }
}
@Composable
fun NewConversationDialog(state: InboxState, select: (String) -> Unit, dismiss: () -> Unit) {
    var address by rememberSaveable { mutableStateOf("") }
    PageDialog("New conversation", dismiss) {
        OutlinedTextField(address, { address = it.take(512) }, Modifier.fillMaxWidth(), label = { Text("Pigeonpost address") }, placeholder = { Text("/your-team/agent") }, singleLine = true,
            isError = address.isNotEmpty() && !validAddress(address), supportingText = { Text("Enter a handle or an inbox address beginning with /.") })
        Button({ select(address) }, enabled = validAddress(address), modifier = Modifier.fillMaxWidth()) { Text("Open conversation") }
        val own = state.mailboxes.filter { it.address != state.acting?.address }
        if (own.isNotEmpty()) {
            Text("Your agents", style = MaterialTheme.typography.titleSmall)
            own.forEach { mailbox -> ListItem(headlineContent = { Text(mailbox.name) }, supportingContent = { Text(mailbox.key) }, leadingContent = { Avatar(mailbox.name) }, modifier = Modifier.clickable { select(mailbox.key) }) }
        }
        val contacts = state.contacts.filter { !it.wildcard && it.admission != "block" }
        if (contacts.isNotEmpty()) {
            Text("Contacts", style = MaterialTheme.typography.titleSmall)
            contacts.forEach { contact -> ListItem(headlineContent = { Text(contact.alias ?: displayName(contact.peer)) }, supportingContent = { Text(contact.peer) }, modifier = Modifier.clickable { select(contact.peer) }) }
        }
    }
}
@Composable
fun SubjectDialog(busy: Boolean, create: (String) -> Unit, dismiss: () -> Unit) {
    var title by rememberSaveable { mutableStateOf("") }
    AlertDialog(onDismissRequest = dismiss, title = { Text("New subject") },
        text = { OutlinedTextField(title, { title = it.take(160) }, label = { Text("Subject") }, singleLine = true) },
        confirmButton = { TextButton({ create(title) }, enabled = title.isNotBlank() && !busy) { Text(if (busy) "Creating…" else "Create") } },
        dismissButton = { TextButton(dismiss) { Text("Cancel") } })
}

private enum class SettingsPage(val title: String) {
    ROOT("Settings"), ACCOUNT("Account"), HANDLES("Handles"), PURCHASES("Get a handle"),
    PREVIEW("Tester registration"), INBOX("Inbox and storage"), HELP("Help and about")
}

@Composable
private fun SettingsRow(title: String, detail: String, icon: ImageVector, click: () -> Unit) {
    Surface(shape = MaterialTheme.shapes.medium, tonalElevation = 1.dp) {
        ListItem(headlineContent = { Text(title, fontWeight = FontWeight.Medium) },
            supportingContent = { Text(detail) },
            leadingContent = { Icon(icon, null, tint = MaterialTheme.colorScheme.primary) },
            trailingContent = { Icon(Icons.Outlined.ChevronRight, null) },
            modifier = Modifier.fillMaxWidth().clickable(onClick = click))
    }
}

@Composable
fun SettingsDialog(state: InboxState, session: SessionState, fixtures: Boolean, handleState: HandleState, handles: HandleStore,
    paidHandles: PaidHandleStore? = null,
    accountHandles: AccountHandleStore? = null, refreshMailboxes: () -> Unit = {},
    openInbox: (Mailbox) -> Unit, dismiss: () -> Unit, contacts: () -> Unit,
    archive: () -> Unit, scan: () -> Unit, signOut: () -> Unit, openLink: (String) -> Unit) {
    var pageName by rememberSaveable { mutableStateOf(SettingsPage.ROOT.name) }
    val page = SettingsPage.valueOf(pageName)
    fun navigate(next: SettingsPage) { pageName = next.name }
    val back: (() -> Unit)? = if (page == SettingsPage.ROOT) null else ({
        navigate(if (page == SettingsPage.PURCHASES || page == SettingsPage.PREVIEW) SettingsPage.HANDLES else SettingsPage.ROOT)
    })
    PageDialog(page.title, dismiss, back = back) {
        when (page) {
            SettingsPage.ROOT -> {
                SettingsRow("Account", session.username ?: "Your profile and devices", Icons.Outlined.AccountCircle) { navigate(SettingsPage.ACCOUNT) }
                SettingsRow("Handles", "Your names and subscriptions", Icons.Outlined.AlternateEmail) { navigate(SettingsPage.HANDLES) }
                SettingsRow("Inbox and storage", "Storage and archived conversations", Icons.Outlined.Inbox) { navigate(SettingsPage.INBOX) }
                SettingsRow("Contacts and permissions", "Senders you know and trust", Icons.Outlined.PeopleOutline, contacts)
                SettingsRow("Help and about", "Support, privacy and app information", Icons.Outlined.HelpOutline) { navigate(SettingsPage.HELP) }
            }
            SettingsPage.ACCOUNT -> {
                Text("Signed in as", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Text(session.username ?: "Your account", style = MaterialTheme.typography.titleLarge)
                Text("Current inbox", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
                state.acting?.let { PostAddressRow(it.key) }
                HorizontalDivider()
                SettingsRow("Scan sign-in code", "Sign in on another device", Icons.Outlined.QrCodeScanner, scan)
                HorizontalDivider()
                OutlinedButton(signOut, Modifier.fillMaxWidth()) { Text("Sign out") }
                TextButton({ openLink("https://pigeonpost.dev/delete-account.html") }) { Text("Delete account", color = MaterialTheme.colorScheme.error) }
            }
            SettingsPage.HANDLES -> {
                if (paidHandles != null) SettingsRow("Get a handle", "Register a name or restore purchases", Icons.Outlined.AddCircleOutline) { navigate(SettingsPage.PURCHASES) }
                SettingsRow("Tester registration", "Complimentary names for approved testers", Icons.Outlined.CardGiftcard) { navigate(SettingsPage.PREVIEW) }
        accountHandles?.let { owned ->
            val holdings by owned.state.collectAsStateWithLifecycle()
            LaunchedEffect(owned) { owned.refresh() }
            Text("Your handles", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            Text("Names belong to your Pigeonpost account across mobile, desktop and web.", style = MaterialTheme.typography.bodySmall)
            if (holdings.loading) LinearProgressIndicator(Modifier.fillMaxWidth())
            holdings.handles.forEach { handle ->
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    SelectionContainer { Text(handle.name, fontWeight = FontWeight.Medium) }
                    Text("${if (handle.active) "Active" else "Expired"} · ${handle.provider}", style = MaterialTheme.typography.bodySmall)
                    handle.expiresAt?.let { expires ->
                        Text("${if (handle.active) "Paid through" else "Expired on"} ${java.text.DateFormat.getDateInstance().format(java.util.Date(expires * 1000))}", style = MaterialTheme.typography.bodySmall)
                    }
                    if (handle.active) state.mailboxes.firstOrNull { it.handle?.startsWith(handle.name + "/") == true }?.let { mailbox ->
                        TextButton({ openInbox(mailbox) }) { Text("Open ${handle.name}") }
                    }
                }
            }
            if (holdings.loaded && holdings.handles.isEmpty() && holdings.error == null) Text("No handles on this Pigeonpost account yet.")
            holdings.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            Text("Expired names need renewal through their original provider.", style = MaterialTheme.typography.bodySmall)
            OutlinedButton({ owned.refresh(); refreshMailboxes() }, Modifier.fillMaxWidth(), enabled = !holdings.loading) { Text("Refresh account handles") }
        }
            }
            SettingsPage.PURCHASES -> paidHandles?.let { PaidHandleSection(it, state.mailboxes, openInbox, openLink) }
            SettingsPage.PREVIEW -> HandleSection(handleState, handles, state.mailboxes, openInbox)
            SettingsPage.INBOX -> {
                Text("Storage", style = MaterialTheme.typography.titleMedium)
                state.quota?.let { quota ->
                    LinearProgressIndicator(progress = { quota.fraction }, modifier = Modifier.fillMaxWidth(), color = if (quota.warning) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary)
                    Text("${bytes(quota.usedBytes)} of ${bytes(quota.limitBytes)} · ${quota.tier}", style = MaterialTheme.typography.bodyMedium)
                } ?: Text("Storage information is unavailable.")
                SettingsRow("Archived conversations", "Saved conversations, out of the way", Icons.Outlined.Archive, archive)
                HorizontalDivider()
                Text("Notifications", style = MaterialTheme.typography.titleMedium)
                Text("Conversations update while the app is open. Background notifications are not available yet.", style = MaterialTheme.typography.bodyMedium)
            }
            SettingsPage.HELP -> {
                TextButton({ openLink("https://pigeonpost.dev/app-support.html") }) { Text("Contact support") }
                TextButton({ openLink("https://pigeonpost.dev/app-privacy.html") }) { Text("Privacy policy") }
                TextButton({ openLink("https://pigeonpost.dev/app-terms.html") }) { Text("Terms of service") }
                HorizontalDivider()
                Text("Pigeonpost ${BuildConfig.VERSION_NAME}", style = MaterialTheme.typography.titleMedium)
                Text("Wodo Teknoloji A.Ş.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                if (fixtures) Text("Development fixtures", style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

@Composable
fun ContactsDialog(state: InboxState, edit: (Contact?) -> Unit, dismiss: () -> Unit, back: (() -> Unit)? = null) {
    PageDialog("Contacts and permissions", dismiss, back = back) {
        Text("Knowing a sender and letting their agent requests run automatically are separate choices.", style = MaterialTheme.typography.bodyMedium)
        Button({ edit(null) }, Modifier.fillMaxWidth()) { Text("Add contact") }
        if (state.contacts.isEmpty()) Text("No contacts yet.")
        state.contacts.sortedBy { it.peer }.forEach { contact ->
            ListItem(headlineContent = { Text(contact.alias ?: contact.peer) }, supportingContent = { Text(contact.peer + "\n" + when {
                contact.admission == "block" -> "Blocked"
                contact.autonomy == "auto" -> "Known · selected requests may run automatically"
                else -> "Known · requests need review"
            }) }, modifier = Modifier.clickable { edit(contact) })
        }
    }
}

@Composable
fun ContactDialog(state: InboxState, original: Contact?, save: (Contact) -> Unit, remove: (String) -> Unit, dismiss: () -> Unit) {
    var peer by rememberSaveable(original?.peer) { mutableStateOf(original?.peer.orEmpty()) }
    var alias by rememberSaveable(original?.peer) { mutableStateOf(original?.alias.orEmpty()) }
    var blocked by rememberSaveable(original?.peer) { mutableStateOf(original?.admission == "block") }
    var auto by rememberSaveable(original?.peer) { mutableStateOf(original?.autonomy == "auto") }
    var verbs by remember(original?.peer) { mutableStateOf(original?.allowedVerbs.orEmpty().intersect(state.vocabulary.safeGrantable.toSet())) }
    var confirm by remember { mutableStateOf<String?>(null) }
    fun contact() = Contact(peer, alias.trim().ifBlank { null }, if (blocked) "block" else "allow", if (auto && !blocked) "auto" else "review", if (auto && !blocked) verbs.toList() else emptyList())
    PageDialog(if (original == null) "Add contact" else "Edit contact", dismiss) {
        OutlinedTextField(peer, { peer = it.take(512) }, Modifier.fillMaxWidth(), label = { Text("Address or /namespace/*") }, enabled = original == null, singleLine = true,
            isError = peer.isNotEmpty() && !validAddress(peer, wildcard = true))
        OutlinedTextField(alias, { alias = it.take(120) }, Modifier.fillMaxWidth(), label = { Text("Display name (optional)") }, singleLine = true)
        ToggleRow("Block sender", "Reject messages from this sender.", blocked) { blocked = it }
        if (!blocked) {
            ToggleRow("Allow selected requests automatically", "Only the permissions selected below are granted.", auto) { auto = it }
            if (auto) {
                if (state.vocabulary.safeGrantable.isEmpty()) Text("No automatic permissions are available from this postbox.")
                state.vocabulary.safeGrantable.forEach { verb ->
                    Row(Modifier.fillMaxWidth().clickable { verbs = if (verb in verbs) verbs - verb else verbs + verb }, verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(verb in verbs, { checked -> verbs = if (checked) verbs + verb else verbs - verb }); Text(verb.replace('_', ' '))
                    }
                }
            }
        }
        Text("Saving a contact does not approve a held request. Permissions apply only to requests the server allows.", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Button({
            val change = contact()
            val widens = change.autonomy == "auto" && (original?.autonomy != "auto" || change.allowedVerbs.orEmpty().any { it !in original.allowedVerbs.orEmpty() })
            if (widens || blocked && original?.admission != "block") confirm = "save" else save(change)
        }, enabled = validAddress(peer, wildcard = true) && !state.actionBusy, modifier = Modifier.fillMaxWidth()) { Text(if (state.actionBusy) "Saving…" else "Save contact") }
        if (original != null) TextButton({ confirm = "remove" }, enabled = !state.actionBusy) { Text("Remove contact", color = MaterialTheme.colorScheme.error) }
    }
    if (confirm != null) Confirm(if (confirm == "remove") "Remove contact?" else if (blocked) "Block sender?" else "Grant selected permissions?",
        if (confirm == "remove") "The sender will fall back to any matching namespace rule or your inbox’s default policy."
        else if (blocked) "New messages from $peer will be blocked."
        else "Requests from $peer may run automatically for: ${verbs.joinToString { it.replace('_', ' ') }}.",
        if (confirm == "remove") "Remove" else "Confirm", { if (confirm == "remove") remove(peer) else save(contact()); confirm = null }, { confirm = null })
}

@Composable
fun PeerDialog(state: InboxState, store: InboxStore, dismiss: () -> Unit, edit: (Contact) -> Unit) {
    val peer = state.selectedPeer ?: return
    val contact = Conversations.contact(peer, state.contacts)
    val exact = state.contacts.firstOrNull { it.peer == peer }
    val full = contact?.autonomy == "auto" && state.vocabulary.safeGrantable.isNotEmpty() && contact.allowedVerbs.orEmpty().containsAll(state.vocabulary.safeGrantable)
    var confirmation by remember { mutableStateOf<String?>(null) }
    PageDialog("Conversation info", dismiss) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Avatar(state.conversation?.name ?: displayName(peer)); Text(state.conversation?.name ?: displayName(peer), style = MaterialTheme.typography.titleLarge)
        }
        SelectionContainer { Text(peer) }
        state.conversation?.identity?.let { own -> Button({ store.switchMailbox(own); dismiss() }) { Text("Open this inbox") } }
        if (contact?.wildcard == true) Text("Uses the ${contact.peer} contact rule. Changes here apply only to this sender.", style = MaterialTheme.typography.bodySmall)
        ToggleRow("Known sender", "Remember this sender without granting new automatic permissions.", contact?.admission == "allow", enabled = !state.actionBusy) { checked ->
            if (checked) store.markKnown(peer) else if (exact != null) confirmation = "forget" else edit(Contact(peer, contact?.alias, "allow", "review", emptyList()))
        }
        ToggleRow("Full available permissions", "Allow every request type the postbox makes available for automatic handling.", full, enabled = !state.actionBusy && contact?.admission != "block") { checked ->
            if (checked) confirmation = "full" else store.fullPermissions(peer, false)
        }
        OutlinedButton({ edit(Contact(peer, contact?.alias, contact?.admission ?: "allow", contact?.autonomy ?: "review", contact?.allowedVerbs)) }, Modifier.fillMaxWidth()) { Text("Choose permissions") }
        Text("Reading a held request marks it read. It does not grant permission to execute it.", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        val standing = state.conversation?.messages?.lastOrNull { !it.outgoing }
        if (standing?.standing != null) Text("Standing: ${standing.standing}" + (standing.tier?.let { " · $it" } ?: ""), style = MaterialTheme.typography.bodyMedium)
        HorizontalDivider()
        OutlinedButton({ store.archive(peer, peer !in state.archived); dismiss() }, Modifier.fillMaxWidth(), enabled = !state.actionBusy) { Text(if (peer in state.archived) "Unarchive conversation" else "Archive conversation") }
        TextButton({ confirmation = if (contact?.admission == "block") "unblock" else "block" }, enabled = !state.actionBusy) { Text(if (contact?.admission == "block") "Unblock sender" else "Block sender", color = MaterialTheme.colorScheme.error) }
    }
    confirmation?.let { action -> Confirm(when (action) { "full" -> "Grant available permissions?"; "forget" -> "Remove contact?"; "unblock" -> "Unblock sender?"; else -> "Block sender?" },
        when (action) {
            "full" -> "Requests from $peer may run automatically for: ${state.vocabulary.safeGrantable.joinToString { it.replace('_', ' ') }}. Requests the server always holds still need review."
            "forget" -> "This removes the exact contact. A matching namespace rule may still apply."
            "unblock" -> "Messages will be accepted. Requests will require review."
            else -> "New messages from $peer will be blocked."
        }, "Confirm", {
            when (action) { "full" -> store.fullPermissions(peer, true); "forget" -> store.removeContact(peer); "unblock" -> store.saveContact(Contact(peer, contact?.alias)); else -> store.block(peer) }
            confirmation = null
        }, { confirmation = null }) }
}

@Composable
private fun ToggleRow(title: String, detail: String, checked: Boolean, enabled: Boolean = true, change: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Column(Modifier.weight(1f)) { Text(title, style = MaterialTheme.typography.titleSmall); Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        Switch(checked, change, enabled = enabled)
    }
}
