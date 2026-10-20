package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.Shapes
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pigeonpost.inbox.R

// The Pigeonpost conversation design, token for token with the iOS app's Theme.swift.
// Dark is the design's own values exactly; light is the same language re-derived for paper.

private fun c(hex: Long) = Color(0xFF000000 or hex)

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
    val toastFill: Color,
    val heldText: Color, val heldPillFill: Color, val heldPillStroke: Color,
    val autoText: Color, val autoFill: Color, val autoStroke: Color,
    val blockedText: Color, val blockedFill: Color, val blockedStroke: Color,
    val heldCardFill: Color, val heldCardStroke: Color, val heldCardTitle: Color, val heldCardText: Color,
    val tones: List<Color>, val toneInk: List<Color>,
)

private fun palette(dark: Boolean): PpColors {
    fun a(light: Long, darkValue: Long) = c(if (dark) darkValue else light)
    return PpColors(
        dark = dark,
        accent = a(0x2F6BE0, 0x8AB4FF), onAccent = a(0xFFFFFF, 0x0D0F12), found = a(0x7C3AED, 0xA78BFA),
        green = a(0x1E8A50, 0x7DD3A0), amber = a(0xA35F00, 0xF0B860),
        ink = a(0x14181F, 0xE8EAED), body = a(0x2E353F, 0xD7DCE3), secondary = a(0x5B6470, 0x9AA3AE),
        muted = a(0x737C88, 0x7D8590), quiet = a(0x3E4651, 0xC3C9D1),
        app = a(0xF3F4F6, 0x0D0F12), sidebar = a(0xF7F8FA, 0x111419), column = a(0xF4F5F8, 0x13161B),
        wash = a(0xFAFBFC, 0x0F1216), ground = a(0xFFFFFF, 0x171B21), field = a(0xECEEF2, 0x171B21),
        composer = a(0xFFFFFF, 0x151920), selected = a(0xE6EAF0, 0x1C2129), chip = a(0xDFE3EA, 0x252B34),
        tile = a(0xE9ECF1, 0x22272F),
        rule = a(0xE3E6EB, 0x1F242B), stroke = a(0xDFE3E8, 0x262B33), strokeStrong = a(0xD3D8DF, 0x2A3039),
        mineFill = a(0xEDF3FF, 0x131C2E), mineStroke = a(0xC9D8F6, 0x263556), mineRule = a(0xD9E4F8, 0x1F2C47),
        mineText = a(0x14213D, 0xE6ECF8), mineMeta = a(0x5B6B8E, 0x8C9AB8),
        codeFill = a(0xF0F2F5, 0x1E232B), codeStroke = a(0xDDE1E7, 0x2A3039), codeText = a(0x2E353F, 0xD7DCE3),
        toastFill = a(0x14181F, 0x2A3039),
        heldText = a(0xA35F00, 0xF0B860), heldPillFill = a(0xFCEFD3, 0x2A2213), heldPillStroke = a(0xF4DDAE, 0x45361A),
        autoText = a(0x1E7A47, 0x7DD3A0), autoFill = a(0xE2F4E9, 0x142A1F), autoStroke = a(0xC4E8D2, 0x1F4030),
        blockedText = a(0xB0284B, 0xF29AAE), blockedFill = a(0xFDE8EE, 0x3A1A22), blockedStroke = a(0xF6C9D5, 0x5A2633),
        heldCardFill = a(0xFFF8EA, 0x1C1810), heldCardStroke = a(0xF1D9A6, 0x45361A),
        heldCardTitle = a(0x7A4A06, 0xF6D9A6), heldCardText = a(0x7D6334, 0xC9B48F),
        tones = listOf(a(0xE4E9F2, 0x232A3A), a(0xE0EBFD, 0x17263F), a(0xDAF2EC, 0x12302D),
            a(0xECE6FD, 0x2A2142), a(0xFBEDD3, 0x33280F), a(0xFCE4EA, 0x3A1A22)),
        toneInk = listOf(a(0x3A4A6B, 0xA9B8D6), a(0x2457B8, 0x8DB4F5), a(0x12756A, 0x6FD6C4),
            a(0x6340C0, 0xB9A2FF), a(0x985A06, 0xF0B860), a(0xAE2A4B, 0xF29AAE)),
    )
}

private val LightTokens = palette(false)
private val DarkTokens = palette(true)

val LocalPp = staticCompositionLocalOf { DarkTokens }

/** The design tokens of the theme being drawn. */
object Pp {
    val colors: PpColors @Composable @ReadOnlyComposable get() = LocalPp.current
}

