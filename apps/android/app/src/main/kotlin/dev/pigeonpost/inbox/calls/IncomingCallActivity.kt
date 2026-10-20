package dev.pigeonpost.inbox.calls

import android.app.KeyguardManager
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import dev.pigeonpost.core.CallRings
import dev.pigeonpost.core.RingClient
import dev.pigeonpost.inbox.ui.Avatar
import dev.pigeonpost.inbox.ui.PigeonpostTheme
import dev.pigeonpost.inbox.ui.Pp
import dev.pigeonpost.inbox.ui.geist
import dev.pigeonpost.inbox.ui.geistMono
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The incoming-call screen: over the lock screen, screen turned on, like the phone app's. The ringing
 * itself is the notification's; this screen shows who is calling and answers or declines. While it is
 * up it asks the calls service every 2.5 s whether the ring is still live, so a call answered on
 * another device or hung up by the caller takes it down even when no `call_cancel` push arrives.
 */
class IncomingCallActivity : ComponentActivity() {
    private var ring by mutableStateOf<IncomingRing?>(null)
    private var ended by mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        if (Build.VERSION.SDK_INT >= 27) { setShowWhenLocked(true); setTurnScreenOn(true) }
        else @Suppress("DEPRECATION") window.addFlags(WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (!adopt(intent)) return
        setContent { PigeonpostTheme(darkTheme = true) { ring?.let { IncomingCallScreen(it, ended, ::answer, ::decline) } } }
        // Taken down elsewhere: cancel push, the page's ring_ended, expiry, a decline from the shade.
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.CREATED) {
                IncomingCalls.rings.collect { live ->
                    val current = ring ?: return@collect
                    if (ended == null && current.room !in live) finishWith(IncomingCalls.endings.value[current.room] ?: "ended")
                }
            }
        }
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.STARTED) {
                while (true) {
                    val current = ring ?: break
                    if (ended != null) break
                    if (current.expiresAt * 1000 <= System.currentTimeMillis()) { IncomingCalls.end(this@IncomingCallActivity, current.room, "no_answer"); break }
                    val token = IncomingCalls.rings.value[current.room]?.ringToken ?: current.ringToken
                    if (token != null) {
                        val origin = IncomingCalls.origin(applicationContext)
                        val state = withContext(Dispatchers.IO) { RingClient(origin).state(current.room, token) }
                        if (state != null && !state.ringing) { IncomingCalls.end(this@IncomingCallActivity, current.room, state.reason ?: "ended"); break }
                    }
                    delay(2500)
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // A newer ring replaces the one on screen; the older one keeps ringing in the shade.
        val next = IncomingCalls.fromIntent(intent) ?: return
        if (next.room != ring?.room && next.room !in IncomingCalls.endings.value) { setIntent(intent); ended = null; adopt(intent) }
    }

    /** The ring this screen is for, or finish: a stale intent (ring over, another account) shows nothing. */
    private fun adopt(intent: Intent?): Boolean {
        val value = IncomingCalls.fromIntent(intent)
        val live = value != null && value.room !in IncomingCalls.endings.value && value.expiresAt * 1000 > System.currentTimeMillis() &&
            value.identity == IncomingCalls.identity(this)
        if (!live) { finish(); return false }
        IncomingCalls.restore(value!!)
        ring = value
        return true
    }

    private fun answer() {
        val current = ring?.let { IncomingCalls.rings.value[it.room] ?: it } ?: return
        val open = {
            IncomingCalls.answered(this, current)
            startActivity(IncomingCalls.answerIntent(this, current))
            finish()
        }
        // Behind a PIN the person unlocks first; the call itself never shows over the lock screen.
        val keyguard = getSystemService(KeyguardManager::class.java)
        if (Build.VERSION.SDK_INT >= 27 && keyguard.isKeyguardLocked) {
            keyguard.requestDismissKeyguard(this, object : KeyguardManager.KeyguardDismissCallback() {
                override fun onDismissSucceeded() = open()
            })
        } else open()
    }

    private fun decline() {
        val current = ring ?: return
        IncomingCalls.decline(applicationContext, current)
    }

    private fun finishWith(reason: String) {
        ended = reason
        lifecycleScope.launch { delay(if (reason == "answered_here" || reason == "declined") 0 else 1600); finish() }
    }
}

@Composable
private fun IncomingCallScreen(ring: IncomingRing, ended: String?, answer: () -> Unit, decline: () -> Unit) {
    val p = Pp.colors
    Column(Modifier.fillMaxSize().background(Color(0xFF0D0F12)).systemBarsPadding().padding(horizontal = 24.dp, vertical = 32.dp),
        horizontalAlignment = Alignment.CenterHorizontally) {
        Text("Pigeonpost", style = geist(14.sp, FontWeight.Medium), color = p.secondary)
        Spacer(Modifier.height(6.dp))
        Text(ended?.let(CallRings::endedText) ?: if (ring.video) "Incoming video call" else "Incoming voice call",
            style = geist(16.sp), color = if (ended != null) p.amber else p.body)
        Spacer(Modifier.weight(1f))
        Avatar(ring.callerAddress, ring.callerName, 112.dp)
        Spacer(Modifier.height(20.dp))
        Text(ring.callerName, style = geist(30.sp, FontWeight.SemiBold), color = p.ink, textAlign = TextAlign.Center, maxLines = 2, overflow = TextOverflow.Ellipsis)
        ring.callerDetail?.let {
            Spacer(Modifier.height(6.dp))
            Text(it, style = geistMono(14.sp), color = p.secondary, textAlign = TextAlign.Center, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Spacer(Modifier.weight(1.4f))
        if (ended == null) Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
            RoundAction("Decline", Icons.Filled.CallEnd, Color(0xFFE5484D), decline)
            RoundAction("Answer", if (ring.video) Icons.Filled.Videocam else Icons.Filled.Call, Color(0xFF2EA043), answer)
        }
        Spacer(Modifier.height(24.dp))
    }
}

@Composable
private fun RoundAction(label: String, icon: ImageVector, fill: Color, action: () -> Unit) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        IconButton(action, Modifier.size(76.dp).semantics { contentDescription = label },
            colors = IconButtonDefaults.iconButtonColors(containerColor = fill, contentColor = Color.White)) {
            Icon(icon, null, Modifier.size(34.dp))
        }
        Spacer(Modifier.height(10.dp))
        Text(label, style = geist(15.sp, FontWeight.Medium), color = Pp.colors.body)
    }
}
