package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.pigeonpost.core.InboxState
import dev.pigeonpost.core.enterprise.*
import dev.pigeonpost.core.groups.GroupsState
import dev.pigeonpost.inbox.InboxViewModel
import dev.pigeonpost.inbox.calls.CallHost
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow

private enum class RoomRoute { DIRECTORY, PREVIEW, CREATE }
private class RoomConfirmation(val context: String, val kind: String, val address: String? = null, val label: String? = null, val organization: String? = null)

@Composable
internal fun OrganizationRoomsDialog(model: InboxViewModel, inbox: InboxState, s: OrganizationRoomsState, groups: GroupsState,
    preparing: Boolean, authorizationId: String?, manage: () -> Unit) {
    val store = model.organizationRooms ?: return
    val mode by (model.calls?.mode ?: remember { MutableStateFlow(CallHost.Mode.HIDDEN) }).collectAsStateWithLifecycle()
    val scheduled by (model.calls?.preparingScheduled ?: remember { MutableStateFlow(false) }).collectAsStateWithLifecycle()
    key(s.contextId, authorizationId) {
        var route by remember { mutableStateOf(RoomRoute.DIRECTORY) }
        var query by remember { mutableStateOf("") }
        var queryEdited by remember { mutableStateOf(false) }
        var organizations by remember { mutableStateOf(false) }
        var choice by remember { mutableStateOf<RoomConfirmation?>(null) }
        var now by remember { mutableLongStateOf(System.currentTimeMillis() / 1000) }
        val current = store.isCurrent(s.contextId)
        val limited = s.retryAt?.let { it > now } == true
        val locked = s.busy || preparing
        val cryptoReady = groups.enabled && groups.mailbox == s.mailbox && !groups.unavailable
        val org = s.organization
        val catalog = s.catalog
        val completed = s.completedCreation
        LaunchedEffect(store) { while (true) { delay(1000); now = System.currentTimeMillis() / 1000; store.synchronize() } }
        LaunchedEffect(query, queryEdited) {
            if (queryEdited && OrganizationRoomInput.text(query, 80)) { delay(300); store.loadRooms(query, s.contextId) }
        }
        LaunchedEffect(s.opening) { s.opening?.let(model::prepareOrganizationRoom) }
        fun dismiss() {
            if (locked) return
            if (s.pendingCreation != null) choice = RoomConfirmation(s.contextId, "leave")
            else if (s.draft?.dirty == true && s.completedCreation == null) choice = RoomConfirmation(s.contextId, "draft")
            else model.closeOrganizationRooms(s.contextId)
        }
        val title = when (route) { RoomRoute.DIRECTORY -> "Organization rooms"; RoomRoute.PREVIEW -> "Room details"; RoomRoute.CREATE -> "New room" }
        PageDialog(title, ::dismiss, back = if (route == RoomRoute.DIRECTORY) null else ({ store.closeReview(s.contextId); route = RoomRoute.DIRECTORY }), dismissEnabled = !locked) {
            if (!current) Text("Organization rooms are unavailable for this inbox.", color = Pp.colors.secondary)
            else {
                s.message?.let { Text(it, color = Pp.colors.heldText, modifier = Modifier.testTag("org:message")) }
                if (limited) Text("Try again in ${(s.retryAt!! - now).coerceAtLeast(1)} seconds.", color = Pp.colors.secondary)
                if (preparing) Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) { CircularProgressIndicator(Modifier.size(24.dp)); Text("Opening encrypted room…") }
                when (route) {
                    RoomRoute.DIRECTORY -> {
                        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                            Text("Using ${inbox.acting?.key ?: s.mailbox}", Modifier.weight(1f), color = Pp.colors.secondary)
                            TextButton({ store.loadOrganizations(s.contextId, query) }, enabled = !locked && !limited, modifier = Modifier.testTag("org:refresh")) { Text("Refresh") }
                        }
                        if (s.loading) Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) { CircularProgressIndicator(Modifier.size(24.dp)); Text("Loading rooms…") }
                        if (org != null) {
                            val matching = s.catalog?.forMailbox(s.mailbox.orEmpty()).orEmpty()
                            if (matching.size > 1) Box {
                                TextButton({ organizations = true }, enabled = !locked, modifier = Modifier.testTag("org:choose")) { Text(org.name) }
                                DropdownMenu(organizations, { organizations = false }) {
                                    matching.forEach { item -> DropdownMenuItem({ Text(item.name) }, {
                                        organizations = false
                                        if (store.selectOrganization(item.id, s.contextId)) { query = ""; queryEdited = false; store.loadRooms("", s.contextId) }
                                    }) }
                                }
                            } else Text(org.name, style = MaterialTheme.typography.headlineSmall)
                            if (!org.writable) Text("This organization is read only.", color = Pp.colors.secondary)
                            SearchField(query, { query = it.take(320); queryEdited = true }, "Find a room", Modifier.fillMaxWidth().testTag("org:search"))
                            if (!OrganizationRoomInput.text(query, 80)) Text("Search with at most 80 characters.", color = Pp.colors.heldText)
                            s.rooms.forEach { room ->
                                OutlinedButton({ if (store.inspect(room.id, s.directoryProof, s.contextId) != null) route = RoomRoute.PREVIEW },
                                    enabled = !locked && !limited, modifier = Modifier.fillMaxWidth().testTag("org:room:${room.id}")) {
                                    Column(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
                                        Text(room.name, style = MaterialTheme.typography.titleMedium)
                                        Text("${room.memberCount} ${if (room.memberCount == 1) "member" else "members"} · " + when (room.action) {
                                            RoomAction.OPEN -> if (room.membership == "keying") "Joining" else "Joined"
                                            RoomAction.ACCEPT_INVITATION -> "Invited"
                                            RoomAction.JOIN -> "Open to organization members"
                                            RoomAction.UNAVAILABLE -> "Invitation required"
                                        }, style = MaterialTheme.typography.bodySmall)
                                    }
                                }
                            }
                            if (s.rooms.isEmpty() && !s.loading) Text(if (query.isEmpty()) "No rooms are listed here yet." else "No rooms match your search.", color = Pp.colors.secondary)
                            if (s.pendingCreation != null || s.draft != null && s.completedCreation == null) OutlinedButton({ route = RoomRoute.CREATE },
                                enabled = !locked, modifier = Modifier.fillMaxWidth().testTag("org:resume")) { Text(if (s.pendingCreation != null) "Review unconfirmed creation" else "Continue room draft") }
                            if (completed != null) OutlinedButton({ store.checkCreated(s.contextId) }, enabled = !locked && !limited && cryptoReady,
                                modifier = Modifier.fillMaxWidth().testTag("org:openCreated")) { Text("Open ${completed.name}") }
                            if (store.canCreate(s.contextId) && s.pendingCreation == null && (s.draft == null || completed != null)) Button({ if (store.beginCreation(s.contextId)) route = RoomRoute.CREATE },
                                enabled = !locked && !limited && cryptoReady, modifier = Modifier.fillMaxWidth().testTag("org:new")) { Text("New room") }
                            if (!cryptoReady) Text("Encrypted rooms are starting on this device.", color = Pp.colors.secondary)
                            Text("Messages are end-to-end encrypted. Your organization controls who can join each room.", color = Pp.colors.secondary)
                        } else if (catalog != null && catalog.orgs.isNotEmpty()) {
                            Text("Choose your organization inbox to see its rooms.", color = Pp.colors.secondary)
                            catalog.orgs.filter { it.readable }.forEach { item ->
                                val mailbox = inbox.mailboxes.firstOrNull { it.address == item.mailbox?.address }
                                if (mailbox != null) OutlinedButton({ choice = RoomConfirmation(s.contextId, "switch", mailbox.address, mailbox.key) },
                                    enabled = !locked && mode == CallHost.Mode.HIDDEN && !scheduled, modifier = Modifier.fillMaxWidth().testTag("org:switch:${item.id}")) { Text("Use ${item.name} · ${mailbox.key}") }
                            }
                            if (mode != CallHost.Mode.HIDDEN || scheduled) Text("Finish your call before switching inboxes.", color = Pp.colors.secondary)
                        } else if (!s.loading) Text("No organizations are available for this inbox.", color = Pp.colors.secondary)
                        TextButton(manage, enabled = !locked) { Text("Manage your organization") }
                    }
                    RoomRoute.PREVIEW -> {
                        val preview = s.preview
                        if (s.reviewing) CircularProgressIndicator(Modifier.size(24.dp))
                        if (preview != null) {
                            Text(preview.name, style = MaterialTheme.typography.headlineSmall)
                            preview.description?.let { Text(it, color = Pp.colors.secondary) }
                            Text("${preview.memberCount} ${if (preview.memberCount == 1) "member" else "members"}")
                            Text("Messages are end-to-end encrypted.", color = Pp.colors.secondary)
                            if (org?.writable == false) Text("This organization is read only.", color = Pp.colors.secondary)
                            val action = store.reviewAction(s.reviewProof, s.contextId)
                            if (action != RoomAction.UNAVAILABLE) Button({ store.openReviewed(s.reviewProof, s.contextId) },
                                enabled = !locked && !limited && cryptoReady, modifier = Modifier.fillMaxWidth().testTag("org:action")) {
                                Text(when (action) { RoomAction.OPEN -> "Open room"; RoomAction.ACCEPT_INVITATION -> "Accept invitation"; else -> "Join room" })
                            } else Text(if (s.mustReviewAgain) "Review the room again before deciding." else "An invitation or an active organization membership is required.", color = Pp.colors.secondary)
                            if (s.mustReviewAgain) OutlinedButton({ store.inspect(preview.id, s.directoryProof, s.contextId) }, enabled = !locked && !limited,
                                modifier = Modifier.testTag("org:reviewAgain")) { Text("Review again") }
                        } else if (!s.reviewing) Text("The room could not be confirmed. Return to the directory and refresh.", color = Pp.colors.secondary)
                    }
                    RoomRoute.CREATE -> {
                        val draft = s.draft
                        if (completed != null) {
                            Text("Room created", style = MaterialTheme.typography.headlineSmall, modifier = Modifier.testTag("org:created"))
                            Text(completed.name)
                            Button({ store.checkCreated(s.contextId) }, enabled = !locked && !limited && cryptoReady, modifier = Modifier.fillMaxWidth().testTag("org:openCreated")) { Text("Open room") }
                        } else if (draft != null) {
                            val editable = store.canCreate(s.contextId) && s.pendingCreation == null && !preparing
                            Text(org?.name.orEmpty(), color = Pp.colors.secondary)
                            OutlinedTextField(draft.name, { store.changeDraft(draft.copy(name = it), s.contextId) }, enabled = editable, label = { Text("Room name") },
                                singleLine = true, modifier = Modifier.fillMaxWidth().testTag("org:name"))
                            OutlinedTextField(draft.description, { store.changeDraft(draft.copy(description = it), s.contextId) }, enabled = editable, label = { Text("Description") },
                                minLines = 2, modifier = Modifier.fillMaxWidth().testTag("org:description"))
                            Text("Who can join", style = MaterialTheme.typography.titleSmall)
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                FilterChip(draft.join == RoomJoinPolicy.MEMBERS, { store.changeDraft(draft.copy(join = RoomJoinPolicy.MEMBERS), s.contextId) }, enabled = editable, label = { Text("Members") })
                                FilterChip(draft.join == RoomJoinPolicy.INVITED, { store.changeDraft(draft.copy(join = RoomJoinPolicy.INVITED), s.contextId) }, enabled = editable, label = { Text("Invited only") })
                            }
                            Text("Who can post", style = MaterialTheme.typography.titleSmall)
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                FilterChip(draft.post == RoomPostPolicy.EVERYONE, { store.changeDraft(draft.copy(post = RoomPostPolicy.EVERYONE), s.contextId) }, enabled = editable, label = { Text("Everyone") })
                                FilterChip(draft.post == RoomPostPolicy.ADMINS, { store.changeDraft(draft.copy(post = RoomPostPolicy.ADMINS), s.contextId) }, enabled = editable, label = { Text("Admins") })
                            }
                            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                                Text("List in the organization directory", Modifier.weight(1f))
                                Switch(draft.listed, { store.changeDraft(draft.copy(listed = it), s.contextId) }, enabled = editable, modifier = Modifier.testTag("org:listed"))
                            }
                            if (s.pendingCreation != null) {
                                Text("Creation is not confirmed. The room may already exist.", color = Pp.colors.heldText)
                                Button({ if (s.createdCandidateId != null) store.checkCreated(s.contextId) else store.submitDraft(draft.id, s.contextId) },
                                    enabled = !locked && !limited && cryptoReady && (s.createdCandidateId != null || store.canCreate(s.contextId)), modifier = Modifier.fillMaxWidth().testTag("org:retry")) { Text(if (s.createdCandidateId != null) "Check created room" else "Retry same creation") }
                                OutlinedButton({ choice = RoomConfirmation(s.contextId, "discard", organization = draft.organizationId) }, enabled = !locked,
                                    modifier = Modifier.fillMaxWidth().testTag("org:discard")) { Text("Discard attempt") }
                            } else Button({ store.submitDraft(draft.id, s.contextId) }, enabled = editable && !limited && cryptoReady && draft.name.isNotBlank(),
                                modifier = Modifier.fillMaxWidth().testTag("org:create")) { Text("Create room") }
                            Text("New members cannot read earlier messages. Room messages are end-to-end encrypted.", color = Pp.colors.secondary)
                        } else Text("Choose New room in the directory to start a draft.", color = Pp.colors.secondary)
                    }
                }
            }
        }
        choice?.let { target ->
            val title = when (target.kind) { "switch" -> "Use this organization inbox?"; "discard" -> "Discard this creation attempt?"; "leave" -> "Leave this creation attempt?"; else -> "Discard this room draft?" }
            val detail = if (target.kind == "switch") "Rooms will open as ${target.label}." else if (target.kind == "draft") "The unsent room draft will be removed." else "The room may already exist. Check your rooms before creating another."
            Confirm(title, detail, when (target.kind) { "switch" -> "Use inbox"; "discard" -> "Discard attempt"; "leave" -> "Leave attempt"; else -> "Discard draft" }, confirm = {
                if (choice === target) {
                    choice = null
                    when (target.kind) {
                        "switch" -> target.address?.let { model.switchOrganizationMailbox(it, target.context) }
                        "discard" -> target.organization?.let { if (store.discardCreation(target.context, it)) route = RoomRoute.DIRECTORY }
                        else -> model.closeOrganizationRooms(target.context, discardUnconfirmed = true)
                    }
                }
            }, dismiss = { if (choice === target) choice = null }, enabled = !locked && target.context == s.contextId &&
                (target.kind != "switch" || mode == CallHost.Mode.HIDDEN && !scheduled))
        }
    }
}
