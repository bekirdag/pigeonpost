package dev.pigeonpost.design

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/** css .pp-callbtn looks. Off = a device that is off (light disc); Active = pressed toggle (accent). */
enum class PPCallButtonKind { Default, Off, Active, Sharing, Leave, Answer, Decline }

private val CallBarBg = Color(0xB8141820) // rgba(20, 24, 32, .72)
private val CallBtnBg = Color(0x1FFFFFFF) // white 12%
private val OffBg = Color(0xFFF2F4F7)
private val OffFg = Color(0xFF12161C)

/**
 * Round call button (css .pp-callbtn): 36 / 48 / 56 / 64dp. [label] renders under it (labelled
 * mobile bars). [onSurface] uses the light-page colours (css .pp-callbar--surface).
 */
@Composable
fun PPCallButton(
    icon: PPIcon,
    contentDescription: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    kind: PPCallButtonKind = PPCallButtonKind.Default,
    size: Dp = 56.dp,
    label: String? = null,
    toggledOn: Boolean? = null,
    onSurface: Boolean = false,
    enabled: Boolean = true,
) {
    val c = PP.colors
    val (bg, fg) = when (kind) {
        PPCallButtonKind.Default -> if (onSurface) c.surfaceSunken to c.text else CallBtnBg to Color.White
        PPCallButtonKind.Off -> if (onSurface) c.text to c.surface else OffBg to OffFg
        PPCallButtonKind.Active -> c.accent to c.onAccent
        PPCallButtonKind.Sharing, PPCallButtonKind.Leave, PPCallButtonKind.Decline -> c.dangerSolid to Color.White
        PPCallButtonKind.Answer -> c.successSolid to Color.White
    }
    val rotatePhone = kind == PPCallButtonKind.Leave || kind == PPCallButtonKind.Decline
    val iconSize = when {
        size >= 64.dp -> 28.dp
        size <= 36.dp -> 18.dp
        else -> 22.dp
    }
    Column(
        modifier.semantics(mergeDescendants = true) {},
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Box(
            Modifier
                .size(size)
                .alpha(if (enabled) 1f else 0.45f)
                .clip(CircleShape)
                .background(bg)
                .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
                .semantics {
                    this.contentDescription = contentDescription
                    if (toggledOn != null) stateDescription = if (toggledOn) "On" else "Off"
                },
            contentAlignment = Alignment.Center,
        ) { PPIconView(icon, Modifier.rotate(if (rotatePhone) 135f else 0f), size = iconSize, tint = fg) }
        if (label != null) {
            Text(label, style = PP.type.caption.copy(fontSize = 11.5.sp, lineHeight = 14.sp), color = if (onSurface) c.textMuted else Color.White.copy(alpha = 0.8f))
        }
    }
}

/**
 * In-call control bar (css .pp-callbar--labeled): microphone, camera, speaker and screen share
 * toggles, then the red leave pill. A device that is off shows its slashed icon on a light disc.
 */
