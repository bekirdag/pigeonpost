package dev.pigeonpost.design

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Feedback tones shared by banners, toasts and meters. */
enum class PPStatus { Neutral, Info, Success, Warning, Danger }

private data class StatusPaint(val icon: Color, val bg: Color, val border: Color, val solid: Color, val glyph: PPIcon)

@Composable
private fun statusPaint(s: PPStatus): StatusPaint {
    val c = PP.colors
    return when (s) {
        PPStatus.Neutral -> StatusPaint(c.textMuted, c.surfaceSunken, c.border, PPPalette.gray600, PPIcon.Info)
        PPStatus.Info -> StatusPaint(c.info, c.infoFill, c.infoBorder, c.infoSolid, PPIcon.Info)
        PPStatus.Success -> StatusPaint(c.success, c.successFill, c.successBorder, PPPalette.green700, PPIcon.CircleCheck)
        PPStatus.Warning -> StatusPaint(c.warning, c.warningFill, c.warningBorder, PPPalette.amber600, PPIcon.AlertTriangle)
        PPStatus.Danger -> StatusPaint(c.danger, c.dangerFill, c.dangerBorder, PPPalette.red600, PPIcon.AlertCircle)
    }
}

/**
 * Inline alert / banner (css .pp-alert). [solid] is the white-on-colour variant for top-of-screen
 * banners. [onDismiss] adds a close button. Announced politely when it appears.
 */
@Composable
fun PPBanner(
    title: String,
    modifier: Modifier = Modifier,
    text: String? = null,
    status: PPStatus = PPStatus.Info,
    icon: PPIcon? = null,
    solid: Boolean = false,
    onDismiss: (() -> Unit)? = null,
    actions: (@Composable RowScope.() -> Unit)? = null,
) {
    val c = PP.colors
    val p = statusPaint(status)
    val shape = RoundedCornerShape(PPRadius.l)
    val fg = if (solid) Color.White else c.text
    Row(
        modifier
            .fillMaxWidth()
            .clip(shape)
            .background(if (solid) p.solid else p.bg)
            .then(if (solid) Modifier else Modifier.border(1.dp, p.border, shape))
            .semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite }
            .padding(horizontal = 16.dp, vertical = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        PPIconView(icon ?: p.glyph, size = 20.dp, tint = if (solid) Color.White else p.icon)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = PP.type.bodyS.copy(fontWeight = FontWeight.SemiBold, lineHeight = 20.sp), color = fg)
            if (text != null) Text(text, style = PP.type.bodyS, color = if (solid) Color.White.copy(alpha = 0.9f) else c.textBody)
            if (actions != null) Row(Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp), content = actions)
        }
        if (onDismiss != null) {
            Box(
                Modifier.offset(x = 8.dp, y = (-6).dp).size(32.dp).clip(CircleShape).clickable(role = Role.Button, onClick = onDismiss).semantics { contentDescription = "Dismiss" },
                contentAlignment = Alignment.Center,
            ) { PPIconView(PPIcon.X, size = 16.dp, tint = if (solid) Color.White else c.textMuted) }
        }
    }
}

/**
 * Toast (css .pp-toast): raised surface, shadow 3, radius 14. Show it from a host such as a
 * SnackbarHost or an overlay; this composable is the visual only. [dark] is the inverse variant.
 */
@Composable
fun PPToast(
    title: String,
    modifier: Modifier = Modifier,
    text: String? = null,
    status: PPStatus = PPStatus.Neutral,
    icon: PPIcon? = null,
    leading: (@Composable () -> Unit)? = null,
    actionLabel: String? = null,
    onAction: (() -> Unit)? = null,
    onDismiss: (() -> Unit)? = null,
    dark: Boolean = false,
) {
    val c = PP.colors
    val p = statusPaint(status)
    val shape = RoundedCornerShape(PPRadius.l)
    val bg = if (dark) c.surfaceInverse else c.surfaceRaised
    val fg = if (dark) c.onInverse else c.text
    Row(
        modifier
            .fillMaxWidth()
            .shadow(12.dp, shape, ambientColor = Color(0x1F12161C), spotColor = Color(0x1F12161C))
            .clip(shape)
            .background(bg)
            .then(if (dark) Modifier else Modifier.border(1.dp, c.border, shape))
            .semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite }
            .padding(start = 14.dp, end = 12.dp, top = 12.dp, bottom = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        when {
            leading != null -> leading()
            icon != null || status != PPStatus.Neutral -> PPIconView(icon ?: p.glyph, size = 20.dp, tint = if (dark) fg else p.icon)
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(title, style = PP.type.bodyS.copy(fontWeight = if (text == null) FontWeight.Medium else FontWeight.SemiBold), color = fg)
            if (text != null) Text(text, style = PP.type.bodyS, color = if (dark) fg.copy(alpha = 0.75f) else c.textMuted)
        }
        if (actionLabel != null && onAction != null) {
            Text(
                actionLabel,
                Modifier.clip(RoundedCornerShape(PPRadius.s)).clickable(role = Role.Button, onClick = onAction).padding(horizontal = 10.dp, vertical = 8.dp),
                style = PP.type.bodyS.copy(fontSize = 13.sp, fontWeight = FontWeight.SemiBold),
                color = if (dark) PPPalette.blue300 else c.textLink,
            )
        }
        if (onDismiss != null) {
            Box(
                Modifier.size(28.dp).clip(CircleShape).clickable(role = Role.Button, onClick = onDismiss).semantics { contentDescription = "Dismiss" },
                contentAlignment = Alignment.Center,
            ) { PPIconView(PPIcon.X, size = 16.dp, tint = if (dark) fg else c.textMuted) }
        }
    }
}

