@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class, androidx.compose.foundation.ExperimentalFoundationApi::class)
package dev.pigeonpost.inbox.ui

import android.app.Activity
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.view.WindowCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.AppPolicy
import dev.pigeonpost.inbox.InboxViewModel
import dev.pigeonpost.inbox.R
import dev.pigeonpost.inbox.auth.SessionState
import dev.pigeonpost.inbox.calls.CallHost

/** Windows at least this wide get the iPad/desktop three-column layout. */
val WideLayoutWidth = 700.dp

@Composable
fun PigeonpostApp(model: InboxViewModel, signIn: (String?, Boolean) -> Unit, chooseFile: () -> Unit,
    choosePhoto: () -> Unit, scan: () -> Unit, attachment: (Attachment, String) -> Unit, openLink: (String) -> Unit,
    notificationSettings: () -> Unit = {}, fullScreenCalls: () -> (() -> Unit)? = { null },
    choosePicture: () -> Unit = {}, takePicture: () -> Unit = {}) {
    val session by model.session.state.collectAsStateWithLifecycle()
    val state by model.inbox.state.collectAsStateWithLifecycle()
    val groupStore = model.groups
    val groups by (groupStore?.state ?: remember { kotlinx.coroutines.flow.MutableStateFlow(dev.pigeonpost.core.groups.GroupsState()) }).collectAsStateWithLifecycle()
    val cropping by model.crop.collectAsStateWithLifecycle()
    val profileState by model.profile.state.collectAsStateWithLifecycle()
    var blockTarget by remember { mutableStateOf<BlockTarget?>(null) }
    var filteredOpen by remember { mutableStateOf<String?>(null) }
    val handleState by model.handles.state.collectAsStateWithLifecycle()
    val appearance by model.appearance.collectAsStateWithLifecycle()
    val callMode = model.calls?.mode?.collectAsStateWithLifecycle()?.value ?: CallHost.Mode.HIDDEN
    val joiningMeeting = model.calls?.preparingScheduled?.collectAsStateWithLifecycle()?.value ?: false
    val store = model.inbox
    val calendarStore = model.calendar
    val calendar by (calendarStore?.state ?: remember { kotlinx.coroutines.flow.MutableStateFlow(dev.pigeonpost.core.calendar.CalendarState()) }).collectAsStateWithLifecycle()
    val meetingCode by model.memberMeetingCode.collectAsStateWithLifecycle()
    val deviceOpen by model.deviceApprovalOpen.collectAsStateWithLifecycle()
    val organizationRoomsOpen by model.organizationRoomsOpen.collectAsStateWithLifecycle()
    val organizationRoomPreparing by model.organizationRoomPreparing.collectAsStateWithLifecycle()
    val organizationRooms by (model.organizationRooms?.state ?: remember { kotlinx.coroutines.flow.MutableStateFlow(dev.pigeonpost.core.enterprise.OrganizationRoomsState()) }).collectAsStateWithLifecycle()
    val deviceApproval by (model.deviceApproval?.state ?: remember { kotlinx.coroutines.flow.MutableStateFlow(dev.pigeonpost.core.enterprise.DeviceApprovalState()) }).collectAsStateWithLifecycle()
    val meetingLookup by (model.meetingLookup?.state ?: remember { kotlinx.coroutines.flow.MutableStateFlow(dev.pigeonpost.core.calendar.CalendarMeetingLookupState()) }).collectAsStateWithLifecycle()
    var sheet by rememberSaveable { mutableStateOf<String?>(null) }
    var editContact by remember { mutableStateOf<Contact?>(null) }
    var viewing by remember { mutableStateOf<Attachment?>(null) }
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(state.error) { state.error?.let { snackbar.showSnackbar(it); store.clearError() } }
    LaunchedEffect(groups.notice) { groups.notice?.let { snackbar.showSnackbar(it); groupStore?.clearNotice() } }
    LaunchedEffect(meetingCode, session.signedIn, session.termsAccepted) {
        if (meetingCode != null && !session.loading && (!session.signedIn || !session.termsAccepted)) snackbar.showSnackbar("Sign in and accept the terms to open this meeting.")
    }
    LaunchedEffect(meetingCode, meetingLookup.contextId, state.acting?.address, session.authorizationId) {
        val code = meetingCode
        if (code != null && session.signedIn && session.termsAccepted && state.accountLoaded && meetingLookup.mailbox == state.acting?.address && model.meetingLookup?.state?.value?.code != code)
            model.meetingLookup?.load(code, meetingLookup.contextId)
    }
    LaunchedEffect(meetingCode, calendar.contextId, calendar.enabled) {
        if (meetingCode != null && calendar.enabled) model.calendar?.refreshFor(calendar.contextId)
    }
    // Preserve a Settings subpage through rotation, but never carry it to another account/inbox.
    var sheetOwner by rememberSaveable { mutableStateOf<String?>(null) }
    LaunchedEffect(session.signedIn, session.loading, session.username, state.acting?.address) {
        val owner = if (session.signedIn && state.acting != null) "${session.username}\n${state.acting?.address}" else null
        val signedOut = !session.loading && !session.signedIn
        if (signedOut || (owner != null && sheetOwner != null && owner != sheetOwner)) {
            sheet = null; editContact = null; viewing = null; blockTarget = null; filteredOpen = null
        }
        if (owner != null || signedOut) sheetOwner = owner
    }
    val lifecycle by LocalLifecycleOwner.current.lifecycle.currentStateFlow.collectAsState()
    val visibleIds = state.subject?.messages.orEmpty().map { it.id }
    // A failed acknowledgement may revert read marks. That alone must not trigger a retry loop.
    LaunchedEffect(session.termsAccepted, state.acting?.address, state.selectedPeer, state.subject?.id, visibleIds, lifecycle) {
        if (session.termsAccepted && state.selectedPeer != null && lifecycle.isAtLeast(Lifecycle.State.RESUMED)) store.acknowledgeVisible()
    }
    // The in-app viewer for files Android can show safely; anything else keeps the existing open/share/save.
    val open: (Attachment, String) -> Unit = { file, action ->
        if (action == "view" || (action == "open" && AttachmentKinds.viewer(file.filename, file.mediaType) != null)) {
            if (AttachmentKinds.viewer(file.filename, file.mediaType) != null) viewing = file else attachment(file, "open")
        } else attachment(file, action)
    }
    // A group file is opened the same ways; its bytes come from the group and are decrypted here.
    val openGroupFile: (dev.pigeonpost.core.groups.Group, dev.pigeonpost.core.groups.GroupFile, String) -> Unit = { g, f, action ->
        open(Attachment("gfile:${g.id}|${f.id}", f.name, f.mime, f.size), action)
    }
    // The Calls or Filtered view stays where it was, so going back returns to it.
    val openPeer: (String) -> Unit = { peer -> model.openPeer(peer) }
    val dark = appearance.isDark()
    val view = LocalView.current
    if (!view.isInEditMode) SideEffect {
        (view.context as? Activity)?.window?.let { window ->
            // A full-screen call is drawn dark whatever the app's appearance, so its bars carry light icons.
            val light = !dark && callMode != CallHost.Mode.FULL
            WindowCompat.getInsetsController(window, view).apply { isAppearanceLightStatusBars = light; isAppearanceLightNavigationBars = light }
        }
    }
    PigeonpostTheme(dark) {
      CompositionLocalProvider(LocalAvatars provides model.pictures, LocalAskBlock provides { target -> blockTarget = target }) {
        val p = Pp.colors
        BoxWithConstraints(Modifier.fillMaxSize()) {
            val wide = maxWidth >= WideLayoutWidth
            BackHandler(enabled = !wide && state.selectedPeer != null && sheet == null) { store.selectPeer(null) }
            BackHandler(enabled = !wide && groups.open != null && sheet == null) { groupStore?.close() }
            BackHandler(enabled = state.leftView != LeftView.INBOX && groups.open == null && (wide || state.selectedPeer == null) && sheet == null) { store.showView(LeftView.INBOX) }
            BackHandler(enabled = state.viewingArchive && (wide || state.selectedPeer == null) && sheet == null) { store.showArchive(false) }
            val ground = when {
                !session.signedIn || !session.termsAccepted || state.acting == null -> p.app
                wide -> p.sidebar
                state.selectedPeer != null || groups.open != null -> p.wash
                else -> p.sidebar
            }
            Scaffold(snackbarHost = { SnackbarHost(snackbar) { Snackbar(it, containerColor = p.toastFill, contentColor = androidx.compose.ui.graphics.Color.White) } },
                contentWindowInsets = WindowInsets.safeDrawing, containerColor = ground) { padding ->
                Box(Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding)
                    .padding(top = if (callMode == CallHost.Mode.MINI) 64.dp else 0.dp).imePadding()) {
                    when {
                        session.loading -> Loading("Opening Pigeonpost…")
                        !session.signedIn -> SignIn(session, signIn, openLink)
                        !session.termsAccepted -> TermsConsentScreen(session, model::acceptTerms, model::signOut, openLink)
                        state.accountLoading && !state.accountLoaded -> Loading("Opening your inboxes…")
                        state.acting == null -> FirstInbox(state, store, model::signOut, openLink)
                        wide && state.leftView == LeftView.CALENDAR && calendarStore != null -> CalendarPane(calendarStore, calendar, { store.showView(LeftView.INBOX) }, if (model.calls == null) null else model::joinCalendar)
                        wide -> TabletInbox(state, store, model, session.username, callMode,
                            newConversation = { sheet = "new" }, settings = { sheet = "settings" },
                            info = { sheet = "peer" }, newSubject = { sheet = "subject" },
                            chooseFile = chooseFile, choosePhoto = choosePhoto, attachment = open, openLink = openLink,
                            groups = groups, newGroup = { sheet = "newGroup" }, groupInfo = { sheet = "groupInfo" }, openGroupFile = openGroupFile,
                            openFiltered = { filteredOpen = it.peer }, openPeer = openPeer)
                        groups.open != null && groupStore != null -> GroupPane(model, groups, state, groupStore, Modifier.fillMaxSize(), wide = false, callMode = callMode,
                            info = { sheet = "groupInfo" }, chooseFile = chooseFile, choosePhoto = choosePhoto, openFile = openGroupFile)
                        state.selectedPeer != null -> Box(Modifier.fillMaxSize().background(p.wash)) {
                            ConversationPane(state, store, model, Modifier.fillMaxSize(), wide = false, callMode = callMode,
                                info = { sheet = "peer" }, newSubject = { sheet = "subject" },
                                chooseFile = chooseFile, choosePhoto = choosePhoto, attachment = open, openLink = openLink)
                        }
                        else -> PhoneHome(state, store, model, groups, Modifier.fillMaxSize().background(p.sidebar),
                            mailbox = { sheet = "mailbox" }, new = { sheet = "new" }, newGroup = { sheet = "newGroup" }, settings = { sheet = "settings" },
                            openFiltered = { filteredOpen = it.peer }, openPeer = openPeer)
                    }
                }
            }
        }
        when (sheet) {
            "mailbox" -> MailboxDialog(state, { store.switchMailbox(it); sheet = null }, { sheet = null }, session.username)
            "new" -> NewConversationDialog(state, { store.selectPeer(it); sheet = null }, { sheet = null })
            "subject" -> SubjectDialog(state.actionBusy, { store.openThread(it) { sheet = null } }, { sheet = null })
            "settings" -> SettingsDialog(state, session, model.graph.fixtures, handleState, model.handles,
                paidHandles = model.paidHandles,
                accountHandles = model.accountHandles, refreshMailboxes = { store.loadAccount() },
                openInbox = { store.switchMailbox(it); sheet = null }, dismiss = { sheet = null },
                contacts = { sheet = "contacts" }, archive = { store.showArchive(true); sheet = null }, scan = scan,
                signOut = { sheet = "signout" }, openLink = openLink, notificationSettings = notificationSettings, model = model, groups = groups, choosePicture = choosePicture, takePicture = takePicture,
                fullScreenCalls = fullScreenCalls(),
                appearance = appearance, chooseAppearance = model::chooseAppearance)
            "contacts" -> ContactsDialog(state, { editContact = it; sheet = "contact" }, { sheet = null }, back = { sheet = "settings" })
            "contact" -> ContactDialog(state, editContact, { store.saveContact(it) { sheet = "contacts" } },
                { peer -> store.removeContact(peer) { sheet = "contacts" } }, { sheet = "contacts" })
            "peer" -> PeerDialog(state, store, { sheet = null }, { contact -> editContact = contact; sheet = "contact" })
            "newGroup" -> if (groupStore != null) NewGroupDialog(state, groups, groupStore) { sheet = null }
            "groupInfo" -> if (groupStore != null) GroupInfoDialog(groups, state, groupStore) { sheet = null }
            "signout" -> Confirm("Sign out?", "Your session and local drafts will be removed from this device.", "Sign out",
                { sheet = null; model.signOut() }, { sheet = "settings" })
        }
        if (joiningMeeting && callMode == CallHost.Mode.HIDDEN && session.signedIn && session.termsAccepted) AlertDialog(
            onDismissRequest = { model.calls?.cancelScheduledJoin() }, title = { Text("Joining meeting") },
            text = { Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) { CircularProgressIndicator(Modifier.size(24.dp)); Text("Preparing your call…") } },
            confirmButton = {}, dismissButton = { TextButton({ model.calls?.cancelScheduledJoin() }) { Text("Cancel") } })
        meetingCode?.let { code -> if (session.signedIn && session.termsAccepted && state.acting != null) MeetingLinkDialog(model, state, code, meetingLookup, calendar.zone.id) }
        if (deviceOpen && !session.loading && session.signedIn && session.termsAccepted) DeviceApprovalDialog(model, deviceApproval)
        if (organizationRoomsOpen && !session.loading && session.signedIn && session.termsAccepted)
            OrganizationRoomsDialog(model, state, organizationRooms, groups, organizationRoomPreparing, session.authorizationId) { openLink("https://enterprise.pigeonpost.dev/app") }
        viewing?.let { file ->
            val identity = state.acting?.address
            if (identity == null) viewing = null
            else AttachmentViewer(identity, file, { model.fetch(identity, file) }, download = { attachment(file, "download") }, dismiss = { viewing = null })
        }
        filteredOpen?.let { peer ->
            val group = state.filteredGroups.firstOrNull { it.peer == peer }
            if (group == null) LaunchedEffect(peer) { filteredOpen = null } else FilteredSheet(group, state, store) { filteredOpen = null }
        }
        blockTarget?.let { target ->
            BlockSheet(target, state.actionBusy, confirm = { wide, report ->
                store.blockSender(target.peer, wide, report, target.messageId, target.callId) { blockTarget = null }
            }, dismiss = { blockTarget = null })
        }
        if (groupStore != null) JoinGroupDialog(groups, groupStore, state)
        cropping?.let { bitmap -> CropDialog(bitmap, profileState.saving, profileState.error, model::saveCrop, model::cancelCrop) }
      }
    }
}

