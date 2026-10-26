//  Voice and video calls on Android.
//
//  The calls are the web inbox's own (site-inbox/calls.js), run in an embedded WebView exactly as the
//  iPhone and iPad app does (apps/ios/Shared/Calls/CallHost.swift): the same client rings, answers,
//  runs group calls and meeting links, chat, device choice and noise removal. Chromium carries the
//  media, encrypted end to end (DTLS-SRTP).
//
//  The app owns sign-in. The page (native-call.html + native-call.js) is handed the member token and
//  the mailbox in use, asks the app when the token needs renewing, and says how much of the screen
//  the call needs: all of it, a strip while the person reads mail during a call, or none. While idle
//  it stays loaded and invisible, so an incoming call rings while the app is open.
//
//  With the app off screen or closed, rings come as notifications (IncomingCalls, Phase 1c): the page
//  is configured with `nativeRing` then, keeps the ring state, and reports `ring`/`ring_ended` here.
//  Answering a notification hands the page `PigeonpostNative.answer(room, ringToken)`.
//
//  Not covered: sending this device's screen (Android WebView has no getDisplayMedia). Others' shared
//  screens are shown.
package dev.pigeonpost.inbox.calls

import android.Manifest
import android.annotation.SuppressLint
import android.app.Application
import android.content.MutableContextWrapper
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.webkit.JavascriptInterface
import android.webkit.PermissionRequest
import android.webkit.RenderProcessGoneDetail
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.core.content.ContextCompat
import dev.pigeonpost.core.Mailbox
import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.ScopedTokenProvider
import dev.pigeonpost.core.calendar.CalendarScheduledJoin
import dev.pigeonpost.inbox.auth.UserSession
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import org.json.JSONObject
import java.util.UUID

/** A debug build's stand-in page, token and mailbox for the local call harness. Never in a release. */
data class CallsOverride(val page: String, val token: String?, val mailbox: String?, val handle: String?, val autoCall: String?)

/** What the activity provides: runtime permissions, links out, and short notices. */
interface CallWindow {
    fun requestPermissions(permissions: Array<String>, done: (Map<String, Boolean>) -> Unit)
    fun openExternal(uri: Uri)
    fun toast(text: String)
}

