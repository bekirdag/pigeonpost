package dev.pigeonpost.inbox.ui

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.autofill.ContentType
import androidx.compose.ui.semantics.contentType
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.pigeonpost.core.*
import dev.pigeonpost.core.contacts.*
import dev.pigeonpost.inbox.PhoneContacts
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

val LocalPeople = staticCompositionLocalOf { PeopleState() }
val LocalPeopleStore = staticCompositionLocalOf<PeopleStore?> { null }
@Composable fun peerLabel(peer: String, legacyAlias: String? = null, fallback: String = displayName(peer)) = LocalPeople.current.label(peer, legacyAlias, fallback)

@Composable
fun PeopleSettings(mode: String, store: PeopleStore, openPeer: (String) -> Unit) {
    val state by store.state.collectAsStateWithLifecycle()
    val context = state.context
    val android = LocalContext.current
    val scope = rememberCoroutineScope()
    var phone by remember(context) { mutableStateOf("") }
    var region by remember(context) { mutableStateOf(PhoneContacts.defaultRegion(android)) }
    var code by remember(context, state.challenge?.id) { mutableStateOf("") }
    var discoverable by remember(context) { mutableStateOf(false) }
    var disclosure by remember(context) { mutableStateOf(false) }
    var pendingPermission by remember { mutableStateOf<Pair<String, String>?>(null) }
    var reading by remember(context) { mutableStateOf(false) }
    var permissionDenied by remember(context) { mutableStateOf(false) }
    var localError by remember(context) { mutableStateOf<String?>(null) }
    var unlink by remember(context) { mutableStateOf<VerifiedPhone?>(null) }
    val name = state.ownName
    var display by remember(context, name) { mutableStateOf(name?.name.orEmpty()) }
    var visibility by remember(context, name) { mutableStateOf(name?.visibility ?: "known") }
    LaunchedEffect(context, mode) { when (mode) { "phone" -> store.loadPhones(context); "name" -> store.loadName(context) } }
    val read: (String, String) -> Unit = { captured, country ->
        if (store.isCurrent(captured)) scope.launch {
            reading = true; localError = null
            try {
                val contacts = PhoneContacts.read(android, country)
                if (store.isCurrent(captured)) {
                    if (contacts.isEmpty()) localError = "No readable phone numbers were found. Check the country and try again."
                    else store.discover(captured, contacts)
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { if (store.isCurrent(captured)) localError = "Contacts could not be read. You can allow access in Android settings and try again." }
            finally { if (store.isCurrent(captured)) reading = false }
        }
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        val target = pendingPermission; pendingPermission = null
        if (target != null && store.isCurrent(target.first)) {
            permissionDenied = !granted
            if (granted) read(target.first, target.second)
            else localError = "Contact access was not allowed. Nothing was imported. You can enable Contacts in Android settings when you want to try again."
        }
    }
    val enabled = !state.busy && !reading
    when (mode) {
        "phone" -> {
            Text("Associate a phone number with this inbox. Your number stays private; people can find this inbox only if you turn on discovery.")
            state.phones.forEach { number ->
                Text(number.masked, style = MaterialTheme.typography.titleMedium)
                Text(number.identity, style = MaterialTheme.typography.bodySmall)
                ToggleRow("Let people who have my number find me", "Only people who check this number can discover this inbox.", number.discoverable, enabled) { store.discoverable(context, number.id, it) }
                TextButton({ unlink = number }, enabled = enabled) { Text("Unlink number") }
                HorizontalDivider()
            }
            OutlinedTextField(region, { region = it.uppercase().take(2) }, label = { Text("Country (TR, US, GB…)") }, singleLine = true, enabled = enabled)
            OutlinedTextField(phone, { phone = it.take(128) }, label = { Text("Phone number") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Phone), singleLine = true, enabled = enabled, modifier = Modifier.testTag("phoneNumber").semantics { contentType = ContentType.PhoneNumber })
            Button({
                val normalized = PhoneContacts.normalize(phone, region)
                if (normalized == null) localError = "Check the number and its country code." else { localError = null; store.sendCode(context, normalized) }
            }, enabled = enabled && phone.isNotBlank()) { Text(if (state.challenge == null) "Send verification code" else "Send a new code") }
            state.challenge?.let {
                OutlinedTextField(code, { code = it.filter { c -> c in '0'..'9' }.take(6) }, label = { Text("6-digit code") }, singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword), enabled = enabled, modifier = Modifier.testTag("phoneCode").semantics { contentType = ContentType.SmsOtpCode })
                ToggleRow("Let people who have my number find me", "Optional. You can change this later.", discoverable, enabled) { discoverable = it }
                Button({ store.verify(context, code, discoverable) }, enabled = enabled && code.length == 6) { Text("Verify number") }
                Text("Codes expire after 10 minutes. Wait at least a minute before requesting another.", style = MaterialTheme.typography.bodySmall)
            }
        }
        "name" -> {
            Text("Choose what people call this inbox. Display names and profile pictures are free. A display name is not a unique handle or a verified identity.")
            OutlinedTextField(display, { display = it.take(100) }, label = { Text("Display name") }, singleLine = true, enabled = enabled && state.nameLoaded, modifier = Modifier.testTag("displayName"))
            listOf("known" to "People I know", "everyone" to "Everyone", "nobody" to "Only me").forEach { (value, label) ->
                Row { RadioButton(visibility == value, { visibility = value }, enabled = enabled); TextButton({ visibility = value }, enabled = enabled) { Text(label) } }
            }
            Button({
                if (runCatching { PeopleInput.name(display) }.isFailure) localError = "Use 1–40 characters without addresses, invisible characters or reserved names."
                else { localError = null; store.saveName(context, display, visibility) }
            }, enabled = enabled && state.nameLoaded && display.isNotBlank()) { Text("Save name") }
            if (name != null) TextButton({ store.removeName(context) }, enabled = enabled) { Text("Remove name") }
        }
        "find" -> {
            Text("Find people who verified their number and chose to be found. No match does not prove someone has no Pigeonpost account.")
            OutlinedTextField(region, { region = it.uppercase().take(2) }, label = { Text("Country for local numbers (TR, US, GB…)") }, singleLine = true, enabled = enabled)
            Button({ disclosure = true }, enabled = enabled && region.length == 2, modifier = Modifier.testTag("findPeople")) { Text("Find people I know") }
            if (permissionDenied) TextButton({
                android.startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${android.packageName}")))
            }) { Text("Open contact permission settings") }
            val matched = state.imported.filter { it.hash in state.matches }
            val unmatched = state.imported.filter { it.hash in state.checked && it.hash !in state.matches }
            LazyColumn(Modifier.fillMaxWidth().heightIn(max = 420.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
              items(matched, key = { it.hash }) { contact ->
                val match = state.matches.getValue(contact.hash)
                Column {
                HorizontalDivider()
                Text(contact.name, style = MaterialTheme.typography.titleMedium)
                match.name?.let { Text("~$it", color = MaterialTheme.colorScheme.onSurfaceVariant) }
                Text(match.handle ?: match.address, style = MaterialTheme.typography.bodySmall)
                TextButton({ if (store.isCurrent(context)) openPeer(match.handle ?: match.address) }, enabled = enabled) { Text("Message") }
                }
            }
            if (unmatched.isNotEmpty()) item { Text("No match found", style = MaterialTheme.typography.titleMedium) }
            items(unmatched, key = { it.hash }) { contact ->
                Column {
                Text(contact.name); Text(contact.phone, style = MaterialTheme.typography.bodySmall)
                if (Features.SMS_INVITES in state.features) TextButton({ store.reviewInvite(context, contact.hash) }, enabled = enabled && contact.hash !in state.invited) {
                    Text(if (contact.hash in state.invited) "Invitation checked" else "Invite by SMS")
                }
                }
            }
            }
            if (state.imported.isNotEmpty()) TextButton({ store.clearImport(context) }, enabled = enabled) { Text("Clear imported list") }
        }
    }
    if (state.busy || reading) LinearProgressIndicator(Modifier.fillMaxWidth())
    state.message?.let { Text(it, modifier = Modifier.testTag("peopleStatus")) }
    localError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    if (disclosure) AlertDialog(onDismissRequest = { disclosure = false }, title = { Text("Find your contacts on Pigeonpost?") }, text = {
        Text("Pigeonpost reads phone numbers on this device and sends hashes to check for people who allow discovery. Phone hashes can still be guessed; they are not anonymous. Contact names, photos and notes stay on this device. Temporary abuse-prevention marks are kept for 7 days. Up to 5,000 numbers can be read, with a smaller daily checking allowance. Nothing is sent by SMS unless you confirm a separate invitation.")
    }, confirmButton = { TextButton({
        disclosure = false
        if (store.isCurrent(context)) {
            if (ContextCompat.checkSelfPermission(android, Manifest.permission.READ_CONTACTS) == PackageManager.PERMISSION_GRANTED) read(context, region)
            else { pendingPermission = context to region; permission.launch(Manifest.permission.READ_CONTACTS) }
        }
    }) { Text("Continue") } }, dismissButton = { TextButton({ disclosure = false }) { Text("Cancel") } })
    state.invite?.let { review -> AlertDialog(onDismissRequest = store::cancelInvite, title = { Text("Send one SMS invitation?") }, text = {
        Text("Invite ${review.contact.name} at ${review.contact.phone} to join Pigeonpost? The message includes your display name, an invitation link and opt-out instructions. They will receive a text message from Pigeonpost.")
    }, confirmButton = { TextButton({ store.invite(review) }, enabled = enabled) { Text("Send invitation") } }, dismissButton = { TextButton(store::cancelInvite) { Text("Cancel") } }) }
    unlink?.let { number -> AlertDialog(onDismissRequest = { unlink = null }, title = { Text("Unlink this number?") }, text = { Text("${number.masked} will no longer identify this inbox in contact discovery.") },
        confirmButton = { TextButton({ store.unlink(context, number.id); unlink = null }) { Text("Unlink") } }, dismissButton = { TextButton({ unlink = null }) { Text("Cancel") } }) }
}

@Composable
fun DisplayNamePrompt(store: PeopleStore) {
    val state by store.state.collectAsStateWithLifecycle()
    val context = state.context
    var name by remember(context) { mutableStateOf("") }
    var error by remember(context) { mutableStateOf<String?>(null) }
    AlertDialog(onDismissRequest = { store.dismissNamePrompt(context) }, title = { Text("What should people call you?") }, text = {
        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text("Choose a free display name for this inbox. People you write to see ~Name beside your address. You can change it later in Settings.")
            OutlinedTextField(name, { name = it.take(100) }, label = { Text("Display name") }, singleLine = true, enabled = !state.busy,
                modifier = Modifier.testTag("onboardingDisplayName").semantics { contentType = ContentType.PersonFullName })
            (error ?: state.message)?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
    }, confirmButton = { TextButton({
        if (runCatching { PeopleInput.name(name) }.isFailure) error = "Use 1–40 characters without addresses, invisible characters or reserved names."
        else { error = null; store.saveName(context, name, "known") }
    }, enabled = !state.busy && name.isNotBlank()) { Text("Save name") } }, dismissButton = {
        TextButton({ store.dismissNamePrompt(context) }, enabled = !state.busy) { Text("Skip for now") }
    })
}
