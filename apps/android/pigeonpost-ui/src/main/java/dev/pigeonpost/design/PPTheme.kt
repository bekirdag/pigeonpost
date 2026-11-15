package dev.pigeonpost.design

import android.content.res.Configuration
import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/** Geist (SIL OFL 1.1), bundled as static faces in res/font. */
val GeistFamily = FontFamily(
    Font(R.font.geist_regular, FontWeight.Normal),
    Font(R.font.geist_italic, FontWeight.Normal, FontStyle.Italic),
    Font(R.font.geist_medium, FontWeight.Medium),
    Font(R.font.geist_semibold, FontWeight.SemiBold),
    Font(R.font.geist_bold, FontWeight.Bold),
)

/** Geist Mono, for addresses, handles, codes and amounts that must line up. */
val GeistMonoFamily = FontFamily(
    Font(R.font.geist_mono_regular, FontWeight.Normal),
    Font(R.font.geist_mono_medium, FontWeight.Medium),
    Font(R.font.geist_mono_semibold, FontWeight.SemiBold),
)

/** The kit's type roles (css .pp-display-xl … .pp-mono), sizes from PPType and line heights from PPLineHeight. */
@Immutable
data class PPTypography(
    val displayXl: TextStyle,
    val displayL: TextStyle,
    val displayM: TextStyle,
    val h1: TextStyle,
    val h2: TextStyle,
    val h3: TextStyle,
    val h4: TextStyle,
    val bodyL: TextStyle,
    val body: TextStyle,
    val bodyS: TextStyle,
    val caption: TextStyle,
    val overline: TextStyle,
    val mono: TextStyle,
)

private fun style(size: TextUnit, line: TextUnit, weight: FontWeight, family: FontFamily = GeistFamily, tracking: TextUnit = TextUnit.Unspecified) =
    TextStyle(
        fontFamily = family,
        fontSize = size,
        lineHeight = line,
        fontWeight = weight,
        letterSpacing = tracking,
        lineHeightStyle = LineHeightStyle(LineHeightStyle.Alignment.Center, LineHeightStyle.Trim.None),
    )

fun ppTypography(): PPTypography = PPTypography(
    displayXl = style(PPType.displayXl, PPLineHeight.displayXl, FontWeight.SemiBold, tracking = (-0.02).em),
    displayL = style(PPType.displayL, PPLineHeight.displayL, FontWeight.SemiBold, tracking = (-0.02).em),
    displayM = style(PPType.displayM, PPLineHeight.displayM, FontWeight.SemiBold, tracking = (-0.015).em),
    h1 = style(PPType.h1, PPLineHeight.h1, FontWeight.SemiBold, tracking = (-0.01).em),
    h2 = style(PPType.h2, PPLineHeight.h2, FontWeight.SemiBold),
    h3 = style(PPType.h3, PPLineHeight.h3, FontWeight.SemiBold),
    h4 = style(PPType.h4, PPLineHeight.h4, FontWeight.SemiBold),
    bodyL = style(PPType.bodyL, PPLineHeight.bodyL, FontWeight.Normal),
    body = style(PPType.body, PPLineHeight.body, FontWeight.Normal),
    bodyS = style(PPType.bodyS, PPLineHeight.bodyS, FontWeight.Normal),
    caption = style(PPType.caption, PPLineHeight.caption, FontWeight.Normal),
    overline = style(PPType.overline, PPLineHeight.overline, FontWeight.SemiBold, tracking = 0.06.em),
    mono = style(14.sp, 20.sp, FontWeight.Normal, family = GeistMonoFamily),
)

/**
 * Material 3 colour scheme mapped from the kit's semantic roles, so stock M3 widgets
 * (dialogs, menus, date pickers) sit comfortably next to PP components. Pure: testable on the JVM.
 */
