package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import dev.pigeonpost.core.*

@Composable
fun HandleSection(state: HandleState, store: HandleStore, mailboxes: List<Mailbox>, openInbox: (Mailbox) -> Unit) {
    var confirm by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(store) { store.refresh() }
    val offer = state.offer
    val namespace = offer?.namespace
    val availability = state.availability
    Text("Your handle", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
    when {
        state.loading -> LinearProgressIndicator(Modifier.fillMaxWidth())
        namespace != null -> {
            SelectionContainer { Text(namespace, style = MaterialTheme.typography.headlineSmall, fontFamily = FontFamily.Monospace) }
            offer.mailbox?.let { handle ->
                Text("Inbox", style = MaterialTheme.typography.labelMedium)
                SelectionContainer { Text(handle, fontFamily = FontFamily.Monospace) }
                mailboxes.firstOrNull { it.handle == handle }?.let { mailbox ->
                    OutlinedButton({ openInbox(mailbox) }, Modifier.fillMaxWidth()) { Text("Open this inbox") }
                }
            } ?: Button(store::repairInbox, Modifier.fillMaxWidth(), enabled = !state.busy) {
                Text(if (state.registering) "Creating your inbox…" else "Create inbox for this handle")
            }
            Text(if (offer.source == "test_preview") "Free preview handle. No payment or automatic renewal."
                else "This handle is registered to your Pigeonpost account.", style = MaterialTheme.typography.bodySmall)
            offer.expiresAt?.let { Text("Expires ${shortTime(it)}", style = MaterialTheme.typography.bodySmall) }
        }
        offer?.eligible == true -> {
            Text("Give your inboxes a name that is easy to share, like /yourname/main.", style = MaterialTheme.typography.bodyMedium)
            OutlinedTextField(state.wantedName, store::edit, Modifier.fillMaxWidth(), label = { Text("Handle name") },
                prefix = { Text("/") }, singleLine = true, enabled = !state.registering && !state.loading,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                isError = state.wantedName.isNotBlank() && !validHandleName(state.wantedName),
                supportingText = { Text("1–32 letters, numbers, dots, underscores or hyphens. No hyphen at either end.") })
            if (availability != null) {
                Text(if (availability.available) "/${availability.name} is available" else when (availability.reason) {
                    "reserved" -> "That name is reserved. Try another."
                    "taken" -> "Someone already has that name. Try another."
                    else -> "That name is unavailable. Try another."
                }, color = if (availability.available) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error)
            }
            if (state.canRegister || state.registering) {
                Button({ confirm = true }, Modifier.fillMaxWidth(), enabled = state.canRegister) {
                    Text(if (state.registering) "Registering your handle…" else "Register for free")
                }
            } else OutlinedButton(store::check, Modifier.fillMaxWidth(), enabled = !state.busy && validHandleName(state.wantedName)) {
                Text(if (state.checking) "Checking availability…" else "Check availability")
            }
            Text("One free handle per approved tester account. No payment details, charges or automatic renewal. Choose carefully: your handle can’t be renamed.",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        state.loaded -> Text("Free handle registration is available to approved testers. Sign in with your approved, verified account.", style = MaterialTheme.typography.bodyMedium)
        state.error == null -> Text("Checking your handle…")
    }
    state.error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium) }
    TextButton(store::refresh, enabled = !state.busy) { Text("Check again") }
    if (confirm) Confirm("Register /${tidyHandle(state.wantedName)}?", "This is your free preview handle. No payment is required. It can’t be renamed.",
        "Register handle", { confirm = false; store.register() }, { confirm = false }, enabled = state.canRegister)
}
