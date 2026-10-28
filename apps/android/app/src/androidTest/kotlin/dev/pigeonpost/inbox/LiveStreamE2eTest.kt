package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.lifecycle.ViewModelProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.pigeonpost.inbox.calls.LiveStreamService
import org.json.JSONObject
import org.json.JSONTokener
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * The app watching a real end-to-end encrypted stream through the real call page (native-call.html,
 * live.js) in its WebView: driven by tests/e2e-streams/android-app.mjs (local LiveKit, calls service,
 * mock postbox, a streamer in headless Chrome, synthetic media). Skipped unless that harness passes
 * `-e streamPage <url> -e stream <id> -e group <id> -e postbox <url>`.
 */
@RunWith(AndroidJUnit4::class)
class LiveStreamE2eTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val args = InstrumentationRegistry.getArguments()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()

    private fun eval(scenario: ActivityScenario<MainActivity>, script: String): String? {
        var model: InboxViewModel? = null
        scenario.onActivity { model = ViewModelProvider(it)[InboxViewModel::class.java] }
        val latch = CountDownLatch(1); var out: String? = null
        Handler(Looper.getMainLooper()).post { model!!.calls!!.webView.evaluateJavascript(script) { out = it; latch.countDown() } }
        latch.await(10, TimeUnit.SECONDS)
        return out?.let { (JSONTokener(it).nextValue() as? String) ?: it }
    }
    private fun report(scenario: ActivityScenario<MainActivity>, phase: String, body: String) =
        eval(scenario, "fetch('/__report',{method:'POST',body:JSON.stringify(Object.assign({engine:'android-app',phase:'$phase',ua:navigator.userAgent},$body))});'sent'")

    @Test fun watchesARealEncryptedStreamAndAnswersTheStageInvitation() {
        val page = args.getString("streamPage"); assumeTrue(page != null)
        LiveFixtureControl.reset()
        val intent = Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "live")
            .putExtra("pigeonpost.calls.page", page).putExtra("pigeonpost.calls.token", "tok-v1")
            .putExtra("pigeonpost.calls.mailbox", "/k/v1").putExtra("pigeonpost.calls.handle", "/v1")
            .putExtra("pigeonpost.fixture.stream", args.getString("stream")).putExtra("pigeonpost.fixture.streams", args.getString("postbox"))
            .putExtra("pigeonpost.fixture.streams.token", "tok-v1").putExtra("pigeonpost.fixture.streams.group", args.getString("group"))
        ActivityScenario.launch<MainActivity>(intent).use { scenario ->
            ui.waitUntil(60_000) { ui.onAllNodesWithText("Garden club", substring = true).fetchSemanticsNodes().isNotEmpty() }
            ui.onNode(hasText("Garden club", substring = true) and hasClickAction()).performClick()
            ui.waitUntil(60_000) { ui.onAllNodesWithTag("groupLiveBanner").fetchSemanticsNodes().isNotEmpty() }
            // Wait for the page to be signed in, then watch from the native banner.
            ui.waitUntil(30_000) { eval(scenario, "String(Boolean(window.PigeonpostCalls && PigeonpostCalls.inspect && PigeonpostCalls.inspect().session))") == "true" }
            ui.onNodeWithTag("groupLiveBanner").performClick()
            // Decoding: the page's own statistics, reported to the harness.
            ui.waitUntil(90_000) { eval(scenario, "String(Boolean(window.PigeonpostLive && PigeonpostLive.inspect() && PigeonpostLive.inspect().epoch > 0))") == "true" }
            ui.waitUntil(30_000) { LiveStreamService.running }
            repeat(3) {
                Thread.sleep(4000)
                eval(scenario, "PigeonpostLive.stats().then(s=>fetch('/__report',{method:'POST',body:JSON.stringify({engine:'android-app',phase:'stats',inspect:PigeonpostLive.inspect(),stats:s,service:${LiveStreamService.running}})}));'ok'")
            }
            // The harness's streamer invites us onto the stage; the app reads it from the postbox and the page offers it.
            ui.waitUntil(90_000) { eval(scenario, "String(Boolean(document.getElementById('live-stage-offer') && !document.getElementById('live-stage-offer').hidden))") == "true" }
            report(scenario, "stage_offer", "{text:document.getElementById('live-stage-text').textContent}")
            // The streamer ends the stream: the page closes it and the foreground service stops.
            ui.waitUntil(90_000) { eval(scenario, "String(Boolean(window.PigeonpostLive && !PigeonpostLive.active()))") == "true" }
            ui.waitUntil(30_000) { !LiveStreamService.running }
            report(scenario, "ended", "{service:${LiveStreamService.running}}")
        }
    }
}
