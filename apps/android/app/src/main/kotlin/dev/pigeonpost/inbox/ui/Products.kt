package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import dev.pigeonpost.core.nests.*

/** Header text of a Nest's status (build plan B3). */
fun nestStatusLabel(status: String, reason: String?): String = when (status) {
    "ready" -> "Ready"
    "working" -> "Working…"
    "needs_you" -> if (reason == "payment") "Needs you · renew its package" else "Needs you"
    "paused" -> if (reason == "over_capacity") "Paused · more Nests than your package allows" else "Paused"
    else -> when (reason) {
        "setting_up" -> "Setting up…"
        "failed" -> "Unavailable · setup failed"
        else -> "Unavailable"
    }
}

private val SERVICES = listOf(
    "Personal assistant" to "Tasks, reminders, scheduling drafts and follow-ups.",
    "AI CRM" to "Leads, pipeline stages, notes and follow-up drafts.",
    "AI HR" to "Job descriptions, candidate intake and interview briefs.",
    "Shopping assistant" to "Wish lists, comparisons and prepared recommendations.",
    "Marketing assistant" to "Brand knowledge, campaign plans and content drafts.",
)

/** Products: Buy an Inbox, Buy Nests, Services (R-35). Shown only with the `nests` feature. */
@Composable
fun ProductsDialog(store: NestStore, dismiss: () -> Unit, openHandles: () -> Unit, openLink: (String) -> Unit,
    openConversation: (String) -> Unit, pickFile: () -> Unit, exportTo: () -> Unit) {
    val state by store.state.collectAsStateWithLifecycle()
    var tab by rememberSaveable { mutableIntStateOf(1) }
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(Unit) { store.load() }
    LaunchedEffect(state.notice, state.error) { (state.error ?: state.notice)?.let { snackbar.showSnackbar(it); store.clearMessages() } }
    val detail = state.detail
    PageDialog(if (detail != null) detail.view.label.ifEmpty { "Nest" } else "Products", dismiss, back = if (detail != null) ({ store.close() }) else null) {
        if (detail != null) {
            NestDetailPage(store, state, detail, openConversation = { openConversation(it); dismiss() }, pickFile = pickFile, exportTo = exportTo)
        } else {
            val tabs = listOf("Buy an Inbox", "Buy Nests", "Services")
            ScrollableTabRow(tab, edgePadding = 0.dp, modifier = Modifier.testTag("productsTabs")) {
                tabs.forEachIndexed { i, title ->
                    Tab(tab == i, { tab = i }, text = { Text(title) },
                        modifier = Modifier.testTag("productsTab$i").semantics { selected = tab == i })
                }
            }
            if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
            when (tab) {
                0 -> InboxTab(openHandles)
                1 -> NestsTab(store, state, openLink)
                else -> ServicesTab()
            }
        }
        SnackbarHost(snackbar)
    }
}

@Composable
private fun InboxTab(openHandles: () -> Unit) {
    Text("An Inbox is a name with its own mailbox and message storage, for you or an agent.", style = MaterialTheme.typography.bodyMedium)
    Button(openHandles, Modifier.fillMaxWidth().testTag("buyInbox")) { Text("Get a handle") }
}

@Composable
private fun ServicesTab() {
    Text("Ready-made apps that run on a Nest. They are not available yet.", style = MaterialTheme.typography.bodyMedium)
    SERVICES.forEach { (name, what) ->
        ListItem(headlineContent = { Text(name) }, supportingContent = { Text(what) },
            trailingContent = { Text("Coming soon", style = MaterialTheme.typography.labelMedium) },
            modifier = Modifier.semantics(mergeDescendants = true) { stateDescription = "Coming soon, not for sale" })
    }
}

