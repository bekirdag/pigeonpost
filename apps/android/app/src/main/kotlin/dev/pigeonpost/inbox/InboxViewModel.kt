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
    var attachmentTarget: DraftKey? = null
    var pendingSave: File? = null
    init {
        viewModelScope.launch {
            session.state.filter { !it.loading }.map { it.signedIn }.distinctUntilChanged().collect { signedIn ->
                if (signedIn) inbox.loadAccount(preferences.getString("mailbox", null))
                else { inbox.reset(); attachmentTarget = null; pendingSave = null; withContext(Dispatchers.IO) { files.clear() } }
            }
        }
    }
    fun completeSignIn(intent: Intent?) { viewModelScope.launch { session.complete(intent) } }
    fun signOut() { inbox.reset(); viewModelScope.launch { session.signOut() } }
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
