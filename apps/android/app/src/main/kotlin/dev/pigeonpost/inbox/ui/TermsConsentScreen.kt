package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import dev.pigeonpost.inbox.AppPolicy
import dev.pigeonpost.inbox.auth.SessionState

@Composable
fun TermsConsentScreen(state: SessionState, accept: () -> Unit, signOut: () -> Unit, openLink: (String) -> Unit) {
    var agreed by rememberSaveable { mutableStateOf(false) }
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(Modifier.widthIn(max = 480.dp).fillMaxWidth().verticalScroll(rememberScrollState()).padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Before you start", style = MaterialTheme.typography.headlineSmall)
            Text("Read the terms before using Pigeonpost. Do not send spam, threats, harassment, hateful or sexually explicit content, or content promoting illegal activity.")
            Text("You can report a message from its actions menu and block a sender from Conversation info.", style = MaterialTheme.typography.bodyMedium)
            TextButton({ openLink(AppPolicy.TERMS_URL) }) { Text("Terms of service") }
            TextButton({ openLink(AppPolicy.PRIVACY_URL) }) { Text("Privacy policy") }
            Row(Modifier.fillMaxWidth().toggleable(agreed, enabled = !state.busy, role = Role.Checkbox, onValueChange = { agreed = it }),
                verticalAlignment = Alignment.CenterVertically) {
                Checkbox(agreed, onCheckedChange = null, enabled = !state.busy)
                Text("I agree to the Terms of service", Modifier.weight(1f))
            }
            state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            if (state.busy) CircularProgressIndicator(Modifier.size(24.dp))
            Button(accept, enabled = agreed && !state.busy, modifier = Modifier.fillMaxWidth()) { Text("Agree and continue") }
            TextButton(signOut, enabled = !state.busy) { Text("Sign out") }
            TextButton({ openLink(AppPolicy.SUPPORT_URL) }) { Text("Support") }
            TextButton({ openLink(AppPolicy.DELETE_URL) }) { Text("Delete account") }
        }
    }
}
