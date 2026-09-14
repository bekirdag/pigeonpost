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

data class AppGraph(val session: UserSession, val api: PostboxApi, val fixtures: Boolean = false) {
    companion object {
        fun live(app: Application): AppGraph {
            val session = Session(app)
            return AppGraph(session, PostboxClient(session))
        }
    }
}
class PigeonpostApplication : Application() { val graph by lazy { AppGraph.live(this) } }

class InboxViewModel(app: Application, val graph: AppGraph) : AndroidViewModel(app) {
    val session = graph.session
    val push = if (graph.fixtures) null else PushNotifications(app)
    private val preferences = app.getSharedPreferences(if (graph.fixtures) "fixture-settings" else "settings", 0)
    val files = PlatformFiles(app, graph.api)
    val inbox = InboxStore(graph.api, viewModelScope,
        rememberMailbox = { preferences.edit().putString("mailbox", it).apply() },
        onSessionExpired = { signOut() })
    val handles = HandleStore(graph.api, viewModelScope,
        onRegistered = { inbox.loadAccount(); accountHandles?.refresh() }, onSessionExpired = { signOut() })
    val accountHandles = (graph.api as? AccountHandleApi)?.let { AccountHandleStore(it, viewModelScope, onSessionExpired = { signOut() }) }
    val billing = if (graph.fixtures) null else GooglePlayBilling(app)
    val paidHandles = (graph.api as? PaidHandleApi)?.let { api -> billing?.let {
        PaidHandleStore(api, it, viewModelScope, onRegistered = { inbox.loadAccount(); accountHandles?.refresh() }, onSessionExpired = { signOut() })
    } }
    var attachmentTarget: DraftKey? = null
    var pendingSave: File? = null
    init {
        viewModelScope.launch {
            combine(session.state, inbox.state) { session, state ->
                if (!session.loading && session.signedIn && session.termsAccepted) state.acting?.address else null
            }.distinctUntilChanged().collect { push?.update(it) }
        }
        viewModelScope.launch {
            session.state.filter { !it.loading }.map { it.signedIn && it.termsAccepted }.distinctUntilChanged().collect { ready ->
                if (ready) { inbox.loadAccount(preferences.getString("mailbox", null)); accountHandles?.refresh(); paidHandles?.restore() }
                else { inbox.reset(); handles.reset(); paidHandles?.reset(); accountHandles?.reset(); attachmentTarget = null; pendingSave = null; withContext(Dispatchers.IO) { files.clear() } }
            }
        }
    }
    fun completeSignIn(intent: Intent?) { viewModelScope.launch { session.complete(intent) } }
    fun acceptTerms() { viewModelScope.launch { session.acceptTerms() } }
    fun signOut() {
        val token = push?.clear()
        handles.reset(); paidHandles?.reset(); accountHandles?.reset(); inbox.reset()
        viewModelScope.launch { push?.unregister(graph.api as? DevicePushApi, token); session.signOut() }
    }
    fun refreshPushRegistration() {
        push?.update(inbox.state.value.acting?.address?.takeIf { session.state.value.signedIn && session.state.value.termsAccepted })
    }
    fun openNotification(identity: String?, peer: String?) {
        if (identity == null || peer == null || !validAddress(identity) || !validAddress(peer)) return
        viewModelScope.launch {
            val signedIn = session.state.first { !it.loading }
            if (!signedIn.signedIn || !signedIn.termsAccepted) return@launch
            val ready = inbox.state.first { it.accountLoaded || !session.state.value.signedIn }
            if (!session.state.value.signedIn) return@launch
            val mailbox = ready.mailboxes.firstOrNull { it.address == identity } ?: return@launch
            if (ready.acting?.address != identity) inbox.switchMailbox(mailbox)
            inbox.selectPeer(peer)
        }
    }
    override fun onCleared() { billing?.close(); super.onCleared() }
    fun chooseAttachments() { attachmentTarget = inbox.state.value.draftKey }
    fun attach(uris: List<Uri>) {
        val target = attachmentTarget ?: return
        attachmentTarget = null
        if (uris.isEmpty()) return
        viewModelScope.launch {
            try { inbox.stage(target, files.stage(uris)) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { inbox.showError(failure.message ?: "Could not read the attachment.") }
        }
    }
}
