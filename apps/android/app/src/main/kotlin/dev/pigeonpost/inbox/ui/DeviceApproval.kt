package dev.pigeonpost.inbox.ui

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.pigeonpost.core.enterprise.*
import dev.pigeonpost.inbox.InboxViewModel
import kotlinx.coroutines.delay
import java.time.Instant

private class MachineConfirmation(val proof: String, val context: String, val handle: String, val number: String)

@Composable
internal fun DeviceApprovalDialog(model: InboxViewModel, s: DeviceApprovalState) {
    val store = model.deviceApproval ?: return
    key(s.contextId) {
        var input by remember { mutableStateOf(s.code?.displayValue.orEmpty()) }
        var confirmation by remember { mutableStateOf<MachineConfirmation?>(null) }
        var now by remember { mutableLongStateOf(Instant.now().epochSecond) }
        LaunchedEffect(s.code) { s.code?.let { input = it.displayValue } }
        LaunchedEffect(s.proofId) { confirmation = null }
        LaunchedEffect(store) { while (true) { delay(1000); now = Instant.now().epochSecond; store.synchronize(); store.refreshExpiry() } }
        val current = store.isCurrent(s.contextId)
        val limited = s.retryAt?.let { it > now } == true
        PageDialog("Sign in a machine", model::closeDeviceApproval, dismissEnabled = !s.busy) {
            if (s.phase == DeviceApprovalPhase.VERIFYING) {
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) { CircularProgressIndicator(Modifier.size(24.dp)); Text("Verifying sign-in…") }
            } else if (!current) {
                Text("Opening your account…", color = Pp.colors.secondary)
            } else {
                Text("Enter the code from the machine you want to sign in.", color = Pp.colors.secondary)
                OutlinedTextField(input, { value ->
                    if (value != input) { store.changeCode(s.contextId); confirmation = null; input = value.take(128) }
                }, enabled = current && !s.busy, singleLine = true, label = { Text("Machine code") }, placeholder = { Text("ABCD-EFGH") },
                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Characters, autoCorrectEnabled = false, keyboardType = KeyboardType.Ascii),
                    modifier = Modifier.fillMaxWidth().testTag("device:code"))
                Button({ store.resolve(input, s.contextId) }, enabled = current && !s.busy && input.isNotBlank() && !limited,
                    modifier = Modifier.fillMaxWidth().testTag("device:review")) { Text("Review request") }
                if (s.busy) Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) { CircularProgressIndicator(Modifier.size(24.dp)); Text("Checking request…") }
                if (current) s.preview?.let { preview ->
                    Text("Organization", style = MaterialTheme.typography.labelLarge, color = Pp.colors.secondary)
                    Text(preview.org.name)
                    SelectionContainer { Text(preview.handle, style = MaterialTheme.typography.headlineSmall, modifier = Modifier.testTag("device:handle")) }
                    if (preview.createsAgent == true && s.phase != DeviceApprovalPhase.APPROVED) Text("Approval will create this agent inbox.", color = Pp.colors.secondary)
                    preview.label?.let { Text("Machine label: $it") }
                    Text("The machine label is supplied by the requester.", color = Pp.colors.muted)
                    Text("Requested access: " + preview.capabilities.joinToString { if (it == "mailbox") "Inbox access" else it })
                    preview.ipCountry?.let { Text("Network location: $it. Compare the confirmation number to identify your machine.", color = Pp.colors.muted) }
                    val seconds = (preview.expiresAt - now).coerceAtLeast(0)
                    Text(if (seconds > 0) "Expires in ${seconds / 60}m ${seconds % 60}s" else "Request expired", color = Pp.colors.secondary)
                    if (s.phase == DeviceApprovalPhase.REVIEW) {
                        HorizontalDivider()
                        Text("Compare with your terminal", style = MaterialTheme.typography.titleMedium)
                        Text(preview.displayConfirmation, style = MaterialTheme.typography.displaySmall,
                            modifier = Modifier.testTag("device:number").semantics { contentDescription = "Confirmation number " + preview.confirmationDigits.toList().joinToString(" ") })
                        Text("Only approve if you started this request and the terminal shows the same number. Otherwise choose Deny.", color = Pp.colors.secondary)
                        if (s.challenge != null) Button({ model.verifyDeviceSignIn(s.proofId, s.contextId) }, enabled = !s.busy && !limited,
                            modifier = Modifier.fillMaxWidth().testTag("device:verify")) { Text("Verify sign-in") }
                        else MachineApprovalHold(store.canApprove(s.proofId, s.contextId) && !limited, s.proofId, s.contextId) {
                            if (store.canApprove(s.proofId, s.contextId)) confirmation = MachineConfirmation(s.proofId, s.contextId, preview.handle, preview.displayConfirmation)
                        }
                        OutlinedButton({ store.deny(s.proofId, s.contextId) }, enabled = store.canDeny(s.proofId, s.contextId) && !limited,
                            modifier = Modifier.fillMaxWidth().testTag("device:deny")) { Text("Deny") }
                    }
                }
                when (s.phase) {
                    DeviceApprovalPhase.APPROVED -> { Text("Machine approved", style = MaterialTheme.typography.titleLarge, modifier = Modifier.testTag("device:approved")); Text("The machine can now finish signing in.") }
                    DeviceApprovalPhase.DENIED -> Text("Request denied", modifier = Modifier.testTag("device:denied"))
                    DeviceApprovalPhase.UNCONFIRMED -> OutlinedButton({ store.checkStatus(s.contextId) }, enabled = current && !limited,
                        modifier = Modifier.testTag("device:check")) { Text("Check current status") }
                    else -> Unit
                }
                s.message?.let { Text(it, color = Pp.colors.heldText, modifier = Modifier.testTag("device:message")) }
                if (limited) Text("Try again in ${(s.retryAt!! - now).coerceAtLeast(1)} seconds.", color = Pp.colors.secondary)
            }
        }
        confirmation?.let { target -> Confirm("Approve this machine?", "Approve ${target.handle} only if its terminal shows ${target.number} and you started this request.", "Approve machine",
            confirm = { if (confirmation === target) { confirmation = null; store.approve(target.proof, target.context) } },
            dismiss = { if (confirmation === target) confirmation = null }, enabled = store.canApprove(target.proof, target.context)) }
    }
}