@Composable
private fun NestsTab(store: NestStore, state: NestState, openLink: (String) -> Unit) {
    // Personal or company: whose Nests are shown. Buying on Google Play is always personal.
    if (state.organizations.isNotEmpty()) Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        FilterChip(state.context == null, { store.choose(null) }, { Text("Personal") }, Modifier.testTag("contextPersonal"))
        state.organizations.forEach { org ->
            FilterChip(state.context?.id == org.id, { store.choose(org) }, { Text(org.name.ifEmpty { org.id }) })
        }
    }
    val context = state.context
    if (context != null) {
        val org = state.orgNests
        Text("${context.name.ifEmpty { "Company" }} Nests", style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
        org?.allowance?.let { Text("${it.used} of ${it.slots} Nest slots in use", style = MaterialTheme.typography.bodySmall) }
        if (org != null && org.nests.isEmpty() && !context.managesNests) Text("No company Nests you can use. An admin of the company adds them and gives people access.")
        org?.nests.orEmpty().forEach { n ->
            ListItem(headlineContent = { Text(n.label) }, supportingContent = { Text("${n.address.orEmpty()} · ${if (n.role == "owner") "Admin" else "Operator"}") },
                trailingContent = { TextButton({ store.open(n.nestId) }) { Text("Open") } })
        }
        if (context.managesNests && org != null) {
            if (org.allowance.slots == 0) Text("This company has no Nest package yet.", style = MaterialTheme.typography.bodySmall)
            else if (org.allowance.used >= org.allowance.slots) Text("Every company Nest slot is in use.", style = MaterialTheme.typography.bodySmall)
            else AddOrgNest(store, state)
        }
        return
    }
    val snapshot = state.snapshot
    val allowance = snapshot?.allowance
    snapshot?.`package`?.let { pkg ->
        Text("Your package: ${pkg.name}", style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
        Text("${allowance?.used ?: 0} of ${allowance?.slots ?: 0} Nests in use", style = MaterialTheme.typography.bodySmall)
    }
    if (allowance?.duplicates?.isNotEmpty() == true) Text(
        "You pay for more than one Nest package. Only one is used; cancel the other where you bought it to stop paying twice.",
        color = MaterialTheme.colorScheme.error)
    snapshot?.setups.orEmpty().forEach { setup ->
        Card(Modifier.fillMaxWidth()) {
            Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text("Paid · ${setup.label} is being set up", fontWeight = FontWeight.Medium)
                Text(if (setup.status == "failed") "Setup did not finish. Your payment is safe." else "This usually takes seconds.", style = MaterialTheme.typography.bodySmall)
                OutlinedButton({ store.retrySetup(setup.setupId) }, enabled = !state.busy) { Text("Retry setup") }
            }
        }
    }
    snapshot?.nests.orEmpty().forEach { n ->
        ListItem(headlineContent = { Text(n.label) },
            supportingContent = { Text(nestStatusLabel(n.status, n.statusReason)) },
            trailingContent = { TextButton({ store.open(n.nestId) }, Modifier.testTag("openNest")) { Text("Open") } },
            modifier = Modifier.semantics(mergeDescendants = true) { stateDescription = nestStatusLabel(n.status, n.statusReason) })
    }
    if (state.funding != null && state.snapshot?.setups.orEmpty().isEmpty() && allowance != null && allowance.used < allowance.slots) AddNest(store, state)
    HorizontalDivider()
    Text("Nest packages", style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
    Text("A Nest is an AI agent with its own private workspace. Allowances are shared by all Nests in a package.", style = MaterialTheme.typography.bodySmall)
    if (state.catalog?.rails?.google == false) Text("Google Play purchases are not available right now.", color = MaterialTheme.colorScheme.error)
    else if (state.playUnavailable) Text("Some packages are not available on Google Play yet.", style = MaterialTheme.typography.bodySmall)
    state.sellable.forEach { pkg -> PackageCard(store, state, pkg) }
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton({ store.load() }, enabled = !state.busy, modifier = Modifier.testTag("restoreNests")) { Text("Restore purchases") }
        if (state.funding?.source == "google") TextButton({ openLink(manageUrl(state)) }) { Text("Manage subscription") }
    }
}