/**
 * Progress bar (css .pp-progress): 6dp track. [value] 0..1; null is indeterminate.
 */
@Composable
fun PPProgress(value: Float?, modifier: Modifier = Modifier, status: PPStatus = PPStatus.Info, height: Dp = 6.dp, label: String? = null) {
    val c = PP.colors
    val fill = when (status) {
        PPStatus.Warning -> c.warningSolid
        PPStatus.Danger -> c.dangerSolid
        PPStatus.Success -> c.successSolid
        else -> c.accent
    }
    val animated by animateFloatAsState((value ?: 0f).coerceIn(0f, 1f), tween(PPDuration.slow, easing = FastOutSlowInEasing), label = "progress")
    BoxWithConstraints(
        modifier
            .fillMaxWidth()
            .height(height)
            .clip(CircleShape)
            .background(c.surfaceSunken)
            .border(1.dp, c.borderSubtle, CircleShape)
            .semantics {
                if (label != null) contentDescription = label
                progressBarRangeInfo = if (value == null) ProgressBarRangeInfo.Indeterminate else ProgressBarRangeInfo(value, 0f..1f)
            },
    ) {
        if (value == null) {
            val t = rememberInfiniteTransition(label = "indeterminate")
            val x by t.animateFloat(-0.4f, 1f, infiniteRepeatable(tween(1400, easing = LinearEasing)), label = "x")
            Box(Modifier.offset(x = maxWidth * x).width(maxWidth * 0.4f).fillMaxHeight().clip(CircleShape).background(fill))
        } else {
            Box(Modifier.width(maxWidth * animated).fillMaxHeight().clip(CircleShape).background(fill))
        }
    }
}

/** Usage meter (css .pp-meter): label + value above a progress track. */
@Composable
fun PPMeter(label: String, value: String, fraction: Float, modifier: Modifier = Modifier, status: PPStatus = PPStatus.Info) {
    val c = PP.colors
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(label, style = PP.type.caption, color = c.textMuted)
            Text(value, style = PP.type.caption.copy(fontWeight = FontWeight.Medium), color = c.text)
        }
        PPProgress(fraction, status = status, label = "$label: $value")
    }
}

/** Loading placeholder with the kit shimmer (css .pp-skeleton). Hidden from accessibility. */
@Composable
fun PPSkeleton(modifier: Modifier = Modifier, shape: Shape = RoundedCornerShape(PPRadius.xs)) {
    val colors = if (PP.isDark) PPEffects.skeletonDark else PPEffects.skeletonLight
    val t = rememberInfiniteTransition(label = "shimmer")
    val x by t.animateFloat(0f, 1f, infiniteRepeatable(tween(1400, easing = FastOutSlowInEasing)), label = "x")
    BoxWithConstraints(modifier.clip(shape)) {
        val w = constraints.maxWidth.toFloat().coerceAtLeast(1f)
        val start = -w + 2 * w * x
        Box(
            Modifier.matchParentSize().background(
                Brush.linearGradient(listOf(colors[0], colors[1], colors[0]), start = Offset(start, 0f), end = Offset(start + w, 0f)),
            ),
        )
    }
}

/** A thread-row shaped skeleton for inbox loading states. */
@Composable
fun PPSkeletonThreadRow(modifier: Modifier = Modifier) {
    Row(modifier.fillMaxWidth().padding(10.dp), horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
        PPSkeleton(Modifier.size(44.dp), CircleShape)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            PPSkeleton(Modifier.fillMaxWidth(0.45f).height(14.dp), RoundedCornerShape(6.dp))
            PPSkeleton(Modifier.fillMaxWidth(0.8f).height(12.dp), RoundedCornerShape(6.dp))
        }
    }
}

@PPPreviews
@Composable
private fun FeedbackPreview() = PPPreviewSurface {
    PPBanner("Your Nest is paused", text = "Running hours for November are used up. Add hours or wait until 1 December.", status = PPStatus.Warning) {
        PPButton("Add hours", {}, size = PPButtonSize.S)
    }
    PPBanner("Payment received", text = "$20.00 was added to your credit balance.", status = PPStatus.Success, onDismiss = {})
    PPBanner("You are offline", status = PPStatus.Danger, solid = true)
    PPToast("Message sent to /wodo/hr", status = PPStatus.Success, actionLabel = "Undo", onAction = {})
    PPToast("Ada Yılmaz", text = "Can you join the 14:30 call?", leading = { PPAvatar("/ada", name = "Ada Yılmaz", size = 32.dp) }, onDismiss = {})
    PPToast("Copied /bekir to the clipboard", dark = true)
    PPProgress(0.62f, label = "Uploading")
    PPProgress(null)
    PPMeter("Output tokens", "88K of 100K", 0.88f, status = PPStatus.Warning)
    PPSkeletonThreadRow()
}
