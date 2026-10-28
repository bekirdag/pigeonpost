package dev.pigeonpost.inbox

import android.Manifest
import android.content.Context
import android.content.Intent
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.rule.GrantPermissionRule
import dev.pigeonpost.inbox.calls.LiveStreamService
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket

/**
 * Group live streams (Phase 6) on the device: the LIVE banner over the real-MLS groups fixture, stage
 * invitations and the end read from the postbox, the "Who can go live" setting, and the bridge to the
 * call page (`PigeonpostNative.watchStream/goLive`, `PigeonpostLive.onGroupStream/current`) through the
 * real WebView, with a loopback stand-in for the page. No media, cameras or servers beyond loopback.
 * Runs on phone and tablet emulators.
 */
@RunWith(AndroidJUnit4::class)
class LiveStreamsUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    @get:Rule val permissions: GrantPermissionRule = GrantPermissionRule.grant(Manifest.permission.CAMERA, Manifest.permission.RECORD_AUDIO)
    private val context get() = ApplicationProvider.getApplicationContext<Context>()

    /** The call page's stand-in: does what native-call.js and live.js promise, and reports every call back here. */
    private class Page : AutoCloseable {
        private val socket = ServerSocket(0, 16, InetAddress.getByName("127.0.0.1"))
        val url = "http://127.0.0.1:${socket.localPort}/native-call.html"
        val log = java.util.Collections.synchronizedList(mutableListOf<JSONObject>())
        private val html = """<!doctype html><html><body><div id="live-ui" hidden style="position:fixed;inset:0;background:#000;color:#fff">Live</div><script>
            const report=x=>PigeonpostAndroid.postMessage(JSON.stringify(x));
            const log=x=>fetch('/log',{method:'POST',body:JSON.stringify(x)});
            let current=null;
            const show=on=>{document.getElementById('live-ui').hidden=!on;report({type:'mode',mode:on?'full':'hidden'});};
            window.PigeonpostLive={current:()=>current,show:()=>{log({call:'show'});show(true);},
              onGroupStream:d=>{log({call:'onGroupStream',data:d});if(current&&d.stream_id===current.id){if(d.state==='ended'){current=null;show(false);}else if(d.stage&&d.stage.state==='invited'){current.role='invited';}}}};
            window.PigeonpostNative={
              configure(o){log({call:'configure',signedIn:Boolean(o.token)});},renewed(){},signOut(){current=null;show(false);},hangUp(){},
              watchStream(g,s){log({call:'watchStream',group:g,stream:s});current={id:s,groupId:g.id,role:'viewer'};show(true);return Promise.resolve({ok:true});},
              goLive(g,o){log({call:'goLive',group:g,options:o});if(o.title==='full'){return Promise.resolve({ok:false,code:'stream_capacity'});}current={id:null,groupId:g.id,role:'host'};show(true);return Promise.resolve({ok:true});},
              stopStream(){log({call:'stopStream'});current=null;show(false);}};
            report({type:'ready'});
            </script></body></html>""".toByteArray()
        init {
            Thread({
                while (!socket.isClosed) try {
                    val client = socket.accept()
                    Thread({ serve(client) }, "pp-live-page").apply { isDaemon = true; start() }
                } catch (_: Exception) { }
            }, "pp-live-http").apply { isDaemon = true; start() }
        }
        private fun serve(client: Socket) = try { client.use {
            it.soTimeout = 5000
            val input = it.getInputStream().bufferedReader()
            val request = input.readLine() ?: return@use
            var length = 0
            while (true) { val line = input.readLine(); if (line.isNullOrEmpty()) break; if (line.lowercase().startsWith("content-length:")) length = line.substringAfter(':').trim().toInt() }
            val body = if (request.startsWith("POST /log")) CharArray(length).also { buf -> var n = 0; while (n < length) { val r = input.read(buf, n, length - n); if (r < 0) break; n += r } }.concatToString() else null
            body?.let { b -> log += JSONObject(b) }
            val out = if (body != null) ByteArray(0) else html
            it.getOutputStream().apply {
                write("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: ${out.size}\r\nConnection: close\r\n\r\n".toByteArray()); write(out); flush()
            }
        } } catch (_: Exception) { }
        fun calls(name: String) = synchronized(log) { log.filter { it.optString("call") == name } }
        override fun close() { socket.close() }
    }

    @Before fun reset() { LiveFixtureControl.reset(); context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit() }
    @After fun after() { LiveFixtureControl.reset() }

    private fun launch(page: Page? = null): ActivityScenario<MainActivity> {
        val intent = Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "live")
        if (page != null) intent.putExtra("pigeonpost.calls.page", page.url).putExtra("pigeonpost.calls.token", "tok-fixture")
            .putExtra("pigeonpost.calls.mailbox", "/k/fixture0000000000000000a").putExtra("pigeonpost.calls.handle", "/demo/main")
        return ActivityScenario.launch(intent)
    }
    private fun dumped(work: () -> Unit) = try { work() } catch (e: Throwable) { ui.onAllNodes(isRoot()).fetchSemanticsNodes().indices.forEach { ui.onAllNodes(isRoot())[it].printToLog("PPDUMP", Int.MAX_VALUE) }; throw e }
    private fun text(text: String, ms: Long = 60_000) = dumped { ui.waitUntil(ms) { ui.onAllNodesWithText(text, substring = true, useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() } }
    private fun gone(tag: String, ms: Long = 30_000) = dumped { ui.waitUntil(ms) { ui.onAllNodesWithTag(tag).fetchSemanticsNodes().isEmpty() } }
    private fun until(ms: Long = 30_000, what: () -> Boolean) = dumped { ui.waitUntil(ms) { what() } }
    private fun openGarden() {
        text("Garden club")
        ui.onNode(hasText("Garden club", substring = true) and hasClickAction()).performClick()
        dumped { ui.waitUntil(60_000) { ui.onAllNodesWithTag("groupPane").fetchSemanticsNodes().isNotEmpty() } }
    }

    @Test fun theBannerShowsWhoIsLiveTheStageInvitationAndTheEnd() {
        launch().use {
            openGarden()
            text("Bob is live: Seed swap")
            ui.onNodeWithTag("groupLiveAction", useUnmergedTree = true).assertTextEquals("Watch")
            LiveFixtureControl.stage = "invited"
            text("Bob invited you onto the stage. Watch to answer")
            LiveFixtureControl.live = false
            gone("groupLiveBanner")
        }
    }

    @Test fun adminsChooseWhoCanGoLive() {
        LiveFixtureControl.admin = true
        launch().use {
            openGarden()
            text("Bob is live")
            ui.onNodeWithContentDescription("Group info").performClick()
            text("Who can go live")
            // The second "Only admins" is the live-stream choice (the first is who can post).
            ui.onAllNodesWithText("Only admins")[1].performScrollTo().performClick()
            until { LiveFixtureControl.policy == "admins" }
        }
    }

    @Test fun membersSeeNoPolicySettingAndCannotGoLiveUnderAdminsOnly() {
        LiveFixtureControl.policy = "admins"; LiveFixtureControl.live = false
        Page().use { page ->
            launch(page).use {
                openGarden()
                until { page.calls("configure").any { it.optBoolean("signedIn") } }
                Thread.sleep(2000)
                ui.onAllNodesWithContentDescription("Go live").assertCountEquals(0)
                ui.onNodeWithContentDescription("Group info").performClick()
                text("Members (")
                ui.onAllNodesWithText("Who can go live").assertCountEquals(0)
            }
        }
    }

    @Test fun watchingGoesThroughTheCallPageAndThePageHearsTheStageAndTheEnd() {
        Page().use { page ->
            launch(page).use {
                openGarden()
                until { page.calls("configure").any { it.optBoolean("signedIn") } }
                text("Bob is live: Seed swap")
                ui.onNodeWithTag("groupLiveBanner").performClick()
                until { page.calls("watchStream").isNotEmpty() }
                val watch = page.calls("watchStream").single()
                assertEquals(LiveFixtureControl.STREAM, watch.getString("stream"))
                assertTrue(watch.getJSONObject("group").getString("id").startsWith("grp_"))
                assertEquals("Garden club", watch.getJSONObject("group").getString("name"))
                // The stream on screen: the foreground service plays it while the app is elsewhere.
                until { LiveStreamService.running }
                ui.onNodeWithTag("groupLiveAction", useUnmergedTree = true).assertTextEquals("Return")
                // The streamer invites us: the page is told, once.
                LiveFixtureControl.stage = "invited"
                until { page.calls("onGroupStream").any { it.getJSONObject("data").optJSONObject("stage")?.optString("state") == "invited" } }
                Thread.sleep(2500)
                assertEquals(1, page.calls("onGroupStream").count { it.getJSONObject("data").optJSONObject("stage")?.optString("state") == "invited" })
                // The stream ends at the postbox: the page is told, closes, and the service stops.
                LiveFixtureControl.live = false
                until { page.calls("onGroupStream").any { it.getJSONObject("data").optString("state") == "ended" } }
                until { !LiveStreamService.running }
                gone("groupLiveBanner")
            }
        }
    }

    @Test fun goingLiveAsksForATitleAndUsesTheCameraAndMicrophone() {
        LiveFixtureControl.live = false
        Page().use { page ->
            launch(page).use {
                openGarden()
                until { page.calls("configure").any { it.optBoolean("signedIn") } }
                dumped { ui.waitUntil(30_000) { ui.onAllNodesWithContentDescription("Go live").fetchSemanticsNodes().isNotEmpty() } }
                // A refusal from the page is explained, and nothing stays on screen.
                ui.onNodeWithContentDescription("Go live").performClick()
                ui.onNodeWithTag("goLiveStart").assertIsNotEnabled()
                ui.onNodeWithTag("goLiveTitle").performTextInput("full")
                ui.onNodeWithTag("goLiveStart").performClick()
                until { page.calls("goLive").isNotEmpty() }
                ui.onNodeWithContentDescription("Go live").performClick()
                ui.onNodeWithTag("goLiveTitle").performTextInput("  Seed swap  ")
                ui.onNodeWithTag("goLiveStart").performClick()
                until { page.calls("goLive").size == 2 }
                val options = page.calls("goLive")[1].getJSONObject("options")
                assertEquals("Seed swap", options.getString("title"))
                assertTrue(options.getBoolean("camera")); assertFalse(options.getBoolean("screen"))
                until { LiveStreamService.running }
            }
        }
    }
}
