package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

val Held = Color(0xFF996000)
val Allowed = Color(0xFF16704A)

private val LightColorScheme = lightColorScheme(
    primary = Color(0xFF16326B),
    secondary = Allowed,
    tertiary = Held,
    tertiaryContainer = Color(0xFFFFF1D8),
    onTertiaryContainer = Color(0xFF4C3100),
    onPrimary = Color(0xFFFFFFFF),
    background = Color(0xFFFFFFFF),
    surface = Color(0xFFFFFFFF),
    onSurface = Color(0xFF14181F),
    surfaceVariant = Color(0xFFF7F9FB),
    onSurfaceVariant = Color(0xFF6B7480),
    outline = Color(0xFFE6E9EE)
)

private val DarkColorScheme = darkColorScheme(
    primary = Color(0xFF8FAEFF),
    secondary = Color(0xFF9ED7B4),
    tertiary = Color(0xFFF4BF60),
    tertiaryContainer = Color(0xFF4C3100),
    onTertiaryContainer = Color(0xFFFFE0A0),
    onPrimary = Color(0xFF10234D),
    background = Color(0xFF0B0D11),
    surface = Color(0xFF16191F),
    onSurface = Color(0xFFF4F6F9),
    surfaceVariant = Color(0xFF222732),
    onSurfaceVariant = Color(0xFFC6CCD6),
    outline = Color(0xFF414957)
)

@Composable
fun PigeonpostTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit
) {
    val colorScheme = if (darkTheme) DarkColorScheme else LightColorScheme

    MaterialTheme(
        colorScheme = colorScheme,
        content = content
    )
}