/** A free slot of the person's own package. The first Nest of a purchase is set up by the server. */
@Composable
private fun AddNest(store: NestStore, state: NestState) {
    var label by rememberSaveable { mutableStateOf("") }
    OutlinedTextField(label, { label = it.take(64) }, Modifier.fillMaxWidth().testTag("addNestName"), label = { Text("Name for a new Nest") }, singleLine = true)
    Button({ store.create(label); label = "" }, enabled = state.canAddNest && label.isNotBlank(), modifier = Modifier.testTag("addNest")) { Text("Add a Nest") }
}

/** A company Nest is an agent address of the organization, so it needs a handle of its own. */
@Composable
private fun AddOrgNest(store: NestStore, state: NestState) {
    var handle by rememberSaveable(state.context?.id) { mutableStateOf("") }
    var label by rememberSaveable(state.context?.id) { mutableStateOf("") }
    HorizontalDivider()
    Text("Add a company Nest", style = MaterialTheme.typography.titleSmall, modifier = Modifier.semantics { heading() })
    OutlinedTextField(handle, { handle = it.take(64) }, Modifier.fillMaxWidth().testTag("addOrgNestHandle"), label = { Text("Handle") }, singleLine = true)
    OutlinedTextField(label, { label = it.take(64) }, Modifier.fillMaxWidth().testTag("addOrgNestName"), label = { Text("Name") }, singleLine = true)
    Text("It uses one of the company's agent addresses and one Nest slot.", style = MaterialTheme.typography.bodySmall)
    Button({ store.createForOrganization(handle, label); handle = ""; label = "" }, enabled = state.canAddOrgNest && handle.isNotBlank() && label.isNotBlank(),
        modifier = Modifier.testTag("addOrgNest")) { Text("Add company Nest") }
}

private fun manageUrl(state: NestState): String {
    val product = state.catalog?.packages?.firstOrNull { it.code == state.funding?.`package` }?.googleProductId
    return if (product == null) "https://play.google.com/store/account/subscriptions"
    else "https://play.google.com/store/account/subscriptions?sku=$product&package=dev.pigeonpost.inbox"
}

private fun allowanceText(pkg: NestPackage): String {
    val gib = pkg.storageBytes / (1024L * 1024 * 1024)
    val ai = if (pkg.model == "none") "No AI connected" else "${pkg.inputTokens / 1_000_000.0}M input + ${pkg.outputTokens / 1000}k output tokens a month"
    return "${pkg.slots} Nest${if (pkg.slots == 1) "" else "s"} · $gib GB · ${pkg.activeHours} h a month · $ai"
}

@Composable
private fun PackageCard(store: NestStore, state: NestState, pkg: NestPackage) {
    val price = state.priceFor(pkg)
    val current = state.funding?.`package` == pkg.code
    val elsewhere = state.funding != null && state.funding?.source !in setOf("google", "grant", "test")
    Card(Modifier.fillMaxWidth().testTag("package-${pkg.code}")) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(pkg.name, Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
                Text(price?.let { "${it.formatted} / month" } ?: "Not available", style = MaterialTheme.typography.labelLarge)
            }
            Text(allowanceText(pkg), style = MaterialTheme.typography.bodySmall)
            when {
                current -> Text("Your current package", style = MaterialTheme.typography.labelMedium)
                elsewhere -> Text("Your Nests are paid for on another platform. Change your package there.", style = MaterialTheme.typography.bodySmall)
                else -> Button({ store.buy(pkg) }, enabled = state.canBuy(pkg),
                    modifier = Modifier.semantics { contentDescription = "${if (state.funding != null) "Change to" else "Buy"} ${pkg.name}" }) {
                    Text(when { state.awaitingPayment == pkg.code -> "Waiting for Google Play…"; state.funding != null -> "Change to this package"; else -> "Subscribe" })
                }
            }
        }
    }
}