@Composable
fun PPCallControls(
    micOn: Boolean,
    cameraOn: Boolean,
    onToggleMic: () -> Unit,
    onToggleCamera: () -> Unit,
    onLeave: () -> Unit,
    modifier: Modifier = Modifier,
    speakerOn: Boolean? = null,
    onToggleSpeaker: () -> Unit = {},
    sharing: Boolean? = null,
    onToggleShare: () -> Unit = {},
    labeled: Boolean = true,
    onSurface: Boolean = false,
) {
    val c = PP.colors
    val shape = if (labeled) RoundedCornerShape(PPRadius.x2xl) else CircleShape
    val btn = if (labeled) 56.dp else 48.dp
    Row(
        modifier
            .shadow(12.dp, shape)
            .clip(shape)
            .background(if (onSurface) c.surface else CallBarBg)
            .border(1.dp, if (onSurface) c.border else Color.White.copy(alpha = 0.08f), shape)
            .padding(horizontal = if (labeled) 14.dp else 8.dp, vertical = if (labeled) 12.dp else 8.dp),
        horizontalArrangement = Arrangement.spacedBy(if (labeled) 14.dp else 10.dp),
        verticalAlignment = Alignment.Top,
    ) {
        PPCallButton(
            if (micOn) PPIcon.Mic else PPIcon.MicOff, if (micOn) "Mute microphone" else "Unmute microphone", onToggleMic,
            kind = if (micOn) PPCallButtonKind.Default else PPCallButtonKind.Off, size = btn,
            label = if (labeled) (if (micOn) "Mute" else "Unmute") else null, toggledOn = micOn, onSurface = onSurface,
        )
        PPCallButton(
            if (cameraOn) PPIcon.Video else PPIcon.VideoOff, if (cameraOn) "Turn camera off" else "Turn camera on", onToggleCamera,
            kind = if (cameraOn) PPCallButtonKind.Default else PPCallButtonKind.Off, size = btn,
            label = if (labeled) "Camera" else null, toggledOn = cameraOn, onSurface = onSurface,
        )
        if (speakerOn != null) {
            PPCallButton(
                PPIcon.Volume2, "Speaker", onToggleSpeaker,
                kind = if (speakerOn) PPCallButtonKind.Active else PPCallButtonKind.Default, size = btn,
                label = if (labeled) "Speaker" else null, toggledOn = speakerOn, onSurface = onSurface,
            )
        }
        if (sharing != null) {
            PPCallButton(
                PPIcon.ScreenShare, if (sharing) "Stop sharing" else "Share screen", onToggleShare,
                kind = if (sharing) PPCallButtonKind.Sharing else PPCallButtonKind.Default, size = btn,
                label = if (labeled) (if (sharing) "Stop" else "Share") else null, toggledOn = sharing, onSurface = onSurface,
            )
        }
        PPCallButton(PPIcon.Phone, "Leave call", onLeave, kind = PPCallButtonKind.Leave, size = btn, label = if (labeled) "Leave" else null, onSurface = onSurface)
    }
}

/**
 * Full-screen incoming call (css .pp-call-ring): caller name and handle over an accent glow, quick
 * replies, then decline and answer (64dp; answer bobs gently). Use it as the content of a
 * full-screen intent activity or an overlay.
 */
@Composable
fun PPIncomingCallView(
    peer: String,
    onDecline: () -> Unit,
    onAnswer: () -> Unit,
    modifier: Modifier = Modifier,
    name: String? = null,
    video: Boolean = false,
    subtitle: String? = null,
    onMessage: (() -> Unit)? = null,
    onRemind: (() -> Unit)? = null,
) {
    val c = PP.colors
    val t = rememberInfiniteTransition(label = "ring")
    val bob by t.animateFloat(0f, 1f, infiniteRepeatable(tween(800, easing = FastOutSlowInEasing), RepeatMode.Reverse), label = "bob")
    val glow = lerp(c.bg, c.accent, 0.3f)
    Column(
        modifier
            .fillMaxSize()
            .background(c.bg)
            .drawBehind {
                // css: radial-gradient(90% 60% at 50% 20%, accent 30% over bg, bg 75%)
                drawRect(Brush.radialGradient(listOf(glow, c.bg), center = androidx.compose.ui.geometry.Offset(size.width / 2, size.height * 0.2f), radius = size.width * 0.9f))
            }
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(start = 28.dp, end = 28.dp, top = 72.dp, bottom = 44.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            PPIconView(if (video) PPIcon.Video else PPIcon.Phone, size = 15.dp, tint = c.textMuted)
            Text(if (video) "Pigeonpost video call" else "Pigeonpost audio call", style = PP.type.bodyS.copy(fontSize = 13.sp), color = c.textMuted)
        }
        Spacer(Modifier.height(18.dp))
        PPAvatar(peer, name = name, size = PPAvatarSize.x2l)
        Spacer(Modifier.height(10.dp))
        Text(
            name ?: PeerFace.displayName(peer),
            Modifier.semantics { heading() },
            style = PP.type.h1.copy(fontSize = 30.sp, lineHeight = 36.sp, letterSpacing = (-0.01).em),
            color = c.text,
            textAlign = TextAlign.Center,
        )
        Text(subtitle ?: peer, style = PP.type.body.copy(fontSize = 14.sp), color = c.textMuted, textAlign = TextAlign.Center)
        Spacer(Modifier.weight(1f))
        if (onMessage != null || onRemind != null) {
            Row(Modifier.padding(bottom = 36.dp), horizontalArrangement = Arrangement.spacedBy(48.dp)) {
                if (onRemind != null) QuickAction(PPIcon.Clock, "Remind me", onRemind)
                if (onMessage != null) QuickAction(PPIcon.MessageCircle, "Message", onMessage)
            }
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceAround, verticalAlignment = Alignment.Top) {
            RingAction("Decline") { PPCallButton(PPIcon.Phone, "Decline call", onDecline, kind = PPCallButtonKind.Decline, size = 64.dp) }
            RingAction("Answer") {
                PPCallButton(
                    if (video) PPIcon.Video else PPIcon.Phone, "Answer call", onAnswer,
                    Modifier.graphicsLayer { translationY = -6.dp.toPx() * bob },
                    kind = PPCallButtonKind.Answer, size = 64.dp,
                )
            }
        }
    }
}