/** Pointer hold and an accessible review action both lead to the same explicit confirmation. */
@Composable
private fun MachineApprovalHold(enabled: Boolean, proof: String, context: String, confirm: () -> Unit) {
    var pressing by remember(proof, context) { mutableStateOf(false) }
    val progress by animateFloatAsState(if (pressing) 1f else 0f, tween(if (pressing) 1500 else 0), label = "Approval hold")
    Surface(shape = RoundedCornerShape(50), color = if (enabled) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.surfaceVariant,
        contentColor = if (enabled) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.fillMaxWidth().testTag("device:hold")
            .clickable(enabled = enabled, role = Role.Button, onClickLabel = "Review machine approval", onClick = confirm)
            .pointerInput(enabled, proof, context) {
                awaitEachGesture {
                    val down = awaitFirstDown(requireUnconsumed = false)
                    if (enabled) {
                        down.consume(); pressing = true
                        try {
                            val ended = withTimeoutOrNull(1500) { waitForUpOrCancellation()?.consume(); true }
                            if (ended == null) { confirm(); waitForUpOrCancellation()?.consume() }
                        } finally { pressing = false }
                    }
                }
            }) {
        Row(Modifier.padding(horizontal = 20.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.Center) {
            CircularProgressIndicator(progress = { progress }, modifier = Modifier.size(22.dp), color = LocalContentColor.current, strokeWidth = 2.dp)
            Spacer(Modifier.width(12.dp)); Text("Hold to approve")
        }
    }
}
