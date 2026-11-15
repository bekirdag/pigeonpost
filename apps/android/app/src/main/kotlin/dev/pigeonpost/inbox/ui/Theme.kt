package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.sp
import androidx.compose.ui.graphics.lerp
import dev.pigeonpost.design.GeistFamily
import dev.pigeonpost.design.GeistMonoFamily
import dev.pigeonpost.design.PP
import dev.pigeonpost.design.PPColors
import dev.pigeonpost.design.PPDark
import dev.pigeonpost.design.PPLight
import dev.pigeonpost.design.PPPalette
import dev.pigeonpost.design.PPRadius
import dev.pigeonpost.design.PPTheme
import dev.pigeonpost.design.PPTone
import dev.pigeonpost.design.ppToneColors

// The Pigeonpost Design Kit (apps/android/pigeonpost-ui, design.pigeonpost.dev) is the source of
// truth: every colour below is one of the kit's semantic roles (PPLight / PPDark), named for the
// places this app already uses it, so the screens keep their structure and take the kit's look.

@Immutable
data class PpColors(
    val dark: Boolean,
    val accent: Color, val onAccent: Color, val found: Color, val green: Color, val amber: Color,
    val ink: Color, val body: Color, val secondary: Color, val muted: Color, val quiet: Color,
    val app: Color, val sidebar: Color, val column: Color, val wash: Color, val ground: Color,
    val field: Color, val composer: Color, val selected: Color, val chip: Color, val tile: Color,
    val rule: Color, val stroke: Color, val strokeStrong: Color,
    val mineFill: Color, val mineStroke: Color, val mineRule: Color, val mineText: Color, val mineMeta: Color,
    val codeFill: Color, val codeStroke: Color, val codeText: Color,
    val toastFill: Color, val onToast: Color,
    val heldText: Color, val heldPillFill: Color, val heldPillStroke: Color,
    val autoText: Color, val autoFill: Color, val autoStroke: Color,
    val blockedText: Color, val blockedFill: Color, val blockedStroke: Color,
    val heldCardFill: Color, val heldCardStroke: Color, val heldCardTitle: Color, val heldCardText: Color,
    val link: Color, val brand: Color, val agentRail: Color,
    val tickSent: Color, val tickSeen: Color, val tickWorking: Color,
    val tones: List<Color>, val toneInk: List<Color>,
)

/** The app's colour names over the kit's roles. Pure, so the mapping is testable on the JVM. */
internal fun palette(k: PPColors, dark: Boolean): PpColors {
    // The six hash tones in the web inbox's order (data-tone 1..6): navy, blue, teal, violet, amber, red.
    val faces = listOf(PPTone.Navy, PPTone.Blue, PPTone.Teal, PPTone.Violet, PPTone.Amber, PPTone.Red).map { ppToneColors(it, k, dark) }
    return PpColors(
        dark = dark,
        accent = k.accent, onAccent = k.onAccent, found = if (dark) PPPalette.violet400 else PPPalette.violet600,
        green = k.success, amber = k.warning,
        ink = k.text, body = k.textBody, secondary = k.textMuted, muted = k.textFaint, quiet = k.textBody,
        app = k.bg, sidebar = k.bgSubtle, column = k.bgSubtle, wash = k.bg, ground = k.surface, field = k.surfaceSunken,
        composer = k.surface, selected = k.surfaceSelected, chip = k.border, tile = k.surfaceHover,
        rule = k.borderSubtle, stroke = k.border, strokeStrong = k.borderStrong,
        mineFill = k.bubbleMine, mineStroke = k.bubbleMineBorder, mineRule = k.bubbleMineBorder, mineText = k.bubbleMineText,
        mineMeta = lerp(k.bubbleMineText, k.bubbleMine, 0.38f),
        codeFill = k.surfaceSunken, codeStroke = k.border, codeText = k.textBody,
        toastFill = k.surfaceInverse, onToast = k.onInverse,
        heldText = k.warning, heldPillFill = k.warningFill, heldPillStroke = k.warningBorder,
        autoText = k.success, autoFill = k.successFill, autoStroke = k.successBorder,
        blockedText = k.danger, blockedFill = k.dangerFill, blockedStroke = k.dangerBorder,
        heldCardFill = k.warningFill, heldCardStroke = k.warningBorder, heldCardTitle = k.warning,
        heldCardText = lerp(k.warning, k.textBody, 0.55f),
        link = k.textLink, brand = k.brand, agentRail = k.bubbleAgentRail,
        tickSent = k.tickSent, tickSeen = k.tickSeen, tickWorking = k.tickWorking,
        tones = faces.map { it.background }, toneInk = faces.map { it.foreground },
    )
}

private val LightTokens = palette(PPLight, false)
private val DarkTokens = palette(PPDark, true)

val LocalPp = staticCompositionLocalOf { LightTokens }

/** An avatar's disc and initials colours outside Compose (the call notification's caller picture). */
fun avatarTone(peer: String?, dark: Boolean): Pair<Color, Color> {
    val tokens = if (dark) DarkTokens else LightTokens
    val index = dev.pigeonpost.core.PeerFace.toneIndex(peer) - 1
    return tokens.tones[index] to tokens.toneInk[index]
}

/** The design tokens of the theme being drawn. */
object Pp {
    val colors: PpColors @Composable @ReadOnlyComposable get() = LocalPp.current
}

/** Geist for words, Geist Mono for addresses and code: the kit's bundled faces (SIL OFL 1.1). */
val Geist = GeistFamily
val GeistMono = GeistMonoFamily

fun geist(size: TextUnit, weight: FontWeight = FontWeight.Normal, color: Color = Color.Unspecified) =
    TextStyle(fontFamily = Geist, fontSize = size, fontWeight = weight, color = color)
fun geistMono(size: TextUnit, weight: FontWeight = FontWeight.Normal, color: Color = Color.Unspecified) =
    TextStyle(fontFamily = GeistMono, fontSize = size, fontWeight = weight, color = color)

/** The person's appearance choice; System follows the device. */
enum class AppearanceChoice(val label: String) {
    SYSTEM("System"), LIGHT("Light"), DARK("Dark");
    companion object { fun of(value: String?) = entries.firstOrNull { it.name.equals(value, true) } ?: SYSTEM }
}

@Composable
fun AppearanceChoice.isDark(): Boolean = when (this) {
    AppearanceChoice.SYSTEM -> isSystemInDarkTheme()
    AppearanceChoice.LIGHT -> false
    AppearanceChoice.DARK -> true
}

/**
 * The kit's PPTheme (colours, Geist type roles, the Material scheme mapped from the same roles),
 * with the app's names for those colours and the kit's radii as Material shapes.
 */
@Composable
fun PigeonpostTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit
) {
    val t = if (darkTheme) DarkTokens else LightTokens
    PPTheme(darkTheme) {
        val type = PP.type
        CompositionLocalProvider(LocalPp provides t) {
            MaterialTheme(
                colorScheme = MaterialTheme.colorScheme.copy(surfaceTint = Color.Transparent),
                // Kit roles; body copy stays at the kit's 15 (body) rather than 17 (body-l) for dense lists.
                typography = MaterialTheme.typography.copy(bodyLarge = type.body, bodyMedium = type.body.copy(fontSize = 14.sp, lineHeight = 20.sp)),
                shapes = Shapes(extraSmall = RoundedCornerShape(PPRadius.xs), small = RoundedCornerShape(PPRadius.s), medium = RoundedCornerShape(PPRadius.m),
                    large = RoundedCornerShape(PPRadius.l), extraLarge = RoundedCornerShape(PPRadius.xl)),
                content = content)
        }
    }
}