@Composable
private fun SignIn(state: SessionState, signIn: (String?, Boolean) -> Unit, openLink: (String) -> Unit) {
    val p = Pp.colors
    Box(Modifier.fillMaxSize().background(p.app), contentAlignment = Alignment.Center) {
        Column(Modifier.widthIn(max = 420.dp).verticalScroll(rememberScrollState()).padding(28.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Image(painterResource(R.drawable.ic_pigeonpost), "Pigeonpost", Modifier.size(96.dp).clip(RoundedCornerShape(22.dp)))
            Text("Pigeonpost", style = geist(30.sp, FontWeight.Bold), color = p.ink)
            Text("A direct line to your agents.", style = geist(16.sp), color = p.secondary)
            Spacer(Modifier.height(18.dp))
            Button({ signIn(null, false) }, Modifier.fillMaxWidth().height(48.dp), enabled = !state.busy, shape = RoundedCornerShape(12.dp)) { Text("Sign in", style = geist(15.sp, FontWeight.SemiBold)) }
            listOf("google" to "Continue with Google", "github" to "Continue with GitHub", "apple" to "Continue with Apple").forEach { (provider, label) ->
                OutlinedButton({ signIn(provider, false) }, Modifier.fillMaxWidth().height(48.dp), enabled = !state.busy, shape = RoundedCornerShape(12.dp),
                    border = androidx.compose.foundation.BorderStroke(1.dp, p.strokeStrong),
                    colors = ButtonDefaults.outlinedButtonColors(containerColor = p.ground, contentColor = p.ink)) { Text(label, style = geist(15.sp, FontWeight.Medium)) }
            }
            TextButton({ signIn(null, true) }, enabled = !state.busy) { Text("Use a different account") }
            if (state.busy) CircularProgressIndicator(Modifier.size(24.dp), color = p.accent)
            state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            Text("Your conversations, subjects, and contacts stay with your account.", style = geist(13.5.sp), color = p.muted, textAlign = TextAlign.Center)
            Row(horizontalArrangement = Arrangement.Center) {
                TextButton({ openLink(AppPolicy.TERMS_URL) }) { Text("Terms of service", color = p.secondary) }
                TextButton({ openLink(AppPolicy.PRIVACY_URL) }) { Text("Privacy policy", color = p.secondary) }
                TextButton({ openLink(AppPolicy.SUPPORT_URL) }) { Text("Support", color = p.secondary) }
            }
        }
    }
}

@Composable
private fun FirstInbox(state: InboxState, store: InboxStore, signOut: () -> Unit, openLink: (String) -> Unit) {
    val p = Pp.colors
    Column(Modifier.fillMaxSize().background(p.app).padding(28.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
        Image(painterResource(R.drawable.ic_pigeonpost), null, Modifier.size(88.dp).clip(RoundedCornerShape(20.dp)))
        Spacer(Modifier.height(16.dp))
        Text(if (state.accountLoaded) "Your first inbox" else "Couldn’t open your inboxes", style = geist(22.sp, FontWeight.SemiBold), color = p.ink)
        Text(if (state.accountLoaded) "Create an inbox to start a conversation with your agents." else "Check your connection and try again.",
            Modifier.padding(vertical = 16.dp), color = p.secondary, style = geist(15.sp), textAlign = TextAlign.Center)
        Button({ if (state.accountLoaded) store.createFirstInbox() else store.loadAccount() }, enabled = !state.creating && !state.accountLoading, shape = RoundedCornerShape(12.dp)) {
            Text(if (state.creating) "Creating…" else if (state.accountLoaded) "Create inbox" else "Try again")
        }
        TextButton(signOut) { Text("Sign out", color = p.secondary) }
        TextButton({ openLink("https://pigeonpost.dev/delete-account.html") }) { Text("Delete account", color = p.blockedText) }
    }
}

/** A search field in the design's own shape: a filled, borderless, rounded row. */
@Composable
fun SearchField(value: String, change: (String) -> Unit, placeholder: String, modifier: Modifier = Modifier) {
    val p = Pp.colors
    BasicTextField(value, change, modifier.semantics { contentDescription = placeholder }, singleLine = true,
        textStyle = geist(14.5.sp, color = p.ink), cursorBrush = SolidColor(p.accent),
        decorationBox = { inner ->
            Row(Modifier.fillMaxWidth().height(40.dp).background(p.field, RoundedCornerShape(10.dp)).padding(horizontal = 12.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Icon(Icons.Outlined.Search, null, Modifier.size(18.dp), tint = p.muted)
                Box(Modifier.weight(1f)) {
                    if (value.isEmpty()) Text(placeholder, style = geist(14.5.sp), color = p.muted, maxLines = 1)
                    inner()
                }
            }
        })
}

/**
 * The phone's front page: Conversations and, when the postbox keeps call history, a Calls tab
 * beside it (SDS §12.1) with the unseen-missed badge. Filtered mail opens from the list's last row.
 */
@Composable
private fun PhoneHome(state: InboxState, store: InboxStore, model: InboxViewModel, groups: dev.pigeonpost.core.groups.GroupsState, modifier: Modifier,
    mailbox: () -> Unit, new: () -> Unit, newGroup: () -> Unit, settings: () -> Unit, openFiltered: (FilteredGroup) -> Unit, openPeer: (String) -> Unit) {
    val p = Pp.colors
    Column(modifier) {
        Box(Modifier.weight(1f)) {
            when (state.leftView) {
                LeftView.CALLS -> CallsPane(state, store, Modifier.fillMaxSize(), phone = true, open = openPeer,
                    callBack = { key, video -> openPeer(key); model.calls?.call(key, video) })
                LeftView.FILTERED -> FilteredPane(state, Modifier.fillMaxSize(), phone = true, back = { store.showView(LeftView.INBOX) }, open = openFiltered)
                LeftView.INBOX -> InboxList(state, store, model, groups, Modifier.fillMaxSize(), mailbox, new, newGroup, settings, openPeer)
                LeftView.CALENDAR -> model.calendar?.let { calendar ->
                    val s by calendar.state.collectAsStateWithLifecycle()
                    CalendarPane(calendar, s, { store.showView(LeftView.INBOX) }, if (model.calls == null) null else model::joinCalendar)
                }
            }
        }
        val hasCalendar = model.calendar != null && "calendar" in state.features
        if (state.has(Features.CALL_HISTORY) || hasCalendar) NavigationBar(containerColor = p.sidebar, tonalElevation = 0.dp, modifier = Modifier.height(64.dp)) {
            NavigationBarItem(state.leftView == LeftView.INBOX || state.leftView == LeftView.FILTERED, { store.showView(LeftView.INBOX) }, { Icon(Icons.Outlined.Forum, null) },
                label = { Text("Conversations") }, modifier = Modifier.testTag("tab:conversations"))
            if (state.has(Features.CALL_HISTORY)) NavigationBarItem(state.leftView == LeftView.CALLS, { store.showView(LeftView.CALLS) }, {
                BadgedBox(badge = { if (state.calls.unseen > 0) Badge { Text(if (state.calls.unseen > 99) "99+" else state.calls.unseen.toString()) } }) { Icon(Icons.Outlined.Call, null) }
            }, label = { Text("Calls") }, modifier = Modifier.testTag("tab:calls").semantics { contentDescription = if (state.calls.unseen > 0) "Calls, ${state.calls.unseen} missed" else "Calls" })
            if (hasCalendar) NavigationBarItem(state.leftView == LeftView.CALENDAR, model::openCalendar, { Icon(Icons.Outlined.CalendarMonth, null) },
                label = { Text("Calendar") }, modifier = Modifier.testTag("tab:calendar"))
        }
    }
}

@Composable
private fun InboxList(state: InboxState, store: InboxStore, model: InboxViewModel, groups: dev.pigeonpost.core.groups.GroupsState, modifier: Modifier,
    mailbox: () -> Unit, new: () -> Unit, newGroup: () -> Unit, settings: () -> Unit, openPeer: (String) -> Unit) {
    val p = Pp.colors
    var compose by remember { mutableStateOf(false) }
    var invitesOpen by rememberSaveable { mutableStateOf(true) }
    Column(modifier) {
        Row(Modifier.fillMaxWidth().padding(start = 8.dp, end = 4.dp, top = 6.dp, bottom = 2.dp), verticalAlignment = Alignment.CenterVertically) {
            if (state.viewingArchive) ActionIcon("Back to inbox", Icons.AutoMirrored.Outlined.ArrowBack, tint = p.accent) { store.showArchive(false) }
            Row(Modifier.weight(1f).clip(RoundedCornerShape(10.dp)).clickable(onClick = mailbox).padding(horizontal = 8.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                state.acting?.let { Avatar(it.key, it.name, 30.dp) }
                Column(Modifier.weight(1f, fill = false)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(if (state.viewingArchive) "Archive" else "Inbox", style = geist(22.sp, FontWeight.Bold), color = p.ink)
                        Icon(Icons.Outlined.ExpandMore, null, Modifier.padding(start = 2.dp).size(20.dp), tint = p.secondary)
                    }
                    state.acting?.let { Text(it.name, style = geistMono(12.sp), color = p.muted, maxLines = 1, overflow = TextOverflow.MiddleEllipsis) }
                }
            }
            // New group sits in the compose menu on a phone (SDS §4.2), when groups are on.
            if (groups.enabled) Box {
                ActionIcon("New conversation", Icons.Outlined.Edit) { compose = true }
                DropdownMenu(compose, { compose = false }, containerColor = p.ground) {
                    DropdownMenuItem({ Text("New conversation") }, { compose = false; new() }, leadingIcon = { Icon(Icons.Outlined.Edit, null) })
                    DropdownMenuItem({ Text("New group") }, { compose = false; newGroup() }, leadingIcon = { Icon(Icons.Outlined.GroupAdd, null) }, modifier = Modifier.testTag("menu:newGroup"))
                    if (state.has("orgs") && model.organizationRooms != null) DropdownMenuItem({ Text("Organization rooms") }, { compose = false; model.openOrganizationRooms() },
                        leadingIcon = { Icon(Icons.Outlined.Business, null) }, modifier = Modifier.testTag("menu:organizationRooms"))
                }
            } else ActionIcon("New conversation", Icons.Outlined.Edit, action = new)
            ActionIcon("Settings", Icons.Outlined.Settings, action = settings)
        }
        SearchField(state.filter, store::filter, "Search conversations", Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp))
        if (state.loading) LinearProgressIndicator(Modifier.fillMaxWidth().height(2.dp), color = p.accent, trackColor = p.rule)
        else Seam(Modifier.fillMaxWidth().height(1.dp))
        if (state.offline) Text("Offline · showing the last loaded conversations", Modifier.padding(16.dp), color = p.heldText, style = geist(12.5.sp, FontWeight.Medium))
        val filtered = if (!state.viewingArchive && state.filter.isEmpty()) state.filteredGroups.size else 0
        val hasGroups = groups.enabled && (groups.joined.isNotEmpty() || groups.invitations.isNotEmpty()) && !state.viewingArchive
        if (state.visible.isEmpty() && !hasGroups && filtered == 0) {
            EmptyPane(if (state.filter.isNotEmpty()) "No matches" else if (state.viewingArchive) "Archive is empty" else "No conversations yet",
                if (state.filter.isNotEmpty()) "Try a name or address." else "Your conversations will appear here.", Modifier.weight(1f))
        } else LazyColumn(Modifier.weight(1f)) {
            model.groups?.let { gs -> groupRows(groups, state, gs, phone = true, invitesOpen = invitesOpen, toggleInvites = { invitesOpen = !invitesOpen }, open = model::openGroup) }
            items(state.visible, key = { it.peer }) { conversation ->
                ConversationRow(conversation, false, phone = true) { openPeer(conversation.peer) }
            }
            if (filtered > 0) item(key = "filtered") { FilteredEntry(filtered) { store.showView(LeftView.FILTERED) } }
        }
        Seam(Modifier.fillMaxWidth().height(1.dp))
        Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
            Text("${state.visible.size} conversations", style = geist(12.sp), color = p.muted)
            TextButton({ store.refresh(); model.groups?.poke() }, enabled = !state.loading) {
                Icon(Icons.Outlined.Refresh, null, Modifier.size(16.dp), tint = p.secondary); Spacer(Modifier.width(6.dp)); Text("Refresh", color = p.secondary, style = geist(13.sp, FontWeight.Medium))
            }
        }
    }
}

/** One conversation: the face, the name in mono, the time, a one-line preview and the server's chips. */
@Composable
fun ConversationRow(row: Conversation, selected: Boolean, phone: Boolean, select: () -> Unit) {
    val p = Pp.colors
    val preview = row.messages.lastOrNull()?.display?.text?.lineSequence()?.firstOrNull { it.isNotBlank() }?.trimStart('#', ' ', '>')?.take(140)?.ifBlank { "Attachment" }
        ?: if (row.messages.lastOrNull()?.attachments?.isNotEmpty() == true) "Attachment" else "Start a conversation"
    val shape = RoundedCornerShape(10.dp)
    Column(Modifier.fillMaxWidth().then(if (phone) Modifier else Modifier.padding(horizontal = 8.dp, vertical = 1.dp))
        .clip(if (phone) RoundedCornerShape(0.dp) else shape).background(if (selected) p.selected else androidx.compose.ui.graphics.Color.Transparent)
        .clickable(onClick = select).testTag("conversation:" + row.peer)) {
        Row(Modifier.padding(horizontal = if (phone) 16.dp else 10.dp, vertical = if (phone) 13.dp else 10.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(11.dp)) {
            Avatar(row.peer, row.name, if (phone) 44.dp else 38.dp)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(row.name, Modifier.weight(1f), style = geistMono(if (phone) 14.5.sp else 13.sp, FontWeight.SemiBold), color = p.ink, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(shortTime(row.last), style = geist(11.5.sp), color = p.muted)
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(preview, Modifier.weight(1f), style = geist(if (phone) 14.sp else 13.sp), maxLines = 1, overflow = TextOverflow.Ellipsis, color = p.secondary)
                    if (row.blocked) Pill("BLOCKED", PillKind.BLOCKED)
                    else if (row.held > 0) Pill("HELD", PillKind.HELD)
                    if (row.unread > 0) UnreadBadge(row.unread, inverted = false)
                }
            }
        }
        if (phone) Seam(Modifier.padding(start = 71.dp).fillMaxWidth().height(1.dp))
    }
}
