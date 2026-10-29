package dev.pigeonpost.inbox

import android.app.Application
import android.content.Intent
import android.net.Uri
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.auth.Session
import dev.pigeonpost.inbox.auth.UserSession
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import dev.pigeonpost.inbox.billing.GooglePlayBilling
import dev.pigeonpost.inbox.push.PushNotifications
import dev.pigeonpost.inbox.calls.CallHost
import dev.pigeonpost.inbox.calls.CallsOverride
import dev.pigeonpost.inbox.ui.AppearanceChoice
import dev.pigeonpost.core.groups.GroupCryptoLibrary
import dev.pigeonpost.core.groups.GroupStorage
import dev.pigeonpost.core.groups.GroupStore
import dev.pigeonpost.core.groups.GroupLinks
import dev.pigeonpost.core.groups.GroupDraftKey
import dev.pigeonpost.core.groups.GroupComposerKind
import dev.pigeonpost.core.groups.GroupTransport
import dev.pigeonpost.core.groups.UniffiGroupCrypto
import dev.pigeonpost.inbox.groups.KeystoreGroupStorage
import dev.pigeonpost.core.calendar.CalendarAttemptStorage
import dev.pigeonpost.core.calendar.CalendarStore
import dev.pigeonpost.core.calendar.CalendarTransport
import dev.pigeonpost.core.calendar.CalendarNotice
import dev.pigeonpost.core.calendar.CalendarEventKey
import dev.pigeonpost.core.calendar.CalendarMeetingApi
import dev.pigeonpost.core.calendar.CalendarMeetingCode
import dev.pigeonpost.core.calendar.CalendarMeetingLookup
import dev.pigeonpost.core.enterprise.DevicePairingApi
import dev.pigeonpost.core.enterprise.DevicePairingCode
import dev.pigeonpost.core.enterprise.DeviceApprovalIdentity
import dev.pigeonpost.core.enterprise.DeviceApprovalStore
import dev.pigeonpost.core.enterprise.OrganizationRoomApi
import dev.pigeonpost.core.enterprise.OrganizationRoomClient
import dev.pigeonpost.core.enterprise.OrganizationRoomIdentity
import dev.pigeonpost.core.enterprise.OrganizationRoomOpening
import dev.pigeonpost.core.enterprise.OrganizationRoomStore
import dev.pigeonpost.core.contacts.PeopleApi
import dev.pigeonpost.core.contacts.PeopleClient
import dev.pigeonpost.core.contacts.PeopleIdentity
import dev.pigeonpost.core.contacts.PeopleStore
import dev.pigeonpost.inbox.calendar.KeystoreCalendarAttempts
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import dev.pigeonpost.core.groups.GroupStream
import dev.pigeonpost.core.groups.GroupStreams
import dev.pigeonpost.core.groups.StreamCheck
import dev.pigeonpost.core.meetings.MeetingNotesApi
import dev.pigeonpost.core.meetings.MeetingNotesStore
import dev.pigeonpost.core.meetings.TransportMeetingNotesApi
import dev.pigeonpost.inbox.calls.LiveMedia
import dev.pigeonpost.inbox.meetings.KeystoreNotesKeys

data class AppGraph(
    val session: UserSession,
    val api: PostboxApi,
    val fixtures: Boolean = false,
    /** The postbox's group routes; null hides groups entirely. */
    val groupTransport: GroupTransport? = api as? GroupTransport,
    /** Loads the group-crypto library; null (or a failure) means groups are unavailable here. */
    val groupCrypto: () -> GroupCryptoLibrary? = { loadGroupCrypto() },
    val groupStorage: (Application) -> GroupStorage = { KeystoreGroupStorage(it) },
    /** Starts once the graph is in use: a fixture's other people, for instance. */
    val start: (CoroutineScope) -> Unit = {},
    /** How often the group list is reread while the app is in front. */
    val groupPollSeconds: Long = 15,
    val calendarTransport: CalendarTransport? = api as? CalendarTransport,
    val calendarStorage: (Application) -> CalendarAttemptStorage = { KeystoreCalendarAttempts(it) },
    val meetingApi: CalendarMeetingApi? = api as? CalendarMeetingApi,
    val pairingApi: DevicePairingApi? = api as? DevicePairingApi,
    val organizationRoomApi: OrganizationRoomApi? = groupTransport?.let(::OrganizationRoomClient),
    val peopleApi: PeopleApi? = groupTransport?.let(::PeopleClient),
    /** How often a live stream on screen is reread for stage invitations and its end (Phase 6). */
    val streamPollMillis: Long = 4000,
) {
    companion object {
        fun live(app: Application): AppGraph {
            val session = Session(app)
            return AppGraph(session, PostboxClient(session))
        }
        /** The native library must load and work, or groups stay hidden: there is no weaker fallback. */
        fun loadGroupCrypto(): GroupCryptoLibrary? = try { UniffiGroupCrypto().also { check(it.generateStateKey().size == 32) } } catch (_: Throwable) { null }
    }
}
class PigeonpostApplication : Application() { val graph by lazy { AppGraph.live(this) } }

