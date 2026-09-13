package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.pigeonpost.core.*
import java.text.DateFormat
import java.text.NumberFormat
import java.util.Currency
import java.util.Date

@Composable
fun PaidHandleSection(store: PaidHandleStore, mailboxes: List<Mailbox>, openInbox: (Mailbox) -> Unit, openLink: (String) -> Unit) {
    val state by store.state.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { store.restore() }
    Text("Handle subscriptions", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
    Text("Register up to 10 handles. Each handle has its own yearly subscription, managed through Google Play.", style = MaterialTheme.typography.bodyMedium)
    Text("${state.active.size} of 10 subscriptions active", style = MaterialTheme.typography.labelLarge)
    if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
    if (state.catalog?.available == false) Text("Handle purchases are not available yet. Please check again later.")
    state.catalog?.handles.orEmpty().filter { it.active || it.namespace != null }.forEach { handle ->
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(handle.namespace?.let { "/$it" } ?: "Paid handle · choose a name below", fontWeight = FontWeight.Medium)
            Text(when {
                !handle.active -> "Inactive · manage in Google Play"
                handle.autoRenewing -> "Renews ${DateFormat.getDateInstance().format(Date(handle.expiresAt * 1000))}"
                else -> "Access until ${DateFormat.getDateInstance().format(Date(handle.expiresAt * 1000))} · renewal cancelled"
            }, style = MaterialTheme.typography.bodySmall)
            if (handle.active) mailboxes.firstOrNull { it.handle?.startsWith("/${handle.namespace}/") == true }?.let { mailbox ->
                TextButton({ openInbox(mailbox) }) { Text("Open /${handle.namespace}") }
            }
        }
    }
    if (state.catalog?.available == true && (state.active.size < 10 || state.unassigned != null)) {
        OutlinedTextField(state.name, store::editName, Modifier.fillMaxWidth(), singleLine = true,
            label = { Text("New handle") }, placeholder = { Text("your-name") }, prefix = { Text("/") },
            enabled = !state.busy && !state.awaitingPayment,
            supportingText = { Text("1–32 letters, numbers, dots, underscores or hyphens.") })
        OutlinedButton(store::check, Modifier.fillMaxWidth(), enabled = validHandleName(state.name) && !state.busy && !state.awaitingPayment) { Text("Check availability") }
        state.availability?.let { Text(if (it.available) "/${it.name} is available" else "That handle is unavailable. Choose another.",
            color = if (it.available) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error) }
        val price = state.nextPrice
        if (state.unassigned != null) Text("You have already paid for this handle. Register its name without another charge.")
        else price?.let { Text("${it.formatted} per year for one handle. Renews automatically until cancelled in Google Play.") }
        if (state.prices.size == 10 && state.prices.map { it.currency }.distinct().size == 1) {
            val total = remember(state.prices) { runCatching {
                NumberFormat.getCurrencyInstance().apply { currency = Currency.getInstance(state.prices.first().currency) }
                    .format(state.prices.sumOf { it.micros } / 1_000_000.0)
            }.getOrNull() }
            total?.let { Text("All 10 handles: $it per year in total.", style = MaterialTheme.typography.bodySmall) }
        }
        Button(store::register, Modifier.fillMaxWidth(), enabled = state.canRegister) {
            Text(when {
                state.awaitingPayment -> "Complete purchase in Google Play…"
                state.unassigned != null -> "Register paid handle"
                price != null -> "Subscribe · ${price.formatted} / year"
                else -> "Waiting for Google Play prices"
            })
        }
    }
    state.notice?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
    state.error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium) }
    OutlinedButton(store::restore, Modifier.fillMaxWidth(), enabled = !state.busy) { Text("Restore purchases") }
    TextButton({ openLink("https://play.google.com/store/account/subscriptions?package=dev.pigeonpost.inbox") }) { Text("Manage Google Play subscriptions") }
}