@Composable
private fun NestDetailPage(store: NestStore, state: NestState, detail: NestDetail, openConversation: (String) -> Unit, pickFile: () -> Unit, exportTo: () -> Unit) {
    val view = detail.view
    val manage = view.role == null || view.role == "owner"
    Text(nestStatusLabel(view.status, view.statusReason), style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.testTag("nestStatus").semantics { heading(); stateDescription = nestStatusLabel(view.status, view.statusReason) })
    view.address?.let { address ->
        Text(address, style = MaterialTheme.typography.bodySmall)
        Button({ openConversation(address) }, Modifier.fillMaxWidth()) { Text("Chat with this Nest") }
    }
    view.deleteAfter?.let { Text("Unpaid: it will be deleted after ${java.text.DateFormat.getDateInstance().format(java.util.Date(it * 1000))}. Export it first.", color = MaterialTheme.colorScheme.error) }
    if (manage) Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        if (view.state == "paused") OutlinedButton({ store.resume() }, enabled = !state.busy && (view.pausedReason == null || view.pausedReason == "owner")) { Text("Resume") }
        else if (view.state in PAUSABLE) OutlinedButton({ store.pause() }, enabled = !state.busy) { Text("Pause") }
        OutlinedButton(exportTo, enabled = !state.busy) { Text("Export") }
    }
    HorizontalDivider()
    // Approvals come first: a Nest's message in chat sends the person here.
    Text("Approvals", style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
    val pending = detail.approvals.filter { it.status == "pending" }
    if (pending.isEmpty()) Text("Nothing waits for your approval.", style = MaterialTheme.typography.bodySmall)
    pending.forEach { card -> ApprovalCard(store, state, card) }
    detail.approvals.filter { it.status != "pending" }.take(5).forEach { card ->
        Text("${card.to ?: card.tool} · ${NestRules.approvalOutcome(card.status, card.result)}", style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("approvalHistory"))
    }
    HorizontalDivider()
    // Files
    Text("Files", style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
    detail.files?.let { Text("${bytesLabel(it.usedBytes)} of ${bytesLabel(it.quotaBytes)} used", style = MaterialTheme.typography.bodySmall) }
    OutlinedButton(pickFile, enabled = !state.busy, modifier = Modifier.testTag("uploadNestFile")) { Text("Upload a file") }
    Text("PDF, Word, Excel, CSV, text and ZIP, up to 25 MB. Files are read in a separate sandbox; your Nest sees only their text.", style = MaterialTheme.typography.bodySmall)
    detail.files?.files.orEmpty().forEach { f ->
        val coverage = f.coverage.total?.let { t -> " · text in ${f.coverage.text ?: 0} of $t" }.orEmpty()
        ListItem(headlineContent = { Text(f.name) },
            supportingContent = { Text("${fileStatus(f.status)}$coverage${(f.reason ?: f.warnings.firstOrNull())?.let { " · $it" }.orEmpty()}") },
            trailingContent = { TextButton({ store.deleteFile(f.fileId) }, Modifier.semantics { contentDescription = "Delete ${f.name}" }) { Text("Delete") } })
    }
    HorizontalDivider()
    // Usage
    detail.usage?.let { u ->
        Text("This month", style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
        val noModel = (u.inputTokens.limit ?: 0L) == 0L && (u.outputTokens.limit ?: 0L) == 0L
        Text(if (noModel) "AI: none connected" else "AI: ${u.inputTokens.used ?: 0} of ${u.inputTokens.limit ?: 0} input tokens, ${u.outputTokens.used ?: 0} of ${u.outputTokens.limit ?: 0} output tokens", style = MaterialTheme.typography.bodySmall)
        val hours = u.`package`?.activeHoursPooled?.let { " of $it h shared by your package" }.orEmpty()
        Text("Running: ${(u.runningSecs ?: 0) / 3600} h ${((u.runningSecs ?: 0) % 3600) / 60} min$hours · Files: ${bytesLabel(u.filesBytes)}", style = MaterialTheme.typography.bodySmall)
    }
    if (manage) {
        HorizontalDivider()
        var label by rememberSaveable(view.nestId) { mutableStateOf(view.label) }
        OutlinedTextField(label, { label = it.take(64) }, Modifier.fillMaxWidth(), label = { Text("Name") }, singleLine = true)
        OutlinedButton({ store.rename(label) }, enabled = !state.busy && label.isNotBlank() && label != view.label) { Text("Rename") }
        var confirm by rememberSaveable(view.nestId) { mutableStateOf("") }
        Text("Delete this Nest", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.error)
        Text("Its files, conversations and workspace are destroyed. Type its name to confirm.", style = MaterialTheme.typography.bodySmall)
        OutlinedTextField(confirm, { confirm = it }, Modifier.fillMaxWidth().testTag("deleteConfirm"), label = { Text(view.label) }, singleLine = true)
        Button({ store.delete(confirm) }, enabled = !state.busy && confirm == view.label,
            colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error), modifier = Modifier.testTag("deleteNest")) { Text("Delete Nest") }
    }
}

/** One approval card: exactly what will happen, shown before anything happens. */
@Composable
private fun ApprovalCard(store: NestStore, state: NestState, card: NestApproval) {
    Card(Modifier.fillMaxWidth().testTag("approval-${card.approvalId}")) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(if (card.tool == "send_message") "Send a message to ${card.to.orEmpty()}" else card.tool, fontWeight = FontWeight.Medium)
            card.body?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
            Text(if (card.reviewable) "Your Nest asked for this. Nothing is sent unless you approve it."
                else "This version of the app cannot show this request in full, so it cannot be approved here.", style = MaterialTheme.typography.bodySmall)
            val expires = if (card.expiresAt > 0) " · expires ${java.text.DateFormat.getDateTimeInstance(java.text.DateFormat.MEDIUM, java.text.DateFormat.SHORT).format(java.util.Date(card.expiresAt * 1000))}" else ""
            Text("Check code ${card.checkCode}$expires", style = MaterialTheme.typography.labelSmall)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (card.reviewable) Button({ store.decide(card, true) }, enabled = !state.busy) { Text("Approve") }
                OutlinedButton({ store.decide(card, false) }, enabled = !state.busy) { Text("Reject") }
            }
        }
    }
}

