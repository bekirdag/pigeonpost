package dev.pigeonpost.design

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** css .pp-btn--* */
enum class PPButtonVariant { Primary, Secondary, Default, Outline, Ghost, Link, Danger, DangerOutline, Success }

/** css .pp-btn--s / default / --l: heights from PPControl (30 / 38 / 46). */
enum class PPButtonSize(val height: Dp, val paddingX: Dp, val fontSize: TextUnit, val radius: Dp, val icon: Dp) {
    S(PPControl.s, 10.dp, 13.sp, PPRadius.s, 16.dp),
    M(PPControl.m, 14.dp, 14.sp, PPRadius.m, 16.dp),
    L(PPControl.l, 20.dp, 15.sp, PPRadius.l, 18.dp),
}

internal data class Paint3(val bg: Color, val border: Color, val fg: Color)

@Composable
internal fun buttonPaint(variant: PPButtonVariant, pressed: Boolean): Paint3 {
    val c = PP.colors
    return when (variant) {
        PPButtonVariant.Primary -> if (pressed) Paint3(c.accentPressed, c.accentPressed, c.onAccent) else Paint3(c.accent, c.accent, c.onAccent)
        PPButtonVariant.Secondary -> Paint3(if (pressed) c.accentSubtleBorder.copy(alpha = 0.5f) else c.accentSubtle, c.accentSubtleBorder, c.textLink)
        PPButtonVariant.Default -> Paint3(if (pressed) c.surfaceHover else c.surface, c.borderStrong, c.text)
        PPButtonVariant.Outline -> Paint3(if (pressed) c.surfaceHover else Color.Transparent, c.borderStrong, c.text)
        PPButtonVariant.Ghost -> Paint3(if (pressed) c.surfaceHover else Color.Transparent, Color.Transparent, c.textBody)
        PPButtonVariant.Link -> Paint3(Color.Transparent, Color.Transparent, c.textLink)
        PPButtonVariant.Danger -> Paint3(c.dangerSolid, c.dangerSolid, c.onSolid)
        PPButtonVariant.DangerOutline -> Paint3(if (pressed) c.dangerFill else Color.Transparent, c.dangerBorder, c.danger)
        PPButtonVariant.Success -> Paint3(c.successSolid, c.successSolid, c.onSolid)
    }
}

/**
 * Text button (css .pp-btn). Verbs on buttons, sentence case. [loading] keeps the width, hides the
 * label behind a spinner and reports "Loading" to accessibility services.
 */
@Composable
fun PPButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    variant: PPButtonVariant = PPButtonVariant.Primary,
    size: PPButtonSize = PPButtonSize.M,
    leadingIcon: PPIcon? = null,
    trailingIcon: PPIcon? = null,
    loading: Boolean = false,
    enabled: Boolean = true,
    fullWidth: Boolean = false,
) {
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val p = buttonPaint(variant, pressed)
    val shape = RoundedCornerShape(size.radius)
    val isLink = variant == PPButtonVariant.Link
    val raised = variant in setOf(PPButtonVariant.Primary, PPButtonVariant.Default, PPButtonVariant.Danger, PPButtonVariant.Success)
    Box(
        modifier
            .then(if (fullWidth) Modifier.fillMaxWidth() else Modifier)
            .alpha(if (enabled) 1f else 0.5f)
            .then(if (raised && enabled && !pressed) Modifier.shadow(1.dp, shape, clip = false) else Modifier)
            .clip(shape)
            .background(p.bg)
            .then(if (isLink) Modifier else Modifier.border(1.dp, p.border, shape))
            .clickable(source, indication = null, enabled = enabled && !loading, role = Role.Button, onClick = onClick)
            .semantics { if (loading) stateDescription = "Loading" }
            .then(if (isLink) Modifier.padding(horizontal = 2.dp) else Modifier.defaultMinSize(minHeight = size.height).height(size.height).padding(horizontal = size.paddingX))
            .offset(y = if (pressed && !isLink) 1.dp else 0.dp),
        contentAlignment = Alignment.Center,
    ) {
        CompositionLocalProvider(LocalContentColor provides p.fg) {
            Row(
                Modifier.alpha(if (loading) 0f else 1f),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (leadingIcon != null) PPIconView(leadingIcon, size = size.icon)
                Text(
                    text,
                    style = PP.type.body.copy(fontSize = size.fontSize, lineHeight = size.fontSize, fontWeight = FontWeight.Medium),
                    color = p.fg,
                    maxLines = 1,
                    textDecoration = if (isLink && pressed) TextDecoration.Underline else null,
                )
                if (trailingIcon != null) PPIconView(trailingIcon, size = size.icon)
            }
            if (loading) PPSpinner(size = 16.dp, color = p.fg)
        }
    }
}

/** css .pp-icon-btn variants. */
enum class PPIconButtonVariant { Default, Ghost, Primary, Secondary, Danger }