class CallHost(private val app: Application, private val session: UserSession, private val scope: CoroutineScope,
    private val override: CallsOverride?, private val permissionCheck: ((String) -> Boolean)? = null) {
    enum class Mode { HIDDEN, MINI, FULL }

    private val mutableMode = MutableStateFlow(Mode.HIDDEN)
    val mode: StateFlow<Mode> = mutableMode.asStateFlow()

    val page: Uri = Uri.parse(override?.page ?: PRODUCTION)
    private val main = Handler(Looper.getMainLooper())
    /** The view outlives an activity (rotation), so it is built on a context that can be re-pointed. */
    val context = MutableContextWrapper(app)
    private var view: WebView? = null
    private var ready = false
    private data class CallContext(val binding: UUID, val page: UUID?, val mailbox: String, val grant: AuthorizationLease?, val actor: AuthorizationLease?)
    private class PendingCall(val context: CallContext, val script: String)
    private class PendingAnswer(val room: String, val token: String?, val kind: String, val identity: String,
                                val grant: AuthorizationLease?, val context: CallContext?, val expires: Long)
    @Volatile private var bindingId = UUID.randomUUID()
    @Volatile private var pageId = UUID.randomUUID()
    private var boundGrant: AuthorizationLease? = null
    private var boundActor: AuthorizationLease? = null
    private var configureId = UUID.randomUUID()
    private var pending: PendingCall? = null
    private var mailbox: Mailbox? = null
    private var known: Map<String, String> = emptyMap()
    private var autoCalled = false
    /** Answer this ring once the page is signed in: room, ring token (null: the page's own ring), kind. */
    private var pendingAnswer: PendingAnswer? = null
    private var configured = false
    /** The app is on screen: the page's own ring screen and ringtone answer calls. */
    private var visible = false
    var window: CallWindow? = null
    private class ScheduledOperation(val id: UUID, val context: CallContext, val request: CalendarScheduledJoin) {
        var page: UUID? = null
        var job: Job? = null
    }
    private var scheduled: ScheduledOperation? = null
    private val mutablePreparing = MutableStateFlow(false)
    val preparingScheduled = mutablePreparing.asStateFlow()

    private fun capture(pageBound: Boolean = ready): CallContext? {
        val me = mailbox ?: return null
        val state = session.state.value
        if (state.loading || !state.signedIn || !state.termsAccepted) return null
        val root = session.authorizationLease
        val actor = scope.coroutineContext[AuthorizationContext]?.lease
        if (boundGrant !== root || boundActor !== actor) return null
        if (root != null && (!root.isValid || actor?.isValid != true || !actor.sharesAuthorization(root))) return null
        if (root == null && override?.token == null) return null
        return CallContext(bindingId, if (pageBound) pageId else null, me.address, root, actor)
    }
    private fun current(c: CallContext): Boolean {
        val state = session.state.value
        return c.binding == bindingId && (c.page == null || c.page == pageId) && c.mailbox == mailbox?.address &&
            c.grant === session.authorizationLease && c.grant?.isValid != false && c.actor?.isValid != false &&
            !state.loading && state.signedIn && state.termsAccepted
    }
    private fun tokenProvider(c: CallContext) = c.actor?.let { ScopedTokenProvider(session, it) } ?: c.grant?.let { ScopedTokenProvider(session, it) }
    private fun permissions(c: CallContext, video: Boolean, denied: String, declined: () -> Unit = {}, go: () -> Unit) {
        if (!current(c)) return
        val needed = buildList { add(Manifest.permission.RECORD_AUDIO); if (video) add(Manifest.permission.CAMERA) }.filterNot(::held)
        val window = window
        if (needed.isEmpty() || window == null) { if (current(c)) go() }
        else window.requestPermissions(needed.toTypedArray()) { result ->
            if (current(c)) {
                if (result[Manifest.permission.RECORD_AUDIO] == true || held(Manifest.permission.RECORD_AUDIO)) go()
                else { declined(); window.toast(denied) }
            }
        }
    }
    private fun runOrQueue(c: CallContext, script: String) {
        if (!current(c)) return
        if (ready && configured) evaluate(script, c.copy(page = pageId)) else pending = PendingCall(c, script)
    }

    init {
        IncomingCalls.host = java.lang.ref.WeakReference(this)
    }

    val webView: WebView get() = view ?: build().also { view = it }

    /** Load the page (once) and tell it who is signed in. Called whenever the mailbox or names change. */
    fun attach(mailbox: Mailbox, known: Map<String, String>) {
        val changed = this.mailbox != mailbox || this.known != known
        val actor = scope.coroutineContext[AuthorizationContext]?.lease
        val rebind = this.mailbox?.address != mailbox.address || boundGrant !== session.authorizationLease || boundActor !== actor
        if (rebind) {
            bindingId = UUID.randomUUID(); configureId = UUID.randomUUID(); configured = false; pending = null
            mutableMode.value = Mode.HIDDEN
            cancelScheduledJoin(hangUp = false)
            pendingAnswer = pendingAnswer?.takeIf { it.identity == mailbox.address && (it.grant == null || it.grant === session.authorizationLease) && it.context == null }
        }
        this.mailbox = mailbox; this.known = known
        boundGrant = session.authorizationLease; boundActor = actor
        val web = webView
        if (rebind && ready) stopPage()
        if (web.url == null) web.loadUrl(page.toString()) else if (changed || rebind) configure()
    }

    fun signOut() {
        bindingId = UUID.randomUUID(); configureId = UUID.randomUUID(); boundGrant = null; boundActor = null
        cancelScheduledJoin(hangUp = false)
        mailbox = null; known = emptyMap(); pending = null; pendingAnswer = null; configured = false
        if (ready) stopPage()
        mutableMode.value = Mode.HIDDEN
    }

    /** Call whoever `peer` is, by voice or with video. Asks for the microphone (and camera) first. */
    fun call(peer: String, video: Boolean) {
        if (scheduled != null) { window?.toast("Finish joining the meeting first."); return }
        val context = capture() ?: return
        val kind = if (video) "video" else "audio"
        permissions(context, video, "Allow the microphone in Android settings to make calls.") {
            runOrQueue(context, "PigeonpostNative.call(${quote(peer)}, ${quote(kind)})")
        }
    }

    /** On screen or not; off screen the app rings with a notification instead of the page. */
    fun setVisible(value: Boolean) {
        if (visible == value) return
        visible = value
        configure()
    }

    /**
     * Answer an incoming call from the notification or the incoming-call screen. The page may not be
     * loaded yet (the app was started by the push); it answers as soon as it has a session.
     */
    fun answer(room: String, ringToken: String?, kind: String, identity: String? = IncomingCalls.rings.value[room]?.identity ?: mailbox?.address) {
        cancelScheduledJoin()
        val target = identity ?: return
        val context = capture()
        val deferred = PendingAnswer(room, ringToken, kind, target, session.authorizationLease,
            context?.takeIf { it.mailbox == target }, android.os.SystemClock.elapsedRealtime() + 30_000)
        if (context == null || context.mailbox != target) {
            if (session.state.value.loading || session.state.value.signedIn || override?.token != null) pendingAnswer = deferred
            return
        }
        permissions(context, kind == "video", "Allow the microphone in Android settings to answer calls.") {
            if (android.os.SystemClock.elapsedRealtime() > deferred.expires) return@permissions
            if (ready && configured) evaluate("PigeonpostNative.answer(${quote(room)}, ${ringToken?.let(::quote) ?: "null"})", context.copy(page = pageId))
            else pendingAnswer = deferred
        }
    }

    /** Decline through the page: a ring it rang itself, or when the ring-token decline did not get through. */
    fun decline(room: String, ringToken: String?) {
        if (pendingAnswer?.room == room) pendingAnswer = null
        if (ready) evaluate("window.PigeonpostNative && PigeonpostNative.decline && PigeonpostNative.decline(${quote(room)}, ${ringToken?.let(::quote) ?: "null"})")
    }

    /**
     * A group call (Phase 3a, docs/groups.md §6): the page's own `placeGroup`/`joinGroup` ring every
     * active member or join the running call; the postbox rechecks membership each time.
     */
    fun callGroup(groupId: String, name: String, video: Boolean, join: Boolean) {
        if (scheduled != null) { window?.toast("Finish joining the meeting first."); return }
        val context = capture() ?: return
        val kind = if (video) "video" else "audio"
        val group = JSONObject().put("id", groupId).put("name", name)
        val script = "window.PigeonpostCalls && PigeonpostCalls.${if (join) "joinGroup" else "placeGroup"} ? PigeonpostCalls.${if (join) "joinGroup" else "placeGroup"}(${quote(kind)}, $group) : (window.PigeonpostCallsHost && PigeonpostCallsHost.toast('Group calls are not available here yet.'))"
        permissions(context, video, "Allow the microphone in Android settings to make calls.") {
            runOrQueue(context, script)
        }
    }

    /** Explicit member join; a code is only a locator and the server decides window/lobby/roles. */
    fun joinScheduled(request: CalendarScheduledJoin) {
        val context = capture()
        if (context == null) { request.close(); window?.toast("Calling isn’t ready yet. Try again in a moment."); return }
        try { request.requireCurrent(context.grant, context.mailbox) } catch (_: CancellationException) { request.close(); return }
        if (scheduled != null || mutableMode.value != Mode.HIDDEN) { request.close(); window?.toast("Finish the current call first."); return }
        val operation = ScheduledOperation(UUID.randomUUID(), context, request)
        scheduled = operation; mutablePreparing.value = true
        permissions(context, request.video, "Allow the microphone in Android settings to join the meeting.", declined = { finishScheduled(operation) }) {
            if (!scheduledCurrent(operation)) return@permissions
            operation.job = scope.launch(AuthorizationContext(context.actor)) {
                val deadline = android.os.SystemClock.elapsedRealtime() + 15_000
                while (!ready || !configured) {
                    if (!scheduledCurrent(operation)) { cancelScheduledJoin(); return@launch }
                    if (android.os.SystemClock.elapsedRealtime() > deadline) { finishScheduled(operation, "Calling isn’t ready yet. Try again."); return@launch }
                    delay(50)
                }
                if (!scheduledCurrent(operation)) { cancelScheduledJoin(); return@launch }
                operation.page = pageId
                val options = JSONObject().put("as", request.mailbox).put("occurrenceId", request.occurrenceId)
                    .put("title", request.title).put("kind", if (request.video) "video" else "audio")
                val id = quote(operation.id.toString())
                val script = """(function(){const send=r=>PigeonpostAndroid.postMessage(JSON.stringify({type:'scheduled_result',operation:$id,result:r}));if(!window.PigeonpostNative||typeof PigeonpostNative.joinScheduled!=='function'){send({state:'error',code:'bridge_unavailable'});return;}Promise.resolve(PigeonpostNative.joinScheduled(${quote(request.code)},$options)).then(send,()=>send({state:'error',code:'network'}));})()"""
                evaluate(script, context.copy(page = operation.page), allowed = { scheduledCurrent(operation) })
                delay(30_000)
                if (scheduledCurrent(operation)) cancelScheduledJoin(message = "Couldn’t reach the meeting. Try again when connected.")
            }
        }
        // Permission callbacks may take time. They must never resurrect a retired actor or linger forever.
        scope.launch(AuthorizationContext(context.actor)) {
            val deadline = android.os.SystemClock.elapsedRealtime() + 45_000
            while (scheduled === operation) {
                if (!scheduledCurrent(operation)) { cancelScheduledJoin(); return@launch }
                if (android.os.SystemClock.elapsedRealtime() > deadline) { finishScheduled(operation, "Joining was cancelled. Try again from the event."); return@launch }
                delay(100)
            }
        }
    }
    private fun scheduledCurrent(operation: ScheduledOperation): Boolean {
        if (scheduled !== operation || !current(operation.context) || operation.page?.let { it != pageId } == true) return false
        return try { operation.request.requireCurrent(session.authorizationLease, mailbox?.address); true } catch (_: CancellationException) { false }
    }
    private fun finishScheduled(operation: ScheduledOperation, message: String? = null) {
        if (scheduled !== operation) return
        val report = current(operation.context)
        scheduled = null; mutablePreparing.value = false; operation.request.close(); operation.job?.cancel()
        if (report && message != null) window?.toast(message)
    }
    fun cancelScheduledJoin(hangUp: Boolean = true, message: String? = null) {
        val operation = scheduled ?: return
        if (hangUp) operation.page?.let { cleanup("window.PigeonpostNative && PigeonpostNative.hangUp()", operation.context.binding, it) }
        finishScheduled(operation, message)
    }

    private fun held(permission: String) = permissionCheck?.invoke(permission) ?: (ContextCompat.checkSelfPermission(app, permission) == PackageManager.PERMISSION_GRANTED)

    private fun configure() {
        if (!ready) return
        val me = mailbox ?: return
        val context = capture(pageBound = true) ?: return
        val visit = UUID.randomUUID(); configureId = visit
        scope.launch(AuthorizationContext(context.actor)) {
            val token = override?.token ?: try { tokenProvider(context)?.token(null) } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null }
            if (!current(context) || visit != configureId || !ready) return@launch
            val address = override?.mailbox ?: me.address
            val handle = if (override?.mailbox != null) override.handle else me.handle
            val options = JSONObject()
                .put("token", token ?: JSONObject.NULL)
                .put("mailbox", JSONObject().put("address", address).put("handle", handle ?: JSONObject.NULL))
                .put("known", JSONObject(known))
                // Off screen (or answering from a notification) the app shows the ring, not the page.
                .put("nativeRing", !visible || pendingAnswer != null)
            evaluate("PigeonpostNative.configure($options)", context) {
                if (visit != configureId) return@evaluate
                configured = token != null
                if (configured) drainPending()
            }
            val auto = override?.autoCall
            if (auto != null && !autoCalled && auto.contains(':')) {
                autoCalled = true
                delay(1500)
                if (current(context)) call(auto.substringBeforeLast(':'), auto.substringAfterLast(':') == "video")
            }
        }
    }
    private fun drainPending() {
        val command = pending; pending = null
        if (command != null && current(command.context)) evaluate(command.script, command.context.copy(page = pageId))
        val answer = pendingAnswer ?: return
        if (answer.expires < android.os.SystemClock.elapsedRealtime() || answer.grant?.let { it !== session.authorizationLease || !it.isValid } == true ||
            answer.context?.let { !current(it) } == true) { pendingAnswer = null; return }
        if (answer.identity != mailbox?.address) return
        pendingAnswer = null
        answer(answer.room, answer.token, answer.kind, answer.identity)
        if (visible) configure()
    }

    private fun renew() {
        val context = capture(pageBound = true) ?: return
        scope.launch(AuthorizationContext(context.actor)) {
            val fresh = override?.token ?: try {
                val provider = tokenProvider(context)
                val current = provider?.token(null)
                provider?.token(current)
            } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null }
            evaluate("PigeonpostNative.renewed(${fresh?.let(::quote) ?: "null"})", context)
        }
    }

    private fun received(json: String) {
        val message = try { JSONObject(json) } catch (_: Exception) { return }
        when (message.optString("type")) {
            "ready" -> {
                ready = true
                configure()
            }
            "scheduled_result" -> {
                val operation = scheduled ?: return
                if (message.optString("operation") != operation.id.toString() || !scheduledCurrent(operation)) return
                val result = message.optJSONObject("result")
                when (result?.optString("state")) {
                    "joined", "waiting", "cancelled" -> finishScheduled(operation)
                    "choose_occurrence" -> finishScheduled(operation, "Choose a specific occurrence from your calendar to join.")
                    else -> finishScheduled(operation, when (result?.optString("code")) {
                        "bridge_unavailable" -> "Calling needs an updated call page. Close and reopen the app, then try again."
                        "identity_changed" -> "Your inbox changed. Open the event again to join."
                        "timeout", "network" -> "Couldn’t reach the meeting. Try again when connected."
                        else -> result?.optString("message")?.take(400)?.takeIf { it.isNotBlank() } ?: "Couldn’t join the meeting. Try again from the event."
                    })
                }
            }
            "mode" -> mutableMode.value = when (message.optString("mode")) { "full" -> Mode.FULL; "mini" -> Mode.MINI; else -> Mode.HIDDEN }
            "renew" -> renew()
            "toast" -> message.optString("text").takeIf { it.isNotBlank() }?.let { window?.toast(it) }
            "ring" -> {
                val me = mailbox ?: return
                val from = message.optJSONObject("from") ?: return
                IncomingCalls.fromPage(app, message.optString("room"), message.optString("kind"), from.optString("address"),
                    from.optString("handle").takeIf { it.isNotEmpty() && it != "null" }, override?.mailbox ?: me.address)
            }
            "ring_ended" -> message.optString("room").takeIf { it.isNotEmpty() }?.let { room ->
                if (IncomingCalls.rings.value.containsKey(room)) IncomingCalls.end(app, room, message.optString("reason").ifEmpty { "ended" })
            }
        }
    }

    private fun evaluate(script: String, context: CallContext? = capture(pageBound = true), allowed: () -> Boolean = { true }, done: (() -> Unit)? = null) {
        if (context == null) return
        main.post {
            if (current(context) && allowed() && samePage(view?.url?.let(Uri::parse))) view?.evaluateJavascript(script) {
                if (current(context) && allowed()) done?.invoke()
            }
        }
    }
    private fun stopPage() {
        cleanup("window.PigeonpostNative && PigeonpostNative.signOut()", bindingId, pageId)
    }
    private fun cleanup(script: String, binding: UUID, page: UUID) {
        main.post { if (binding == bindingId && page == pageId) view?.evaluateJavascript(script, null) }
    }

    /** The page's own origin, and nothing else. */
    fun samePage(uri: Uri?) = uri != null && uri.scheme == page.scheme && uri.host == page.host && uri.port == page.port && uri.path == page.path
    private fun sameOrigin(uri: Uri?) = uri != null && uri.scheme == page.scheme && uri.host == page.host && uri.port == page.port

    @SuppressLint("SetJavaScriptEnabled")
    private fun build(): WebView = WebView(context).apply {
        setBackgroundColor(Color.TRANSPARENT)
        isVerticalScrollBarEnabled = false; isHorizontalScrollBarEnabled = false
        overScrollMode = WebView.OVER_SCROLL_NEVER
        contentDescription = "Call"
        settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            mediaPlaybackRequiresUserGesture = false
            allowFileAccess = false
            allowContentAccess = false
            setSupportMultipleWindows(false)
            javaScriptCanOpenWindowsAutomatically = false
            mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
            setGeolocationEnabled(false)
        }
        addJavascriptInterface(Bridge(this), "PigeonpostAndroid")
        webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                if (request.isForMainFrame && samePage(request.url)) return false
                // A link out (the meeting link's own page, a help link) opens in the browser.
                if (request.hasGesture() && request.url.scheme in setOf("https", "http")) window?.openExternal(request.url)
                return true
            }
            override fun onPageStarted(view: WebView, url: String?, favicon: android.graphics.Bitmap?) {
                pageId = UUID.randomUUID(); configureId = UUID.randomUUID(); ready = false; configured = false
                if (scheduled?.page != null) cancelScheduledJoin(hangUp = false)
            }
            override fun onPageFinished(view: WebView, url: String?) {
                if (view !== this@CallHost.view || ready || !samePage(url?.let(Uri::parse))) return
                val document = pageId
                main.post {
                    if (view === this@CallHost.view && document == pageId && !ready && samePage(view.url?.let(Uri::parse))) {
                        // A fast local/cached document can announce readiness before queued native
                        // lifecycle work finishes. Ask the loaded bridge to announce it again;
                        // normal source/page/actor checks still handle configuration and actions.
                        view.evaluateJavascript("""if(document.readyState!=='loading'&&window.PigeonpostNative&&typeof PigeonpostNative.configure==='function'){PigeonpostAndroid.postMessage(JSON.stringify({type:'ready'}));}""", null)
                    }
                }
            }
            override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
                // Start again from a page that knows nothing; the activity re-adds the new view.
                bindingId = UUID.randomUUID(); pageId = UUID.randomUUID(); ready = false; configured = false; pending = null; pendingAnswer = null
                cancelScheduledJoin(hangUp = false); mutableMode.value = Mode.HIDDEN
                (view.parent as? android.view.ViewGroup)?.removeView(view)
                view.destroy()
                if (this@CallHost.view === view) this@CallHost.view = null
                rebuilt.value++
                mailbox?.let { me -> attach(me, known) }
                return true
            }
        }
        webChromeClient = object : WebChromeClient() {
            override fun onPermissionRequest(request: PermissionRequest) {
                main.post { permission(request) }
            }
        }
    }

    /** Bumped when the WebView is replaced, so the activity re-parents the new one. */
    val rebuilt = MutableStateFlow(0)

    /** Microphone and camera for the call page only, once the app itself holds them. */
    private fun permission(request: PermissionRequest) {
        if (!sameOrigin(request.origin)) { request.deny(); return }
        val context = capture(pageBound = true)
        if (context == null || !configured || !ready) { request.deny(); return }
        val joining = scheduled
        val wanted = request.resources.filter { it == PermissionRequest.RESOURCE_AUDIO_CAPTURE || it == PermissionRequest.RESOURCE_VIDEO_CAPTURE }
        if (wanted.isEmpty()) { request.deny(); return }
        fun android(resource: String) = if (resource == PermissionRequest.RESOURCE_AUDIO_CAPTURE) Manifest.permission.RECORD_AUDIO else Manifest.permission.CAMERA
        fun answer() {
            if (!current(context) || joining?.let { !scheduledCurrent(it) } == true) { request.deny(); return }
            val granted = wanted.filter { held(android(it)) }
            if (granted.isEmpty()) request.deny() else request.grant(granted.toTypedArray())
        }
        val missing = wanted.map(::android).filterNot(::held)
        val window = window
        if (missing.isEmpty() || window == null) answer()
        else window.requestPermissions(missing.toTypedArray()) { answer() }
    }

    fun destroy() { signOut(); pageId = UUID.randomUUID(); view?.let { (it.parent as? android.view.ViewGroup)?.removeView(it); it.destroy() }; view = null }

    private inner class Bridge(private val source: WebView) {
        @JavascriptInterface
        fun postMessage(json: String) {
            if (json.length > 16 * 1024) return
            val binding = bindingId; val page = pageId
            main.post {
                // Only the call page may speak for the app; navigation is pinned to it, and this checks again.
                if (source === view && page == pageId && samePage(source.url?.let(Uri::parse))) {
                    val type = runCatching { JSONObject(json).optString("type") }.getOrNull()
                    // Readiness belongs to this document, independently of the current login.
                    // It grants no action: configure() captures and checks the live actor itself.
                    // Other messages still belong to their original account/inbox binding.
                    if (type == "ready" || binding == bindingId && capture(pageBound = true) != null) received(json)
                }
            }
        }
    }

    companion object {
        const val PRODUCTION = "https://inbox.pigeonpost.dev/native-call.html?v=20261025-scheduled1"
        /** The calls service is `/calls/v1` on the page's own origin. */
        const val PRODUCTION_ORIGIN = "https://inbox.pigeonpost.dev"
        private fun quote(value: String) = JSONObject.quote(value)
    }
}