/** States a person can pause. Setting-up, failed and already paused Nests offer no Pause. */
private val PAUSABLE = setOf("ready", "running", "stopped")

private fun fileStatus(status: String) = when (status) {
    "parsed" -> "Ready"
    "partial" -> "Partly read"
    "rejected" -> "Not readable"
    "failed" -> "Not stored"
    else -> "Uploading"
}

private fun bytesLabel(b: Long): String = when {
    b >= 1L shl 30 -> "%.1f GB".format(b / (1L shl 30).toDouble())
    b >= 1L shl 20 -> "%.1f MB".format(b / (1L shl 20).toDouble())
    b >= 1024 -> "${b / 1024} KB"
    else -> "$b B"
}

/** Above a Nest conversation: what the Nest is doing and what waits for the person (B3, B4). */
@Composable
fun NestStatusBar(store: NestStore?, peer: String, openNest: (String) -> Unit) {
    val nests = store ?: return
    val state by nests.state.collectAsStateWithLifecycle()
    val id = state.nestIdFor(peer) ?: return
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(peer, id) { lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { nests.watchChat(peer) } }
    val chat = state.chat?.takeIf { it.peer == peer }
    // An organization's Nest is known only by address until the first status arrives.
    val nest = chat?.view ?: state.snapshot?.nests?.firstOrNull { it.address == peer } ?: return
    val label = nestStatusLabel(nest.status, nest.statusReason)
    val waiting = chat?.waiting ?: 0
    val summary = when (waiting) { 0 -> label; 1 -> "$label · 1 action to approve"; else -> "$label · $waiting actions to approve" }
    Surface(Modifier.fillMaxWidth().testTag("nestStatusBar"), color = MaterialTheme.colorScheme.secondaryContainer) {
        Row(Modifier.padding(start = 14.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Row(Modifier.weight(1f).padding(vertical = 6.dp).semantics(mergeDescendants = true) { role = Role.Image; contentDescription = "Nest status: $summary" },
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (nest.status == "working") CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                Text("Nest · $summary", style = MaterialTheme.typography.labelLarge)
            }
            TextButton({ openNest(id) }, Modifier.testTag("nestStatusOpen")) { Text(if (waiting > 0) "Review" else "Details") }
        }
    }
}
