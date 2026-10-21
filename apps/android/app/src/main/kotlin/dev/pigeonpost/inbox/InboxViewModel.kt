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
import dev.pigeonpost.core.groups.GroupTransport
import dev.pigeonpost.core.groups.UniffiGroupCrypto
import dev.pigeonpost.inbox.groups.KeystoreGroupStorage
import kotlinx.coroutines.CoroutineScope

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
    val calls = if (graph.fixtures && callsOverride == null) null else CallHost(app, graph.session, viewModelScope, callsOverride)
    val inbox = InboxStore(graph.api, viewModelScope,
        rememberMailbox = { preferences.edit().putString("mailbox", it).apply() },
        onSessionExpired = { signOut() })
    /** Profile pictures over the initials, once the postbox says `avatars` is on. */
    val avatars = AvatarStore(graph.api, viewModelScope)
    val pictures = AvatarPictures(app, avatars)
    /** End-to-end encrypted groups (Phase 3b), once the postbox says `groups` is on. */
    val groups = graph.groupTransport?.let { GroupStore(it, graph.groupCrypto, graph.groupStorage(app), viewModelScope, onSessionExpired = { signOut() }) }
    val handles = HandleStore(graph.api, viewModelScope,
        onRegistered = { inbox.loadAccount(); accountHandles?.refresh() }, onSessionExpired = { signOut() })
    val accountHandles = (graph.api as? AccountHandleApi)?.let { AccountHandleStore(it, viewModelScope, onSessionExpired = { signOut() }) }
    val billing = if (graph.fixtures) null else GooglePlayBilling(app)
    val paidHandles = (graph.api as? PaidHandleApi)?.let { api -> billing?.let {
        PaidHandleStore(api, it, viewModelScope, onRegistered = { inbox.loadAccount(); accountHandles?.refresh() }, onSessionExpired = { signOut() })
    } }
    private val loadingAttachments = MutableStateFlow<Map<DraftKey, Int>>(emptyMap())
    val stagingAttachments = loadingAttachments.asStateFlow()
    // Hold the rich-content payload until its temporary URI permission has been used.
    private val retainedPastes = mutableMapOf<String, Any>()
    var attachmentTarget: DraftKey? = null
    var pendingSave: File? = null
    init {
        graph.start(viewModelScope)
        viewModelScope.launch {
            combine(session.state, inbox.state) { session, state ->
                if (!session.loading && session.signedIn && session.termsAccepted && state.acting != null) Triple(state.acting!!.address, state.acting!!.handle, state.features) else null
            }.distinctUntilChanged().collect { target ->
                avatars.enable(target != null && Features.AVATARS in target.third)
                groups?.onMailbox(target?.first, target?.second, target?.third.orEmpty())
            }
        }
        calls?.let { host ->
            viewModelScope.launch {
                combine(session.state, inbox.state) { session, state ->
                    if (!session.loading && session.signedIn && session.termsAccepted && state.acting != null)
                        state.acting to state.conversations.associate { it.peer to it.name } else null
                }.distinctUntilChanged().collect { target ->
                    if (target != null) host.attach(target.first!!, target.second) else host.signOut()
                }
            }
        }
        viewModelScope.launch {
            combine(session.state, inbox.state) { session, state ->
                if (!session.loading && session.signedIn && session.termsAccepted) state.acting?.address else null
            }.distinctUntilChanged().collect { push?.update(it) }
        }
        viewModelScope.launch {
            session.state.filter { !it.loading }.map { it.signedIn && it.termsAccepted }.distinctUntilChanged().collect { ready ->
                if (ready) { inbox.loadAccount(preferences.getString("mailbox", null)); accountHandles?.refresh(); paidHandles?.restore() }
                else { inbox.reset(); handles.reset(); paidHandles?.reset(); accountHandles?.reset(); avatars.reset(); groups?.reset(); attachmentTarget = null; pendingSave = null; withContext(Dispatchers.IO) { files.clear() } }
            }
        }
    }
    fun completeSignIn(intent: Intent?) { viewModelScope.launch { session.complete(intent) } }
    fun acceptTerms() { viewModelScope.launch { session.acceptTerms() } }
    fun signOut() {
        val token = push?.clear()
        handles.reset(); paidHandles?.reset(); accountHandles?.reset(); inbox.reset(); avatars.reset(); groups?.reset()
        viewModelScope.launch { push?.unregister(graph.api as? DevicePushApi, token); session.signOut() }
    }
    fun refreshPushRegistration() {
        push?.update(inbox.state.value.acting?.address?.takeIf { session.state.value.signedIn && session.state.value.termsAccepted })
    }
    /** One thing open at a time: a direct conversation or a group. */
    fun openPeer(peer: String?) { groups?.close(); inbox.selectPeer(peer) }
    fun openGroup(id: String) { inbox.selectPeer(null); inbox.showView(LeftView.INBOX); groups?.open(id) }
    fun setActive(value: Boolean) { inbox.setActive(value); groups?.setActive(value) }

    /** `https://inbox.pigeonpost.dev/g/<id>#<token>`: show the group before anything is joined. */
    fun openGroupLink(uri: Uri?) {
        val link = GroupLinks.parse(uri?.toString()) ?: return
        groups?.resolveJoin(link.first, link.second)
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
    override fun onCleared() { billing?.close(); calls?.destroy(); super.onCleared() }
    fun chooseAttachments() { attachmentTarget = inbox.state.value.draftKey }
    fun attach(uris: List<Uri>) {
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
        viewModelScope.launch {
            try { inbox.stage(target, files.stage(uris)) }
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
