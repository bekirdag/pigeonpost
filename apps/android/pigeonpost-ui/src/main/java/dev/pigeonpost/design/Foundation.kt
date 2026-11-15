package dev.pigeonpost.design

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * A kit icon (Lucide, ISC) tinted with [tint]. Pass [contentDescription] only when the icon carries
 * meaning on its own; decorative icons next to a label stay null.
 */
@Composable
fun PPIconView(
    icon: PPIcon,
    modifier: Modifier = Modifier,
    size: Dp = 20.dp,
    tint: Color = LocalContentColor.current,
    contentDescription: String? = null,
) {
    Icon(painterResource(icon.res), contentDescription, modifier.size(size), tint = tint)
}

/** A small ring spinner (css aria-busy buttons, loader-circle). */
@Composable
fun PPSpinner(modifier: Modifier = Modifier, size: Dp = 16.dp, color: Color = LocalContentColor.current, strokeWidth: Dp = 2.dp) {
    val t = rememberInfiniteTransition(label = "spin")
    val angle by t.animateFloat(0f, 360f, infiniteRepeatable(tween(800, easing = LinearEasing)), label = "angle")
    Canvas(modifier.size(size).rotate(angle)) {
        val s = strokeWidth.toPx()
        drawArc(color.copy(alpha = 0.25f), 0f, 360f, false, Offset(s / 2, s / 2), Size(this.size.width - s, this.size.height - s), style = Stroke(s))
        drawArc(color, 0f, 90f, false, Offset(s / 2, s / 2), Size(this.size.width - s, this.size.height - s), style = Stroke(s, cap = StrokeCap.Round))
    }
}

@PPPreviews
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun IconsPreview() = PPPreviewSurface {
    Text("181 icons, 24 x 24, stroke 2", style = PP.type.caption, color = PP.colors.textMuted)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        PPIcon.entries.take(60).forEach { PPIconView(it, tint = PP.colors.textBody) }
    }
    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        PPSpinner(color = PP.colors.accent)
        PPSpinner(size = 24.dp, color = PP.colors.textMuted)
    }
}