/** Geist for words, Geist Mono for addresses and code (SIL OFL 1.1, assets/Geist-OFL.txt). */
val Geist = FontFamily(
    Font(R.font.geist_regular, FontWeight.Normal),
    Font(R.font.geist_italic, FontWeight.Normal, FontStyle.Italic),
    Font(R.font.geist_medium, FontWeight.Medium),
    Font(R.font.geist_semi_bold, FontWeight.SemiBold),
    Font(R.font.geist_bold, FontWeight.Bold),
)
val GeistMono = FontFamily(
    Font(R.font.geist_mono_regular, FontWeight.Normal),
    Font(R.font.geist_mono_medium, FontWeight.Medium),
    Font(R.font.geist_mono_semi_bold, FontWeight.SemiBold),
    Font(R.font.geist_mono_semi_bold, FontWeight.Bold),
)

fun geist(size: TextUnit, weight: FontWeight = FontWeight.Normal, color: Color = Color.Unspecified) =
    TextStyle(fontFamily = Geist, fontSize = size, fontWeight = weight, color = color)
fun geistMono(size: TextUnit, weight: FontWeight = FontWeight.Normal, color: Color = Color.Unspecified) =
    TextStyle(fontFamily = GeistMono, fontSize = size, fontWeight = weight, color = color)

private val PpTypography = Typography().let { base ->
    fun TextStyle.g() = copy(fontFamily = Geist)
    Typography(
        displayLarge = base.displayLarge.g(), displayMedium = base.displayMedium.g(), displaySmall = base.displaySmall.g(),
        headlineLarge = base.headlineLarge.g().copy(fontWeight = FontWeight.Bold),
        headlineMedium = base.headlineMedium.g().copy(fontWeight = FontWeight.SemiBold),
        headlineSmall = base.headlineSmall.g().copy(fontWeight = FontWeight.SemiBold, fontSize = 22.sp),
        titleLarge = base.titleLarge.g().copy(fontWeight = FontWeight.SemiBold), titleMedium = base.titleMedium.g().copy(fontWeight = FontWeight.SemiBold),
        titleSmall = base.titleSmall.g(),
        bodyLarge = base.bodyLarge.g().copy(fontSize = 15.sp, lineHeight = 22.sp, letterSpacing = 0.sp),
        bodyMedium = base.bodyMedium.g().copy(fontSize = 14.sp, lineHeight = 20.sp, letterSpacing = 0.sp),
        bodySmall = base.bodySmall.g(), labelLarge = base.labelLarge.g().copy(fontWeight = FontWeight.Medium),
        labelMedium = base.labelMedium.g(), labelSmall = base.labelSmall.g().copy(letterSpacing = 0.sp),
    )
}

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

@Composable
fun PigeonpostTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit
) {
    val t = if (darkTheme) DarkTokens else LightTokens
    val scheme = (if (darkTheme) darkColorScheme() else lightColorScheme()).copy(
        primary = t.accent, onPrimary = t.onAccent, primaryContainer = t.mineFill, onPrimaryContainer = t.mineText,
        inversePrimary = t.accent,
        secondary = t.green, onSecondary = t.onAccent, secondaryContainer = t.chip, onSecondaryContainer = t.ink,
        tertiary = t.amber, onTertiary = t.onAccent, tertiaryContainer = t.heldCardFill, onTertiaryContainer = t.heldCardTitle,
        background = t.app, onBackground = t.ink,
        surface = t.ground, onSurface = t.ink, surfaceVariant = t.field, onSurfaceVariant = t.secondary,
        surfaceTint = Color.Transparent, inverseSurface = t.toastFill, inverseOnSurface = Color.White,
        error = t.blockedText, onError = t.onAccent, errorContainer = t.blockedFill, onErrorContainer = t.blockedText,
        outline = t.strokeStrong, outlineVariant = t.rule, scrim = Color.Black,
        surfaceBright = t.ground, surfaceDim = t.wash,
        surfaceContainerLowest = t.wash, surfaceContainerLow = t.sidebar, surfaceContainer = t.ground,
        surfaceContainerHigh = t.ground, surfaceContainerHighest = t.field,
    )
    CompositionLocalProvider(LocalPp provides t) {
        MaterialTheme(colorScheme = scheme, typography = PpTypography,
            shapes = Shapes(extraSmall = RoundedCornerShape(6.dp), small = RoundedCornerShape(8.dp), medium = RoundedCornerShape(10.dp),
                large = RoundedCornerShape(14.dp), extraLarge = RoundedCornerShape(18.dp)),
            content = content)
    }
}