fun ppColorScheme(c: PPColors, dark: Boolean): ColorScheme {
    val base = if (dark) darkColorScheme() else lightColorScheme()
    return base.copy(
        primary = c.accent,
        onPrimary = c.onAccent,
        primaryContainer = c.accentSubtle,
        onPrimaryContainer = c.textLink,
        secondary = c.brand,
        onSecondary = c.surface,
        secondaryContainer = c.surfaceSelected,
        onSecondaryContainer = c.text,
        tertiary = c.successSolid,
        onTertiary = c.onSolid,
        tertiaryContainer = c.successFill,
        onTertiaryContainer = c.success,
        error = c.dangerSolid,
        onError = c.onSolid,
        errorContainer = c.dangerFill,
        onErrorContainer = c.danger,
        background = c.bg,
        onBackground = c.text,
        surface = c.surface,
        onSurface = c.text,
        surfaceVariant = c.surfaceSunken,
        onSurfaceVariant = c.textMuted,
        surfaceTint = c.accent,
        inverseSurface = c.surfaceInverse,
        inverseOnSurface = c.onInverse,
        inversePrimary = c.textLink,
        outline = c.borderStrong,
        outlineVariant = c.border,
        scrim = if (dark) PPEffects.scrimDark else PPEffects.scrimLight,
        surfaceBright = c.surfaceRaised,
        surfaceDim = c.surfaceSunken,
        surfaceContainerLowest = c.surface,
        surfaceContainerLow = c.bgSubtle,
        surfaceContainer = c.surface,
        surfaceContainerHigh = c.surfaceRaised,
        surfaceContainerHighest = c.surfaceHover,
    )
}

/** Material typography mapped onto Geist roles. */
fun ppMaterialTypography(t: PPTypography): Typography = Typography(
    displayLarge = t.displayL, displayMedium = t.displayM, displaySmall = t.h1,
    headlineLarge = t.h1, headlineMedium = t.h2, headlineSmall = t.h3,
    titleLarge = t.h2, titleMedium = t.h4, titleSmall = t.h4.copy(fontSize = PPType.bodyS),
    bodyLarge = t.bodyL, bodyMedium = t.body, bodySmall = t.bodyS,
    labelLarge = t.body.copy(fontWeight = FontWeight.Medium, fontSize = 14.sp),
    labelMedium = t.caption.copy(fontWeight = FontWeight.Medium),
    labelSmall = t.overline,
)

val LocalPPColors = staticCompositionLocalOf { PPLight }
val LocalPPTypography = staticCompositionLocalOf { ppTypography() }
val LocalPPDark = staticCompositionLocalOf { false }

/** Accessors: `PP.colors.accent`, `PP.type.body`, `PP.isDark`. */
object PP {
    val colors: PPColors
        @Composable @ReadOnlyComposable get() = LocalPPColors.current
    val type: PPTypography
        @Composable @ReadOnlyComposable get() = LocalPPTypography.current
    val isDark: Boolean
        @Composable @ReadOnlyComposable get() = LocalPPDark.current
    val focusRing: Color
        @Composable @ReadOnlyComposable get() = if (LocalPPDark.current) PPEffects.focusRingDark else PPEffects.focusRingLight
}

/** Wrap an app (or a screen) once. Follows the system setting unless [darkTheme] is given. */
@Composable
fun PPTheme(darkTheme: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    val colors = if (darkTheme) PPDark else PPLight
    val type = ppTypography()
    CompositionLocalProvider(
        LocalPPColors provides colors,
        LocalPPTypography provides type,
        LocalPPDark provides darkTheme,
    ) {
        MaterialTheme(colorScheme = ppColorScheme(colors, darkTheme), typography = ppMaterialTypography(type), content = content)
    }
}

/** Light + dark previews in one annotation. */
@Preview(name = "Light", showBackground = true, widthDp = 390)
@Preview(name = "Dark", showBackground = true, widthDp = 390, uiMode = Configuration.UI_MODE_NIGHT_YES)
annotation class PPPreviews

/** Theme + page background + padding, for previews and catalogues. */
@Composable
fun PPPreviewSurface(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    PPTheme {
        Box(Modifier.background(PP.colors.bg)) {
            Column(modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp), content = content)
        }
    }
}

@PPPreviews
@Composable
private fun TypePreview() = PPPreviewSurface {
    val t = PP.type
    androidx.compose.material3.Text("Messages for people and agents", style = t.h1, color = PP.colors.text)
    androidx.compose.material3.Text("Ada Yılmaz replied to /wodo/hr at 14:05.", style = t.body, color = PP.colors.textBody)
    androidx.compose.material3.Text("/bekir/main", style = t.mono, color = PP.colors.textMuted)
    androidx.compose.material3.Text("INBOX", style = t.overline, color = PP.colors.textFaint)
}
