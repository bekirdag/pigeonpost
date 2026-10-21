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
import dev.pigeonpost.inbox.auth.UserSession
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import org.json.JSONObject

/** A debug build's stand-in page, token and mailbox for the local call harness. Never in a release. */
data class CallsOverride(val page: String, val token: String?, val mailbox: String?, val handle: String?, val autoCall: String?)

/** What the activity provides: runtime permissions, links out, and short notices. */
interface CallWindow {
    fun requestPermissions(permissions: Array<String>, done: (Map<String, Boolean>) -> Unit)
    fun openExternal(uri: Uri)
    fun toast(text: String)
}

class CallHost(private val app: Application, private val session: UserSession, private val scope: CoroutineScope,
    private val override: CallsOverride?) {
    enum class Mode { HIDDEN, MINI, FULL }

    private val mutableMode = MutableStateFlow(Mode.HIDDEN)
    val mode: StateFlow<Mode> = mutableMode.asStateFlow()

    val page: Uri = Uri.parse(override?.page ?: PRODUCTION)
    private val main = Handler(Looper.getMainLooper())
    /** The view outlives an activity (rotation), so it is built on a context that can be re-pointed. */
    val context = MutableContextWrapper(app)
    private var view: WebView? = null
    private var ready = false
    private var pending: Pair<String, String>? = null
    private var mailbox: Mailbox? = null
    private var known: Map<String, String> = emptyMap()
    private var autoCalled = false
    /** Answer this ring once the page is signed in: room, ring token (null: the page's own ring), kind. */
    private var pendingAnswer: Triple<String, String?, String>? = null
    private var configured = false
    /** The app is on screen: the page's own ring screen and ringtone answer calls. */
    private var visible = false
    var window: CallWindow? = null

    init {
        IncomingCalls.host = java.lang.ref.WeakReference(this)
    }

    val webView: WebView get() = view ?: build().also { view = it }

    /** Load the page (once) and tell it who is signed in. Called whenever the mailbox or names change. */
    fun attach(mailbox: Mailbox, known: Map<String, String>) {
        val changed = this.mailbox != mailbox || this.known != known
        this.mailbox = mailbox; this.known = known
        val web = webView
        if (web.url == null) web.loadUrl(page.toString()) else if (changed) configure()
    }

    fun signOut() {
        mailbox = null; known = emptyMap(); pending = null; pendingAnswer = null; configured = false
        if (ready) evaluate("window.PigeonpostNative && PigeonpostNative.signOut()")
        mutableMode.value = Mode.HIDDEN
    }

    /** Call whoever `peer` is, by voice or with video. Asks for the microphone (and camera) first. */
    fun call(peer: String, video: Boolean) {
        val kind = if (video) "video" else "audio"
        val needed = buildList { add(Manifest.permission.RECORD_AUDIO); if (video) add(Manifest.permission.CAMERA) }.filterNot(::held)
        val go = { if (ready) evaluate("PigeonpostNative.call(${quote(peer)}, ${quote(kind)})") else pending = peer to kind }
        val window = window
        if (needed.isEmpty() || window == null) go()
        else window.requestPermissions(needed.toTypedArray()) { result ->
            if (result[Manifest.permission.RECORD_AUDIO] == true) go() else window.toast("Allow the microphone in Android settings to make calls.")
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
    fun answer(room: String, ringToken: String?, kind: String) {
        val needed = buildList { add(Manifest.permission.RECORD_AUDIO); if (kind == "video") add(Manifest.permission.CAMERA) }.filterNot(::held)
        val go = {
            if (ready && configured) evaluate("PigeonpostNative.answer(${quote(room)}, ${ringToken?.let(::quote) ?: "null"})")
            else pendingAnswer = Triple(room, ringToken, kind)
        }
        val window = window
        if (needed.isEmpty() || window == null) go()
        else window.requestPermissions(needed.toTypedArray()) { result ->
            if (result[Manifest.permission.RECORD_AUDIO] == true) go()
            else { decline(room, ringToken); window.toast("Allow the microphone in Android settings to answer calls.") }
        }
    }

    /** Decline through the page: a ring it rang itself, or when the ring-token decline did not get through. */
    fun decline(room: String, ringToken: String?) {
        if (pendingAnswer?.first == room) pendingAnswer = null
        if (ready) evaluate("window.PigeonpostNative && PigeonpostNative.decline && PigeonpostNative.decline(${quote(room)}, ${ringToken?.let(::quote) ?: "null"})")
    }

    /**
     * A group call (Phase 3a, docs/groups.md §6): the page's own `placeGroup`/`joinGroup` ring every
     * active member or join the running call; the postbox rechecks membership each time.
     */
    fun callGroup(groupId: String, name: String, video: Boolean, join: Boolean) {
        val kind = if (video) "video" else "audio"
        val group = JSONObject().put("id", groupId).put("name", name)
        val script = "window.PigeonpostCalls && PigeonpostCalls.${if (join) "joinGroup" else "placeGroup"} ? PigeonpostCalls.${if (join) "joinGroup" else "placeGroup"}(${quote(kind)}, $group) : (window.PigeonpostCallsHost && PigeonpostCallsHost.toast('Group calls are not available here yet.'))"
        val needed = buildList { add(Manifest.permission.RECORD_AUDIO); if (video) add(Manifest.permission.CAMERA) }.filterNot(::held)
        val go = { if (ready && configured) evaluate(script) else window?.toast("Calling isn’t ready yet. Try again in a moment.") }
        val window = window
        if (needed.isEmpty() || window == null) go()
        else window.requestPermissions(needed.toTypedArray()) { result ->
            if (result[Manifest.permission.RECORD_AUDIO] == true) go() else window.toast("Allow the microphone in Android settings to make calls.")
        }
    }

    private fun held(permission: String) = ContextCompat.checkSelfPermission(app, permission) == PackageManager.PERMISSION_GRANTED

    private fun configure() {
        if (!ready) return
        val me = mailbox ?: return
        scope.launch {
            val token = override?.token ?: try { session.token(null) } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null }
            val address = override?.mailbox ?: me.address
            val handle = if (override?.mailbox != null) override.handle else me.handle
            val options = JSONObject()
                .put("token", token ?: JSONObject.NULL)
                .put("mailbox", JSONObject().put("address", address).put("handle", handle ?: JSONObject.NULL))
                .put("known", JSONObject(known))
                // Off screen (or answering from a notification) the app shows the ring, not the page.
                .put("nativeRing", !visible || pendingAnswer != null)
            evaluate("PigeonpostNative.configure($options)")
            configured = token != null
            pendingAnswer?.let { (room, ringToken, _) ->
                pendingAnswer = null
                evaluate("PigeonpostNative.answer(${quote(room)}, ${ringToken?.let(::quote) ?: "null"})")
            }
            val auto = override?.autoCall
            if (auto != null && !autoCalled && auto.contains(':')) {
                autoCalled = true
                delay(1500)
                call(auto.substringBeforeLast(':'), auto.substringAfterLast(':') == "video")
            }
        }
    }

    private fun renew() {
        scope.launch {
            val fresh = override?.token ?: try {
                val current = session.token(null)
                session.token(current)
            } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null }
            evaluate("PigeonpostNative.renewed(${fresh?.let(::quote) ?: "null"})")
        }
    }

    private fun received(json: String) {
        val message = try { JSONObject(json) } catch (_: Exception) { return }
        when (message.optString("type")) {
            "ready" -> {
                ready = true
                configure()
                pending?.let { (peer, kind) -> pending = null; evaluate("PigeonpostNative.call(${quote(peer)}, ${quote(kind)})") }
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

    private fun evaluate(script: String) { main.post { view?.evaluateJavascript(script, null) } }

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
        addJavascriptInterface(Bridge(), "PigeonpostAndroid")
        webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                if (request.isForMainFrame && samePage(request.url)) return false
                // A link out (the meeting link's own page, a help link) opens in the browser.
                if (request.hasGesture() && request.url.scheme in setOf("https", "http")) window?.openExternal(request.url)
                return true
            }
            override fun onPageStarted(view: WebView, url: String?, favicon: android.graphics.Bitmap?) { ready = false; configured = false }
            override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
                // Start again from a page that knows nothing; the activity re-adds the new view.
                ready = false; mutableMode.value = Mode.HIDDEN
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
        val wanted = request.resources.filter { it == PermissionRequest.RESOURCE_AUDIO_CAPTURE || it == PermissionRequest.RESOURCE_VIDEO_CAPTURE }
        if (wanted.isEmpty()) { request.deny(); return }
        fun android(resource: String) = if (resource == PermissionRequest.RESOURCE_AUDIO_CAPTURE) Manifest.permission.RECORD_AUDIO else Manifest.permission.CAMERA
        fun answer() {
            val granted = wanted.filter { held(android(it)) }
            if (granted.isEmpty()) request.deny() else request.grant(granted.toTypedArray())
        }
        val missing = wanted.map(::android).filterNot(::held)
        val window = window
        if (missing.isEmpty() || window == null) answer()
        else window.requestPermissions(missing.toTypedArray()) { answer() }
    }

    fun destroy() { view?.let { (it.parent as? android.view.ViewGroup)?.removeView(it); it.destroy() }; view = null }

    private inner class Bridge {
        @JavascriptInterface
        fun postMessage(json: String) {
            main.post {
                // Only the call page may speak for the app; navigation is pinned to it, and this checks again.
                if (samePage(view?.url?.let(Uri::parse))) received(json)
            }
        }
    }

    companion object {
        const val PRODUCTION = "https://inbox.pigeonpost.dev/native-call.html?v=20261020-ring"
        /** The calls service is `/calls/v1` on the page's own origin. */
        const val PRODUCTION_ORIGIN = "https://inbox.pigeonpost.dev"
        private fun quote(value: String) = JSONObject.quote(value)
    }
}