class InboxViewModel(app: Application, val graph: AppGraph, callsOverride: CallsOverride? = null, pinnedAppearance: String? = null) : AndroidViewModel(app) {
    val session = graph.session
    private val storeScope = AuthorizedStoreScope(viewModelScope)
    private data class StoreActivation(val authorizationId: String?, val id: String = java.util.UUID.randomUUID().toString())
    private data class InboxBinding(val scopeId: String, val address: String, val handle: String?, val features: Set<String>)
    private data class CallBinding(val scopeId: String, val mailbox: Mailbox, val names: Map<String, String>)
    private val activation = MutableStateFlow<StoreActivation?>(null)
    private var calendarNoticeJob: Job? = null
    private var calendarNoticeId = java.util.UUID.randomUUID()
    private val meetingCode = MutableStateFlow<String?>(null)
    val memberMeetingCode = meetingCode.asStateFlow()
    private var meetingIntentRoot: AuthorizationLease? = null
    private val machineOpen = MutableStateFlow(false)
    val deviceApprovalOpen = machineOpen.asStateFlow()
    private var deviceNavigation: Job? = null
    var authorizationWindow: ((Intent) -> Unit)? = null
    private val roomPage = MutableStateFlow(false)
    val organizationRoomsOpen = roomPage.asStateFlow()
    private val roomPreparation = MutableStateFlow(false)
    val organizationRoomPreparing = roomPreparation.asStateFlow()
    private var roomPreparationJob: Job? = null
    private var roomPreparationId = java.util.UUID.randomUUID()
    val push = if (graph.fixtures) null else PushNotifications(app)
    private val preferences = app.getSharedPreferences(if (graph.fixtures) "fixture-settings" else "settings", 0)
    val files = PlatformFiles(app, graph.api)
    // The appearance choice is the device owner's, not the account's: it survives sign-out.
    private val looks = app.getSharedPreferences("appearance", 0)
    private val appearanceState = MutableStateFlow(pinnedAppearance?.let(AppearanceChoice::of) ?: AppearanceChoice.of(looks.getString("choice", null)))
    val appearance = appearanceState.asStateFlow()
    fun chooseAppearance(choice: AppearanceChoice) {
        appearanceState.value = choice
        looks.edit().putString("choice", choice.name.lowercase()).apply()
    }
    /** Calls run the web call client in a kept-alive WebView; fixtures have none unless a debug harness is given. */
    val calls = if (graph.fixtures && callsOverride == null) null else CallHost(app, graph.session, storeScope, callsOverride)
    val inbox = InboxStore(graph.api, storeScope,
        rememberMailbox = { preferences.edit().putString("mailbox", it).apply() },
        onSessionExpired = { signOut() })
    /** Profile pictures over the initials, once the postbox says `avatars` is on. */
    val avatars = AvatarStore(graph.api, storeScope)
    val pictures = AvatarPictures(app, avatars)
    /** End-to-end encrypted groups (Phase 3b), once the postbox says `groups` is on. */
    val groups = graph.groupTransport?.let { GroupStore(it, graph.groupCrypto, graph.groupStorage(app), storeScope, pollSeconds = graph.groupPollSeconds, openPollSeconds = minOf(3, graph.groupPollSeconds), onSessionExpired = { signOut() }) }
    /** Phase 6: audio focus and the foreground service while a live stream is on screen. */
    private val liveMedia = calls?.let { LiveMedia(app, it, viewModelScope) }
    /** Phase 7: this device's meeting-notes key, kept by the Android Keystore. */
    val notesKeys = KeystoreNotesKeys(app, if (graph.fixtures) "fixture-meeting-notes" else "meeting-notes")
    val meetingNotes = graph.groupTransport?.let { transport ->
        MeetingNotesStore(TransportMeetingNotesApi(transport) { inbox.state.value.acting?.address }, { notesKeys.current() }, storeScope,
            onSessionExpired = { signOut() })
    }
    private val notesPage = MutableStateFlow(false)
    val meetingNotesOpen = notesPage.asStateFlow()
    fun openMeetingNotes() { val notes = meetingNotes ?: return; if (!notes.state.value.enabled) return; notes.back(); notes.load(); notesPage.value = true }
    fun closeMeetingNotes() { notesPage.value = false; meetingNotes?.back() }
    val calendar = graph.calendarTransport?.let { CalendarStore(it, graph.calendarStorage(app), storeScope, allowVolatileStorage = graph.fixtures, onSessionExpired = { signOut() }) }
    val meetingLookup = graph.meetingApi?.let { CalendarMeetingLookup(it, storeScope, onSessionExpired = { signOut() }) }
    val deviceApproval = graph.pairingApi?.let { api -> DeviceApprovalStore(api, viewModelScope, identity = {
        val state = session.state.value; val grant = session.authorizationLease; val subject = state.subject
        if (!state.loading && state.signedIn && state.termsAccepted && !subject.isNullOrEmpty() && grant != null && grant.isValid && grant.id == state.authorizationId)
            DeviceApprovalIdentity(subject, grant) else null
    }, transitioning = { session.state.value.loading && session.state.value.busy }, onSessionExpired = { expected ->
        viewModelScope.launch { if (session.authorizationLease === expected) signOut() }
    }) }
    val organizationRooms = graph.organizationRoomApi?.let { api -> OrganizationRoomStore(api, viewModelScope, identity = {
        val state = session.state.value; val grant = session.authorizationLease; val inboxState = inbox.state.value
        val subject = state.subject; val mailbox = inboxState.acting?.address
        if (!state.loading && state.signedIn && state.termsAccepted && !subject.isNullOrEmpty() && mailbox != null &&
            grant?.isValid == true && grant.id == state.authorizationId && activation.value?.authorizationId == state.authorizationId &&
            "orgs" in inboxState.features && "groups" in inboxState.features && groups?.state?.value?.unavailable != true)
            OrganizationRoomIdentity(subject, grant, mailbox) else null
    }, onSessionExpired = { expected -> viewModelScope.launch { if (session.authorizationLease === expected) signOut() } }) }
    val handles = HandleStore(graph.api, storeScope,
        onRegistered = { inbox.loadAccount(); accountHandles?.refresh() }, onSessionExpired = { signOut() })
    val accountHandles = (graph.api as? AccountHandleApi)?.let { AccountHandleStore(it, storeScope, onSessionExpired = { signOut() }) }
    val billing = if (graph.fixtures) null else GooglePlayBilling(app)
    val paidHandles = (graph.api as? PaidHandleApi)?.let { api -> billing?.let {
        PaidHandleStore(api, it, storeScope, onRegistered = { inbox.loadAccount(); accountHandles?.refresh() }, onSessionExpired = { signOut() })
    } }
    private val loadingAttachments = MutableStateFlow<Map<DraftKey, Int>>(emptyMap())
    val stagingAttachments = loadingAttachments.asStateFlow()
    // Hold the rich-content payload until its temporary URI permission has been used.
    private val retainedPastes = mutableMapOf<String, Any>()
    var attachmentTarget: DraftKey? = null
    var pendingSave: File? = null
    // Declared before init: the session collector there can reset these straight away.
    private val stagedForGroups = MutableStateFlow<Map<GroupDraftKey, List<StagedAttachment>>>(emptyMap())
    val groupStaged = stagedForGroups.asStateFlow()
    val profile = ProfileStore(graph.api, storeScope) { keys -> avatars.forget(keys + inbox.state.value.mailboxes.filter { it.address in keys }.mapNotNull { it.handle }) }
    val people = graph.peopleApi?.let { api -> PeopleStore(api, viewModelScope, identity = {
        val state = session.state.value; val grant = session.authorizationLease; val inboxState = inbox.state.value
        val subject = state.subject; val mailbox = inboxState.acting?.address
        if (!state.loading && state.signedIn && state.termsAccepted && !subject.isNullOrEmpty() && mailbox != null &&
            grant?.isValid == true && grant.id == state.authorizationId && activation.value?.authorizationId == state.authorizationId)
            PeopleIdentity(subject, grant, mailbox, inboxState.features) else null
    }, aliases = AndroidLocalAliases(app, graph.fixtures), expired = { expected -> viewModelScope.launch { if (session.authorizationLease === expected) signOut() } }) }
    val pictureForAll = MutableStateFlow(false)
    private val cropping = MutableStateFlow<android.graphics.Bitmap?>(null)
    val crop = cropping.asStateFlow()
    init {
        graph.start(viewModelScope)
        viewModelScope.launch { session.state.collect { deviceApproval?.synchronize(); organizationRooms?.synchronize(); people?.synchronize() } }
        dev.pigeonpost.inbox.push.PushNotifications.groupPoke = { groups?.poke() }
        PushNotifications.calendarPoke = { viewModelScope.launch { calendar?.refresh() } }
        viewModelScope.launch {
            combine(session.state, inbox.state, activation) { session, state, active ->
                if (active != null && active.authorizationId == session.authorizationId && !session.loading && session.signedIn && session.termsAccepted && state.acting != null)
                    InboxBinding(active.id, state.acting!!.address, state.acting!!.handle, state.features) else null
            }.distinctUntilChanged().collect { target ->
                avatars.enable(target != null && Features.AVATARS in target.features)
                groups?.onMailbox(target?.address, target?.handle, target?.features.orEmpty())
                calendar?.onMailbox(target?.address, target?.features.orEmpty())
                meetingNotes?.onMailbox(target?.address, target?.features.orEmpty())
                if (meetingNotes?.state?.value?.enabled != true) notesPage.value = false
                meetingLookup?.onMailbox(target?.address)
                organizationRooms?.synchronize()
                people?.synchronize()
                if (roomPage.value && organizationRooms?.isCurrent() == true && organizationRooms.state.value.catalog == null)
                    organizationRooms.loadOrganizations()
            }
        }
        viewModelScope.launch {
            inbox.state.map { it.leftView == LeftView.CALENDAR }.distinctUntilChanged().collect { calendar?.setOpen(it) }
        }
        calls?.let { host ->
            // A live stream on screen: tell the page what only the postbox knows (a stage invitation,
            // being taken off the stage, the end), as the web inbox does from `group.stream` events.
            viewModelScope.launch {
                host.live.map { it?.streamId }.distinctUntilChanged().collectLatest { streamId ->
                    if (streamId == null) return@collectLatest
                    var before: GroupStream? = null
                    while (true) {
                        val store = groups ?: break
                        val me = inbox.state.value.acting?.address ?: break
                        val check = store.checkStream(streamId)
                        host.forwardStream(GroupStreams.pageEvents(before, check, me, streamId))
                        if (check is StreamCheck.Live) before = check.stream
                        if (check is StreamCheck.Ended) break
                        delay(graph.streamPollMillis)
                    }
                }
            }
            viewModelScope.launch {
                combine(session.state, inbox.state, activation) { session, state, active ->
                    if (active != null && active.authorizationId == session.authorizationId && !session.loading && session.signedIn && session.termsAccepted && state.acting != null)
                        CallBinding(active.id, state.acting!!, state.conversations.associate { it.peer to it.name }) else null
                }.distinctUntilChanged().collect { target ->
                    if (target != null) host.attach(target.mailbox, target.names) else host.signOut()
                }
            }
        }
        viewModelScope.launch {
            combine(session.state, inbox.state) { session, state ->
                if (!session.loading && session.signedIn && session.termsAccepted) state.acting?.address else null
            }.distinctUntilChanged().collect { push?.update(it) }
        }
        viewModelScope.launch {
            session.state.filter { !it.loading }.map { (it.signedIn && it.termsAccepted) to it.authorizationId }.distinctUntilChanged().collect { (ready, authorizationId) ->
                resetStores()
                withContext(Dispatchers.IO) { files.clear() }
                val current = session.state.value
                val grant = session.authorizationLease
                if (ready && current.signedIn && current.termsAccepted && current.authorizationId == authorizationId && grant?.id == authorizationId && grant?.isValid != false) {
                    try { storeScope.activate(grant) } catch (_: AuthorizationRetired) { return@collect }
                    if (session.authorizationLease !== grant || grant?.isValid == false || session.state.value.authorizationId != authorizationId) {
                        storeScope.retire(); return@collect
                    }
                    activation.value = StoreActivation(authorizationId)
                    inbox.loadAccount(preferences.getString("mailbox", null)); accountHandles?.refresh(); paidHandles?.restore()
                }
            }
        }
    }
    fun completeSignIn(intent: Intent?) { viewModelScope.launch { session.complete(intent) } }
    fun acceptTerms() { viewModelScope.launch { session.acceptTerms() } }
    fun signOut() {
        roomPage.value = false; cancelRoomPreparation(); organizationRooms?.reset()
        closeDeviceApproval()
        closeMeetingLink()
        calls?.stopStream()
        viewModelScope.launch(Dispatchers.IO) { runCatching { notesKeys.destroy() } }
        val authorization = session.authorizationLease
        val token = push?.clear()
        people?.reset(clearAliases = true)
        resetStores()
        val cleanup = authorization?.takeIf { it.isValid }?.child()
        viewModelScope.launch(AuthorizationContext(cleanup)) {
            try { push?.unregister(graph.api as? DevicePushApi, token) { session.authorizationLease === authorization } }
            finally { session.signOut(authorization) }
        }
    }
    private fun resetStores() {
        people?.reset()
        roomPage.value = false; cancelRoomPreparation(); organizationRooms?.reset()
        storeScope.retire()
        activation.value = null
        inbox.reset(); handles.reset(); paidHandles?.reset(); accountHandles?.reset(); avatars.reset(); groups?.reset(); calendar?.reset(); meetingLookup?.reset(); meetingNotes?.reset(); notesPage.value = false; profile.reset(); pictures.clear()
        if (meetingIntentRoot?.isValid == false) closeMeetingLink()
        stagedForGroups.value.values.flatten().forEach { it.file.delete() }
        stagedForGroups.value = emptyMap(); attachmentTarget = null; groupAttachTarget = null; pendingSave = null
        loadingAttachments.value = emptyMap(); retainedPastes.clear(); cropping.value = null
    }
    fun refreshPushRegistration() {
        push?.update(inbox.state.value.acting?.address?.takeIf { session.state.value.signedIn && session.state.value.termsAccepted })
    }
    /** One thing open at a time: a direct conversation or a group. */
    fun openPeer(peer: String?) { groups?.close(); inbox.selectPeer(peer) }
    /** A new conversation replaces whatever the detail pane showed, a group or the calendar included. */
    fun startConversation(peer: String) {
        groups?.close()
        if (inbox.state.value.leftView == LeftView.CALENDAR) inbox.showView(LeftView.INBOX)
        inbox.startConversation(peer)
    }
    /** The store has already opened the new group in its own mailbox; nothing else may cover it. */
    fun groupCreated() { inbox.selectPeer(null); if (inbox.state.value.leftView == LeftView.CALENDAR) inbox.showView(LeftView.INBOX) }
    fun openGroup(id: String) { inbox.selectPeer(null); inbox.showView(LeftView.INBOX); groups?.open(id) }
    fun openCalendar() { groups?.close(); inbox.selectPeer(null); inbox.showView(LeftView.CALENDAR) }
    fun joinCalendar(key: CalendarEventKey, video: Boolean, expectedContext: String) {
        val calendar = calendar ?: return
        val host = calls ?: return
        calendar.prepareScheduledJoin(key, video, expectedContext) { request ->
            val state = calendar.state.value
            if (state.contextId == expectedContext && state.openEvent == key) {
                calendar.closeEvent(); host.joinScheduled(request)
            } else request.close()
        }
    }
    fun closeMeetingLink() { meetingCode.value = null; meetingIntentRoot = null; meetingLookup?.close() }
    fun openOrganizationRooms() {
        val store = organizationRooms ?: return
        store.synchronize()
        if (!store.isCurrent()) { inbox.showError("Organization rooms are unavailable for this inbox."); return }
        roomPage.value = true; store.loadOrganizations()
    }
    fun closeOrganizationRooms(context: String, discardUnconfirmed: Boolean = false): Boolean {
        val store = organizationRooms ?: return false
        if (context != store.state.value.contextId || roomPreparation.value || !store.close(discardUnconfirmed)) return false
        roomPage.value = false; return true
    }
    fun switchOrganizationMailbox(address: String, context: String): Boolean {
        val store = organizationRooms ?: return false
        if (!store.isCurrent(context) || store.state.value.busy || roomPreparation.value ||
            calls?.mode?.value?.let { it != CallHost.Mode.HIDDEN } == true || calls?.preparingScheduled?.value == true) return false
        val target = inbox.state.value.mailboxes.firstOrNull { it.address == address } ?: return false
        if (store.state.value.catalog?.orgs?.any { it.readable && it.mailbox?.address == address } != true) return false
        store.reset(); inbox.switchMailbox(target); return true
    }
    private fun cancelRoomPreparation() {
        roomPreparationId = java.util.UUID.randomUUID(); roomPreparationJob?.cancel(); roomPreparationJob = null; roomPreparation.value = false
    }
    fun prepareOrganizationRoom(opening: OrganizationRoomOpening) {
        val store = organizationRooms ?: return; val groups = groups ?: return
        if (!store.allows(opening) || roomPreparation.value) return
        val root = session.authorizationLease ?: return
        val mailbox = inbox.state.value.acting?.address ?: return
        val child = try { root.child() } catch (_: dev.pigeonpost.core.AuthorizationRetired) { return }
        val visit = java.util.UUID.randomUUID(); roomPreparationId = visit; roomPreparation.value = true
        fun current() = store.allows(opening) && root.isValid && session.authorizationLease === root && inbox.state.value.acting?.address == mailbox
        val job = viewModelScope.launch(dev.pigeonpost.core.AuthorizationContext(child), start = kotlinx.coroutines.CoroutineStart.LAZY) {
            try {
                val prepared = withTimeoutOrNull(15_000) {
                    groups.state.first { !current() || it.unavailable || it.enabled && it.mailbox == mailbox }
                    if (!current() || !groups.state.value.enabled || groups.state.value.unavailable) false
                    else groups.prepareOrganizationRoom(opening.groupId, opening.organizationId, groups.state.value.contextId)
                } == true
                if (current()) {
                    if (prepared) { inbox.selectPeer(null); inbox.showView(LeftView.INBOX); store.openingCompleted(opening); store.reset(); roomPage.value = false }
                    else store.openingFailed(opening)
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { if (current()) store.openingFailed(opening, failure) }
            finally { child.retire(); if (roomPreparationId == visit) { roomPreparationJob = null; roomPreparation.value = false } }
        }
        roomPreparationJob = job; job.start()
    }
    fun closeDeviceApproval() { deviceNavigation?.cancel(); deviceNavigation = null; machineOpen.value = false; deviceApproval?.close() }
    fun openDeviceApproval(code: DevicePairingCode? = null) {
        val store = deviceApproval ?: return
        closeDeviceApproval()
        val root = session.authorizationLease
        calendarNoticeId = java.util.UUID.randomUUID(); val visit = calendarNoticeId
        calendarNoticeJob?.cancel(); calendar?.noticeWaiting(false)
        val job = viewModelScope.launch(start = kotlinx.coroutines.CoroutineStart.LAZY) {
            try {
                val calendar = calendar
                if (calendar != null && (calendar.state.value.editing || calendar.state.value.openEvent != null)) {
                    calendar.noticeWaiting(true)
                    calendar.state.first { visit != calendarNoticeId || root?.isValid == false || !it.editing && it.openEvent == null }
                }
                combine(session.state, inbox.state) { s, i -> s to i }.first { (s, i) ->
                    visit != calendarNoticeId || root?.let { !it.isValid || session.authorizationLease !== it } == true ||
                        !s.loading && s.signedIn && s.termsAccepted && i.accountLoaded
                }
                if (visit != calendarNoticeId || root?.let { !it.isValid || session.authorizationLease !== it } == true) return@launch
                if ("orgs" !in inbox.state.value.features) { inbox.showError("Machine sign-in is unavailable on this server."); return@launch }
                store.synchronize()
                if (!store.isCurrent()) { inbox.showError("Sign in again to verify your account before reviewing a machine."); return@launch }
                machineOpen.value = true
                code?.let { store.resolve(it.displayValue, store.state.value.contextId) }
            } finally { if (visit == calendarNoticeId) calendar?.noticeWaiting(false) }
        }
        deviceNavigation = job; calendarNoticeJob = job; job.start()
    }
    fun verifyDeviceSignIn(proof: String, context: String) {
        if (calls?.mode?.value?.let { it != CallHost.Mode.HIDDEN } == true || calls?.preparingScheduled?.value == true) {
            inbox.showError("Finish your call before verifying sign-in."); return
        }
        deviceApproval?.verifySignIn(proof, context) { requirement, expected ->
            session.verifyStrongSignIn(requirement, expected) { intent -> authorizationWindow?.invoke(intent) ?: throw dev.pigeonpost.core.StrongSignInException() }
        }
    }
    fun showMeetingCode(input: String) {
        val code = CalendarMeetingCode.parse(input) ?: return
        val root = session.authorizationLease
        calendarNoticeId = java.util.UUID.randomUUID(); val visit = calendarNoticeId
        calendarNoticeJob?.cancel(); calendar?.noticeWaiting(false)
        calendarNoticeJob = viewModelScope.launch {
            try {
                val calendar = calendar
                if (calendar != null && (calendar.state.value.editing || calendar.state.value.openEvent != null)) {
                    calendar.noticeWaiting(true)
                    combine(session.state, calendar.state) { _, state -> state }.first {
                        visit != calendarNoticeId || root?.let { !it.isValid || session.authorizationLease !== it } == true || !it.editing && it.openEvent == null
                    }
                }
                if (visit != calendarNoticeId || root?.let { !it.isValid || session.authorizationLease !== it } == true) return@launch
                meetingLookup?.close(); meetingIntentRoot = root; meetingCode.value = code
                val lookup = meetingLookup
                val snapshot = lookup?.state?.value
                if (snapshot?.mailbox == inbox.state.value.acting?.address && snapshot != null && session.state.value.signedIn && session.state.value.termsAccepted)
                    lookup.load(code, snapshot.contextId)
            } finally { if (visit == calendarNoticeId) calendar?.noticeWaiting(false) }
        }
    }
    fun joinMeetingLink(occurrence: String, video: Boolean, context: String, code: String) {
        if (meetingCode.value != code) return
        val host = calls ?: return
        meetingLookup?.prepareJoin(occurrence, video, context, code) { request -> closeMeetingLink(); host.joinScheduled(request) }
    }
    fun setActive(value: Boolean) { inbox.setActive(value); groups?.setActive(value); calendar?.setActive(value) }

    /** `https://inbox.pigeonpost.dev/g/<id>#<token>`: show the group before anything is joined. */
    fun openGroupLink(uri: Uri?) {
        val link = GroupLinks.parse(uri?.toString()) ?: return
        groups?.resolveJoin(link.first, link.second)
    }

    /** A tap requests calendar navigation only; membership and each detail are reread as the owner. */
    fun openCalendarNotice(payload: Map<String, String>) {
        val notice = CalendarNotice.parse(payload) ?: return
        val calendar = calendar ?: return
        closeMeetingLink()
        calendarNoticeId = java.util.UUID.randomUUID()
        val visit = calendarNoticeId
        calendarNoticeJob?.cancel(); calendar.noticeWaiting(false)
        calendarNoticeJob = viewModelScope.launch {
            val signedIn = session.state.first { !it.loading }
            if (!signedIn.signedIn || !signedIn.termsAccepted) return@launch
            val root = session.authorizationLease ?: return@launch
            fun current() = visit == calendarNoticeId && root.isValid && session.authorizationLease === root && session.state.value.signedIn && session.state.value.termsAccepted
            try {
                val ready = withTimeoutOrNull(30_000) {
                    combine(session.state, inbox.state) { _, inbox -> inbox }.first { !current() || it.accountLoaded && it.loaded }
                } ?: return@launch
                if (!current() || ready.mailboxes.none { it.address == notice.mailbox } || "calendar" !in ready.features) return@launch
                // Even a notice for another owned mailbox waits for the current draft/detail.
                if (calendar.state.value.editing || calendar.state.value.openEvent != null) {
                    calendar.noticeWaiting(true)
                    combine(session.state, calendar.state) { _, state -> state }.first { !current() || !it.editing && it.openEvent == null }
                }
                if (!current()) return@launch
                val mailbox = inbox.state.value.mailboxes.firstOrNull { it.address == notice.mailbox } ?: return@launch
                if (inbox.state.value.acting?.address != notice.mailbox) inbox.switchMailbox(mailbox)
                val loaded = withTimeoutOrNull(30_000) {
                    combine(session.state, inbox.state) { _, state -> state }.first { !current() || it.acting?.address != notice.mailbox || it.loaded }
                } ?: return@launch
                if (!current() || loaded.acting?.address != notice.mailbox || "calendar" !in loaded.features) return@launch
                val actor = withTimeoutOrNull(30_000) {
                    combine(session.state, calendar.state) { _, state -> state }.first { !current() || it.mailbox == notice.mailbox && it.enabled }
                } ?: return@launch
                if (!current() || actor.mailbox != notice.mailbox || inbox.state.value.acting?.address != notice.mailbox ||
                    calendar.state.value.contextId != actor.contextId) return@launch
                openCalendar(); calendar.refresh(); notice.event?.let(calendar::openEvent)
            } finally { if (visit == calendarNoticeId) calendar.noticeWaiting(false) }
        }
    }

    fun openNotification(identity: String?, peer: String?) {
        if (identity != null && peer != null && validAddress(identity) && GroupLinks.isGroupId(peer)) {
            viewModelScope.launch {
                val signedIn = session.state.first { !it.loading }
                if (!signedIn.signedIn || !signedIn.termsAccepted) return@launch
                val ready = inbox.state.first { it.accountLoaded || !session.state.value.signedIn }
                val mailbox = ready.mailboxes.firstOrNull { it.address == identity } ?: return@launch
                if (ready.acting?.address != identity) inbox.switchMailbox(mailbox)
                val store = groups ?: return@launch
                store.state.first { (it.enabled || it.unavailable) && it.mailbox == identity }
                store.refresh()
                if (store.state.value.groups.any { it.id == peer }) openGroup(peer)
            }
            return
        }
        if (identity == null || peer == null || !validAddress(identity) || !validAddress(peer)) return
        viewModelScope.launch {
            val signedIn = session.state.first { !it.loading }
            if (!signedIn.signedIn || !signedIn.termsAccepted) return@launch
            val ready = inbox.state.first { it.accountLoaded || !session.state.value.signedIn }
            if (!session.state.value.signedIn) return@launch
            val mailbox = ready.mailboxes.firstOrNull { it.address == identity } ?: return@launch
            if (ready.acting?.address != identity) inbox.switchMailbox(mailbox)
            openPeer(peer)
        }
    }
    override fun onCleared() {
        cancelRoomPreparation(); organizationRooms?.reset()
        deviceNavigation?.cancel(); deviceApproval?.close(); authorizationWindow = null
        storeScope.retire()
        dev.pigeonpost.inbox.push.PushNotifications.groupPoke = null; PushNotifications.calendarPoke = null; calendarNoticeJob?.cancel(); billing?.close(); calls?.destroy(); super.onCleared() }
    fun chooseAttachments() { attachmentTarget = inbox.state.value.draftKey; groupAttachTarget = null }

    // ---- group composer files (sealed and uploaded when the message is sent) -----------------
    private data class GroupAttachmentTarget(val key: GroupDraftKey, val contextId: String)
    private var groupAttachTarget: GroupAttachmentTarget? = null
    fun chooseGroupAttachments(groupId: String) {
        val state = groups?.state?.value ?: return
        val mailbox = state.mailbox ?: return
        if (state.sending || state.unconfirmed(groupId) || state.composers[groupId]?.kind == GroupComposerKind.EDIT) return
        groupAttachTarget = GroupAttachmentTarget(GroupDraftKey(mailbox, groupId), state.contextId); attachmentTarget = null
    }
    fun unstageGroupFile(groupId: String, id: String) {
        val state = groups?.state?.value ?: return
        val key = GroupDraftKey(state.mailbox ?: return, groupId)
        if (state.sending || state.unconfirmed(groupId)) return
        stagedForGroups.value[key].orEmpty().firstOrNull { it.id == id }?.file?.delete()
        stagedForGroups.value = stagedForGroups.value + (key to stagedForGroups.value[key].orEmpty().filterNot { it.id == id })
    }
    private fun stageGroup(target: GroupAttachmentTarget, uris: List<Uri>) {
        storeScope.launch {
            try {
                val added = files.stage(uris)
                val state = groups?.state?.value
                if (state?.contextId != target.contextId || state.mailbox != target.key.mailbox || state.sending || state.unconfirmed(target.key.groupId)) {
                    added.forEach { it.file.delete() }; return@launch
                }
                val all = stagedForGroups.value[target.key].orEmpty() + added
                if (all.size > MAX_ATTACHMENTS) { added.forEach { it.file.delete() }; inbox.showError("Choose at most $MAX_ATTACHMENTS attachments."); return@launch }
                stagedForGroups.value = stagedForGroups.value + (target.key to all)
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { inbox.showError(failure.message ?: "Could not read the attachment.") }
        }
    }
    fun sendGroup(groupId: String, text: String, done: (Boolean) -> Unit = {}) {
        val store = groups ?: return
        val state = store.state.value
        val key = GroupDraftKey(state.mailbox ?: return, groupId)
        val staged = if (state.composers[groupId]?.kind == GroupComposerKind.EDIT) emptyList() else stagedForGroups.value[key].orEmpty()
        store.sendPrepared(groupId, text, {
            withContext(Dispatchers.IO) { staged.map { dev.pigeonpost.core.groups.OutgoingFile(it.name, it.mediaType, it.file.readBytes()) } }
        }) { ok ->
            if (store.state.value.contextId == state.contextId) {
                if (ok && staged.isNotEmpty()) {
                    staged.forEach { it.file.delete() }
                    stagedForGroups.value = stagedForGroups.value + (key to stagedForGroups.value[key].orEmpty().filterNot { item -> staged.any { it.id == item.id } })
                }
                done(ok)
            }
        }
    }
    fun retryGroup(groupId: String, cmid: String) {
        val store = groups ?: return
        val state = store.state.value
        val key = GroupDraftKey(state.mailbox ?: return, groupId)
        val edit = state.history[groupId].orEmpty().any { it.cmid == cmid && it.editOf != null }
        val staged = if (edit) emptyList() else stagedForGroups.value[key].orEmpty()
        store.retry(groupId, cmid) { ok ->
            if (ok && store.state.value.contextId == state.contextId && staged.isNotEmpty()) {
                staged.forEach { it.file.delete() }
                stagedForGroups.value = stagedForGroups.value + (key to stagedForGroups.value[key].orEmpty().filterNot { item -> staged.any { it.id == item.id } })
            }
        }
    }
    /**
     * The bytes of a file: a direct-message attachment from the postbox, or a group file
     * (`gfile:<group>|<file>`) fetched sealed and decrypted here.
     */
    suspend fun fetch(identity: String, value: Attachment): File {
        if (!value.id.startsWith("gfile:")) return files.download(identity, value)
        val (gid, fid) = value.id.removePrefix("gfile:").split('|', limit = 2).let { it[0] to it.getOrElse(1) { "" } }
        val file = groups?.state?.value?.history?.get(gid).orEmpty().flatMap { it.files }.firstOrNull { it.id == fid } ?: error("This file is no longer available.")
        return openGroupFile(gid, file)
    }
    /** A group file, decrypted on this device into the private cache, for the viewer or to share. */
    suspend fun openGroupFile(groupId: String, file: dev.pigeonpost.core.groups.GroupFile): File {
        val bytes = groups?.openFile(groupId, file) ?: error("Groups are not available.")
        return files.received(file.name, bytes)
    }

    // ---- profile picture ------------------------------------------------------------------------
    fun pictureChosen(uri: Uri?) {
        if (uri == null) return
        storeScope.launch {
            try {
                val bitmap = dev.pigeonpost.inbox.ui.decodeForCrop(getApplication(), uri)
                kotlin.coroutines.coroutineContext[AuthorizationContext]?.lease?.requireActive()
                cropping.value = bitmap
            }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { inbox.showError("That picture could not be opened. Try a JPEG or PNG.") }
        }
    }
    fun cancelCrop() { cropping.value = null }
    fun saveCrop(left: Int, top: Int, side: Int) {
        val bitmap = cropping.value ?: return
        storeScope.launch {
            val (bytes, size) = withContext(Dispatchers.Default) { dev.pigeonpost.inbox.ui.encodeCrop(bitmap, left, top, side) }
            kotlin.coroutines.coroutineContext[AuthorizationContext]?.lease?.requireActive()
            profile.upload(bytes, "image/jpeg", size, pictureForAll.value, inbox.state.value.mailboxes.map { it.address }) { cropping.value = null }
        }
    }
    fun attach(uris: List<Uri>) {
        groupAttachTarget?.let { gid -> groupAttachTarget = null; stageGroup(gid, uris); return }
        val target = attachmentTarget ?: return
        attachmentTarget = null
        stage(target, uris)
    }
    fun pasteAttachments(uris: List<Uri>, payload: Any) {
        val target = inbox.state.value.draftKey ?: return
        if (inbox.state.value.isSending) return
        val token = java.util.UUID.randomUUID().toString()
        retainedPastes[token] = payload
        stage(target, uris) { retainedPastes.remove(token) }
    }
    private fun stage(target: DraftKey, uris: List<Uri>, finished: () -> Unit = {}) {
        if (uris.isEmpty()) { finished(); return }
        loadingAttachments.value = loadingAttachments.value + (target to (loadingAttachments.value.getOrDefault(target, 0) + 1))
        storeScope.launch {
            try {
                val added = files.stage(uris)
                try { kotlin.coroutines.coroutineContext[AuthorizationContext]?.lease?.requireActive() }
                catch (retired: AuthorizationRetired) { added.forEach { it.file.delete() }; throw retired }
                inbox.stage(target, added)
            }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { inbox.showError(failure.message ?: "Could not read the attachment.") }
            finally {
                finished()
                val left = loadingAttachments.value.getOrDefault(target, 1) - 1
                loadingAttachments.value = if (left > 0) loadingAttachments.value + (target to left) else loadingAttachments.value - target
            }
        }
    }
}