@Composable
private fun RingAction(label: String, button: @Composable () -> Unit) {
    Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        button()
        Text(label, style = PP.type.bodyS.copy(fontSize = 13.sp), color = PP.colors.textBody)
    }
}

@Composable
private fun QuickAction(icon: PPIcon, label: String, onClick: () -> Unit) {
    val c = PP.colors
    Column(
        Modifier.clip(RoundedCornerShape(PPRadius.m)).clickable(role = Role.Button, onClick = onClick).padding(8.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Box(Modifier.size(44.dp).clip(CircleShape).background(c.surfaceSunken), contentAlignment = Alignment.Center) {
            PPIconView(icon, size = 20.dp, tint = c.textBody)
        }
        Text(label, style = PP.type.caption, color = c.textMuted)
    }
}

/** Call timer pill (css .pp-call-timer): mono digits with a red recording dot when [recording]. */
@Composable
fun PPCallTimer(elapsed: String, modifier: Modifier = Modifier, recording: Boolean = false) {
    Row(
        modifier.height(28.dp).clip(CircleShape).background(CallBarBg).padding(horizontal = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (recording) Box(Modifier.size(8.dp).clip(CircleShape).background(PP.colors.dangerSolid))
        Text(elapsed, style = PP.type.mono.copy(fontSize = 13.sp, lineHeight = 13.sp, fontWeight = FontWeight.Medium), color = Color.White)
    }
}

@PPPreviews
@Composable
private fun CallControlsPreview() = PPPreviewSurface {
    Box(Modifier.fillMaxWidth().clip(RoundedCornerShape(PPRadius.xl)).background(PPPalette.gray900).padding(16.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(16.dp)) {
            PPCallTimer("12:48", recording = true)
            var mic by remember { mutableStateOf(true) }
            PPCallControls(mic, false, { mic = !mic }, {}, {}, speakerOn = true, sharing = false)
            PPCallControls(true, true, {}, {}, {}, labeled = false, sharing = true)
        }
    }
    PPCallControls(false, true, {}, {}, {}, onSurface = true, labeled = false)
}

@Preview(name = "Incoming call", widthDp = 390, heightDp = 780)
@Preview(name = "Incoming call, dark", widthDp = 390, heightDp = 780, uiMode = android.content.res.Configuration.UI_MODE_NIGHT_YES)
@Composable
private fun IncomingCallPreview() = PPTheme {
    PPIncomingCallView("/ada", {}, {}, name = "Ada Yılmaz", video = true, subtitle = "/ada · Wodo Network", onMessage = {}, onRemind = {})
}