/** css .pp-icon-btn sizes: S 30, M 38, L 46, Touch 44 (mobile bars). */
enum class PPIconButtonSize(val box: Dp, val icon: Dp, val radius: Dp) {
    S(PPControl.s, 16.dp, PPRadius.s), M(PPControl.m, 18.dp, PPRadius.m), L(PPControl.l, 20.dp, PPRadius.l), Touch(PPControl.touchMin, 22.dp, PPRadius.l)
}

/**
 * Square icon button (css .pp-icon-btn). [contentDescription] is required: it is the button's name.
 * [selected] maps to aria-pressed; [badge] shows an unread count (0 hides it, -1 shows a dot).
 */
@Composable
fun PPIconButton(
    icon: PPIcon,
    contentDescription: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    variant: PPIconButtonVariant = PPIconButtonVariant.Ghost,
    size: PPIconButtonSize = PPIconButtonSize.Touch,
    round: Boolean = false,
    selected: Boolean? = null,
    enabled: Boolean = true,
    badge: Int = 0,
) {
    val c = PP.colors
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val shape: Shape = if (round) CircleShape else RoundedCornerShape(size.radius)
    val p = when {
        selected == true -> Paint3(c.surfaceSelected, c.accentSubtleBorder, c.textLink)
        variant == PPIconButtonVariant.Default -> Paint3(if (pressed) c.surfaceHover else c.surface, c.borderStrong, c.textBody)
        variant == PPIconButtonVariant.Ghost -> Paint3(if (pressed) c.surfaceHover else Color.Transparent, Color.Transparent, if (pressed) c.text else c.textMuted)
        variant == PPIconButtonVariant.Primary -> Paint3(if (pressed) c.accentPressed else c.accent, c.accent, c.onAccent)
        variant == PPIconButtonVariant.Secondary -> Paint3(c.accentSubtle, c.accentSubtleBorder, c.textLink)
        else -> Paint3(c.dangerSolid, c.dangerSolid, c.onSolid)
    }
    Box(modifier.size(size.box)) {
        Box(
            Modifier
                .size(size.box)
                .alpha(if (enabled) 1f else 0.5f)
                .clip(shape)
                .background(p.bg)
                .border(1.dp, p.border, shape)
                .clickable(source, indication = null, enabled = enabled, role = Role.Button, onClick = onClick)
                .semantics {
                    this.contentDescription = contentDescription
                    if (selected != null) stateDescription = if (selected) "On" else "Off"
                },
            contentAlignment = Alignment.Center,
        ) { PPIconView(icon, size = size.icon, tint = p.fg) }
        if (badge != 0) {
            val dot = badge < 0
            Box(
                Modifier
                    .align(Alignment.TopEnd)
                    .offset(x = if (dot) (-2).dp else 4.dp, y = if (dot) 2.dp else (-4).dp)
                    .then(if (dot) Modifier.size(10.dp) else Modifier.height(18.dp).defaultMinSize(minWidth = 18.dp))
                    .clip(CircleShape)
                    .background(c.dangerSolid)
                    .border(2.dp, c.surface, CircleShape)
                    .padding(horizontal = if (dot) 0.dp else 5.dp),
                contentAlignment = Alignment.Center,
            ) {
                if (!dot) Text(if (badge > 99) "99+" else "$badge", style = PP.type.caption.copy(fontSize = 10.5.sp, lineHeight = 18.sp, fontWeight = FontWeight.SemiBold), color = c.onSolid)
            }
        }
    }
}

@PPPreviews
@androidx.compose.runtime.Composable
private fun ButtonsPreview() = PPPreviewSurface {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        PPButton("Send", {}, leadingIcon = PPIcon.SendHorizontal)
        PPButton("Reply", {}, variant = PPButtonVariant.Secondary)
        PPButton("Cancel", {}, variant = PPButtonVariant.Outline)
    }
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        PPButton("Block /spam", {}, variant = PPButtonVariant.Danger, size = PPButtonSize.S)
        PPButton("Approve", {}, variant = PPButtonVariant.Success, size = PPButtonSize.S)
        PPButton("Skip", {}, variant = PPButtonVariant.Ghost, size = PPButtonSize.S)
        PPButton("View terms", {}, variant = PPButtonVariant.Link)
    }
    PPButton("Top up $20.00", {}, size = PPButtonSize.L, fullWidth = true, loading = true)
    PPButton("Start a Nest", {}, size = PPButtonSize.L, fullWidth = true, leadingIcon = PPIcon.Bird, enabled = false)
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        PPIconButton(PPIcon.Search, "Search", {})
        PPIconButton(PPIcon.Bell, "Notifications", {}, variant = PPIconButtonVariant.Default, badge = 3)
        PPIconButton(PPIcon.Plus, "New message", {}, variant = PPIconButtonVariant.Primary, round = true)
        PPIconButton(PPIcon.Pin, "Pin", {}, selected = true)
        PPIconButton(PPIcon.Trash2, "Delete", {}, variant = PPIconButtonVariant.Danger, size = PPIconButtonSize.M)
    }
}
