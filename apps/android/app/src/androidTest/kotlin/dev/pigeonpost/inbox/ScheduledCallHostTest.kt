package dev.pigeonpost.inbox

import android.app.Application
import android.content.Intent
import android.net.Uri
import android.webkit.JavascriptInterface
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.*
import dev.pigeonpost.core.calendar.CalendarScheduledJoin
import dev.pigeonpost.inbox.auth.SessionState
import dev.pigeonpost.inbox.auth.UserSession
import dev.pigeonpost.inbox.calls.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import org.json.JSONArray
import org.json.JSONObject
import org.json.JSONTokener
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume

/** The actual WebView/Java bridge with a loopback page; no media, public service or real calls. */
@RunWith(AndroidJUnit4::class)
class ScheduledCallHostTest {
    internal class Page(autoReady: Boolean = true, exposeApi: Boolean = autoReady) : AutoCloseable {
        private val socket = ServerSocket(0, 16, InetAddress.getByName("127.0.0.1"))
        private val connections = java.util.Collections.synchronizedSet(mutableSetOf<Socket>())
        val url = "http://127.0.0.1:${socket.localPort}/native-call.html"
        private val html = """<!doctype html><html><body>Local bridge fixture<script>
            window.events=[];window.options=null;window.renewedCount=0;window.documentID=String(Date.now())+String(Math.random());
            const report=x=>PigeonpostAndroid.postMessage(JSON.stringify(x));
            window.fixtureNative={
              configure(o){window.options=o;events.push({type:'configure',mailbox:o.mailbox.address});},
              signOut(){window.options=null;events.push({type:'signout'});report({type:'mode',mode:'hidden'});},
              renewed(t){window.renewedCount++;},
              call(peer,kind){events.push({type:'direct',peer,kind});},
              answer(room,t){events.push({type:'answer',room});},
              decline(room,t){events.push({type:'decline',room});},
              hangUp(){events.push({type:'hangup'});report({type:'mode',mode:'hidden'});},
              joinScheduled(code,o){events.push({type:'join',code,options:o});report({type:'mode',mode:'full'});return Promise.resolve({state:'waiting'});}
            };
            window.PigeonpostCalls={placeGroup(k,g){events.push({type:'group',id:g.id});},joinGroup(k,g){events.push({type:'group',id:g.id});}};
            if ($exposeApi) window.PigeonpostNative=window.fixtureNative;
            window.readyMessages=0;
            window.signalReady=()=>{window.PigeonpostNative=window.fixtureNative;window.readyMessages++;report({type:'ready'});};
            if ($autoReady) window.signalReady();
            </script></body></html>""".toByteArray()
        init {
            Thread({
                while (!socket.isClosed) try {
                    val client = socket.accept(); connections += client
                    Thread({
                        try { client.use {
                            it.soTimeout = 5000
                            val input = it.getInputStream().bufferedReader()
                            if (input.readLine() == null) return@use
                            while (!input.readLine().isNullOrEmpty()) { }
                            it.getOutputStream().apply {
                                write("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: ${html.size}\r\nConnection: close\r\n\r\n".toByteArray())
                                write(html); flush()
                            }
                        } } catch (_: Exception) { } finally { connections -= client }
                    }, "pp-bridge-http-request").apply { isDaemon = true; start() }
                } catch (_: Exception) { if (!socket.isClosed) throw IllegalStateException("Local bridge fixture failed") }
            }, "pp-bridge-http").apply { isDaemon = true; start() }
        }
        override fun close() { socket.close(); synchronized(connections) { connections.toList() }.forEach { it.close() } }
    }
    private class Session(loading: Boolean) : UserSession {
        @Volatile var grant: AuthorizationLease? = if (loading) null else AuthorizationLease()
        override val authorizationLease get() = grant
        override val state = MutableStateFlow(SessionState(loading = loading, signedIn = !loading, termsAccepted = !loading, authorizationId = grant?.id))
        var gate: CompletableDeferred<Unit>? = null
        var acquisitions = 0
        override suspend fun token(rejected: String?): String {
            val captured = grant ?: throw AuthorizationRetired()
            captured.requireActive(); acquisitions++; gate?.await()
            return "fixture-" + captured.id
        }
        fun replace() { grant?.retire(); grant = AuthorizationLease(); state.value = SessionState(loading = false, signedIn = true, termsAccepted = true, authorizationId = grant!!.id) }
        override suspend fun begin(provider: String?, otherAccount: Boolean): Intent? = null
        override suspend fun complete(result: Intent?) { }
        override suspend fun cancel() { }
        override suspend fun acceptTerms() { }
        override suspend fun signOut() { grant?.retire(); state.value = SessionState(loading = false) }
    }
    private class Window : CallWindow {
        class Question(val permissions: Array<String>, val answer: (Map<String, Boolean>) -> Unit)
        val questions = mutableListOf<Question>()
        val granted = mutableSetOf<String>()
        val notices = mutableListOf<String>()
        override fun requestPermissions(permissions: Array<String>, done: (Map<String, Boolean>) -> Unit) { questions += Question(permissions, done) }
        override fun openExternal(uri: Uri) { fail("The call bridge must not open an external page") }
        override fun toast(text: String) { notices += text }
        fun allow(index: Int = questions.lastIndex) { val q = questions[index]; granted += q.permissions; q.answer(q.permissions.associateWith { true }) }
        fun deny(index: Int = questions.lastIndex) { val q = questions[index]; q.answer(q.permissions.associateWith { false }) }
    }
    private class Fixture(loading: Boolean, val page: Page) {
        val session = Session(loading)
        val window = Window()
        val base = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val scope = AuthorizedStoreScope(base).apply { activate(session.grant) }
        val host = CallHost(ApplicationProvider.getApplicationContext<Application>(), session, scope,
            CallsOverride(page.url, null, null, null, null), permissionCheck = { it in window.granted })
        fun attach() { host.attach(Mailbox("/k/alice", "/alice"), emptyMap()) }
        suspend fun js(expression: String): Any? = suspendCancellableCoroutine { continuation ->
            host.webView.evaluateJavascript("JSON.stringify($expression)") { encoded ->
                if (continuation.isActive) {
                    val decoded = runCatching { JSONTokener(encoded).nextValue() }.getOrNull()
                    val result = if (decoded is String) runCatching { JSONTokener(decoded).nextValue() }.getOrNull() else decoded
                    continuation.resume(result)
                }
            }
        }
        suspend fun waitFor(condition: suspend () -> Boolean) { withTimeout(10_000) { while (!condition()) delay(20) } }
        suspend fun ready() { waitFor { js("window.options && window.options.token") == "fixture-" + session.grant!!.id } }
        suspend fun events(type: String) = js("window.events.filter(x=>x.type===${JSONObject.quote(type)})") as JSONArray
        fun join(actor: AuthorizationLease = session.grant!!, title: String = "Reviewed call", video: Boolean = false) = CalendarScheduledJoin("/k/alice", "ABCDEFGHJK", "occ_original", title, video, actor)
        fun replace() { session.replace(); scope.retire(); scope.activate(session.grant); attach() }
    }
    private class ReadyProbe(val sent: CountDownLatch = CountDownLatch(1)) {
        @JavascriptInterface fun sent() { sent.countDown() }
    }
    private fun world(loading: Boolean = false, autoReady: Boolean = true, exposeApi: Boolean = autoReady, work: suspend Fixture.() -> Unit) = runBlocking<Unit> {
        val page = Page(autoReady, exposeApi); val f = Fixture(loading, page)
        try {
            withTimeout(25_000) { withContext(Dispatchers.Main) {
                f.host.window = f.window; f.host.setVisible(true)
                if (!loading) f.attach()
                f.work()
            } }
        } finally { withContext(Dispatchers.Main) { f.host.destroy(); f.scope.retire() }; f.base.cancel(); page.close() }
    }
    @Test fun pageReadinessQueuedBeforeRebindingConfiguresOnlyTheReplacementGrant() = world(autoReady = false) {
        val probe = ReadyProbe()
        // Interfaces become visible on the next document load. The fixture has no live service.
        host.webView.addJavascriptInterface(probe, "PigeonpostReadyProbe")
        host.webView.reload()
        waitFor { js("typeof window.signalReady === 'function'") == true }
        val previous = session.grant!!
        host.webView.evaluateJavascript("window.signalReady();PigeonpostReadyProbe.sent()", null)
        // Hold this main-thread turn until the real Java bridge has queued the ready callback.
        // Rebind before releasing the queue; this deterministically exercises the startup race.
        assertTrue("The real page did not queue its readiness message", probe.sent.await(3, TimeUnit.SECONDS))
        replace()
        ready()
        assertFalse(previous.isValid)
        assertEquals(1, events("configure").length())
        assertEquals("fixture-" + session.grant!!.id, js("window.options.token"))
    }
    @Test fun aLoadedCallPageRecoversAMissedReadinessAnnouncement() = world(autoReady = false, exposeApi = true) {
        ready()
        assertEquals(0, (js("window.readyMessages") as Number).toInt())
        assertEquals(1, events("configure").length())
        assertEquals("fixture-" + session.grant!!.id, js("window.options.token"))
    }
    @Test fun reviewedJoinWaitsForPermissionAndCopiesOnlySupportedMemberInputs() = world {
        ready()
        val title = "Title\";window.compromised=true;"
        host.joinScheduled(join(title = title, video = true))
        assertTrue(host.preparingScheduled.value); assertEquals(1, window.questions.size); assertEquals(0, events("join").length())
        window.allow()
        waitFor { events("join").length() == 1 && !host.preparingScheduled.value }
        val item = events("join").getJSONObject(0)
        assertEquals("ABCDEFGHJK", item.getString("code"))
        val options = item.getJSONObject("options")
        assertEquals(setOf("as", "occurrenceId", "title", "kind"), options.keys().asSequence().toSet())
        assertEquals("/k/alice", options.getString("as")); assertEquals("occ_original", options.getString("occurrenceId"))
        assertEquals(title, options.getString("title")); assertEquals("video", options.getString("kind"))
        assertEquals(false, js("window.compromised === true"))
    }
    @Test fun anOldPermissionAnswerCannotStartAMeetingAsTheReplacementLogin() = world {
        ready(); host.joinScheduled(join()); assertEquals(1, window.questions.size)
        replace(); ready(); window.allow(0)
        waitFor { !host.preparingScheduled.value }
        assertEquals(0, events("join").length())
        host.joinScheduled(join()); waitFor { events("join").length() == 1 }
    }
    @Test fun retiringOnlyTheCalendarActorAlsoRefusesADelayedPermissionAnswer() = world {
        ready(); val actor = session.grant!!.child()
        host.joinScheduled(join(actor)); actor.retire(); window.allow()
        waitFor { !host.preparingScheduled.value }
        assertEquals(0, events("join").length()); assertTrue(session.grant!!.isValid)
    }
    @Test fun reloadingTheCallPageCannotReuseItsOldPermissionAnswer() = world {
        ready(); val document = js("window.documentID")
        host.joinScheduled(join()); host.webView.reload()
        waitFor { val next = js("window.documentID"); next is String && next != document }
        ready(); window.allow()
        waitFor { !host.preparingScheduled.value }
        assertEquals(0, events("join").length())
    }
    @Test fun microphoneDenialEndsPreparationImmediately() = world {
        ready(); host.joinScheduled(join()); window.deny()
        assertFalse(host.preparingScheduled.value); assertEquals(0, events("join").length())
        assertTrue(window.notices.any { it.contains("microphone") })
    }
    @Test fun aColdStartRingWaitsForTheRestoredRecipientBeforeAskingForPermission() = world(loading = true) {
        host.answer("fixture-room", "fixture-ring-only", "audio", "/k/alice")
        assertTrue(window.questions.isEmpty())
        session.replace(); scope.activate(session.grant); attach(); ready()
        waitFor { window.questions.size == 1 }; window.allow()
        waitFor { events("answer").length() == 1 }
        assertEquals("fixture-room", events("answer").getJSONObject(0).getString("room"))
    }
    @Test fun aDirectCallQueuedDuringTokenLoadingRunsOnlyAfterConfigureExecutes() = world {
        session.gate = CompletableDeferred()
        waitFor { session.acquisitions > 0 }
        host.call("/peer", false); window.allow()
        assertEquals(0, events("direct").length())
        session.gate!!.complete(Unit); ready()
        waitFor { events("direct").length() == 1 }
        assertEquals("/peer", events("direct").getJSONObject(0).getString("peer"))
    }
    @Test fun calendarRetirementBeforeTheQueuedScriptExecutesPreventsTheJoin() = world {
        ready(); val actor = session.grant!!.child()
        host.joinScheduled(join(actor)); window.allow(); actor.retire()
        waitFor { !host.preparingScheduled.value }
        assertEquals(0, events("join").length())
    }
    @Test fun aRestoredDifferentRecipientCannotAdoptAnUnboundRingAnswer() = world(loading = true) {
        host.answer("fixture-room", "fixture-ring-only", "audio", "/k/other")
        session.replace(); scope.activate(session.grant); attach(); ready()
        assertTrue(window.questions.isEmpty()); assertEquals(0, events("answer").length())
    }
    @Test fun aDelayedRenewalCannotInjectTheReplacementLoginsTokenIntoItsOldRequest() = world {
        ready(); val before = session.acquisitions
        session.gate = CompletableDeferred()
        js("(PigeonpostAndroid.postMessage(JSON.stringify({type:'renew'})),true)")
        waitFor { session.acquisitions > before }
        replace(); session.gate!!.complete(Unit); ready()
        assertEquals(0, (js("window.renewedCount") as Number).toInt())
    }
}
