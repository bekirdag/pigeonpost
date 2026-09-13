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
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import dev.pigeonpost.inbox.billing.GooglePlayBilling

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
            session.state.filter { !it.loading }.map { it.signedIn && it.termsAccepted }.distinctUntilChanged().collect { ready ->
                if (ready) { inbox.loadAccount(preferences.getString("mailbox", null)); accountHandles?.refresh(); paidHandles?.restore() }
                else { inbox.reset(); handles.reset(); paidHandles?.reset(); accountHandles?.reset(); attachmentTarget = null; pendingSave = null; withContext(Dispatchers.IO) { files.clear() } }
            }
        }
    }
    fun completeSignIn(intent: Intent?) { viewModelScope.launch { session.complete(intent) } }
    fun acceptTerms() { viewModelScope.launch { session.acceptTerms() } }
    fun signOut() { handles.reset(); paidHandles?.reset(); accountHandles?.reset(); inbox.reset(); viewModelScope.launch { session.signOut() } }
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
