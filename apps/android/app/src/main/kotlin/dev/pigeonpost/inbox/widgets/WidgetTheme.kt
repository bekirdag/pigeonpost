package dev.pigeonpost.inbox.widgets

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.glance.color.ColorProvider
import androidx.glance.text.FontFamily
import androidx.glance.text.FontWeight
import androidx.glance.text.TextStyle
import androidx.glance.unit.ColorProvider
import dev.pigeonpost.design.PPColors
import dev.pigeonpost.design.PPDark
import dev.pigeonpost.design.PPLight
import dev.pigeonpost.design.PPRadius
import dev.pigeonpost.design.PPTone
import dev.pigeonpost.design.PPType
import dev.pigeonpost.design.PeerFace
import dev.pigeonpost.design.ppToneColors

/**
 * The design kit's tokens as widget colours (pigeonpost-ui PPTokens), light and dark, resolved by
 * the launcher's night mode. Fixed kit colours, never Material You: the widgets read as Pigeonpost
 * next to any wallpaper. Contrast pairs are the kit's own (text on surface, onAccent on accent).
 *
 * RemoteViews cannot load bundled fonts, so text is the system sans serif at the kit's sizes and
 * weights; Geist stays inside the app.
 */
object WidgetColors {
    private fun pair(pick: (PPColors) -> Color) = ColorProvider(day = pick(PPLight), night = pick(PPDark))
    val surface = pair { it.surface }
    val raised = pair { it.surfaceSunken }
    val border = pair { it.border }
    val text = pair { it.text }
    val body = pair { it.textBody }
    val muted = pair { it.textMuted }
    val link = pair { it.textLink }
    val accent = pair { it.accent }
    val onAccent = pair { it.onAccent }
    val accentSubtle = pair { it.accentSubtle }
    val success = pair { it.success }
    val successFill = pair { it.successFill }
    val warning = pair { it.warning }
    val warningFill = pair { it.warningFill }
    val danger = pair { it.danger }
    val dangerFill = pair { it.dangerFill }
    val info = pair { it.info }
    val infoFill = pair { it.infoFill }
    val track = pair { it.surfaceHover }

    /** A peer keeps the same face colours as in the app and on the web (PeerFace.toneIndex). */
    fun face(peer: String): Pair<ColorProvider, ColorProvider> = tone(PeerFace.tone(peer))
    fun tone(tone: PPTone): Pair<ColorProvider, ColorProvider> {
        val day = ppToneColors(tone, PPLight, false); val night = ppToneColors(tone, PPDark, true)
        return ColorProvider(day = day.background, night = night.background) to ColorProvider(day = day.foreground, night = night.foreground)
    }
}

object WidgetType {
    val title = PPType.h4
    val count = PPType.h1
    val countLarge = PPType.displayM
    val body = PPType.body
    val bodySmall = PPType.bodyS
    val caption = PPType.caption
    val overline = PPType.overline
    fun style(size: TextUnit, color: ColorProvider = WidgetColors.text, weight: FontWeight = FontWeight.Normal) =
        TextStyle(color = color, fontSize = size, fontWeight = weight, fontFamily = FontFamily.SansSerif)
}

object WidgetSpace {
    val gutter = 12.dp
    val gutterLarge = 16.dp
    val row = 8.dp
    val radius = PPRadius.x2xl
    val inner = PPRadius.l
}
