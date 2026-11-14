package dev.pigeonpost.design

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
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
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.error
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Focus ring + border for inputs (css :focus-visible → 0 0 0 3px focus-ring). */
@Composable
private fun Modifier.fieldFrame(focused: Boolean, invalid: Boolean, radius: Dp, enabled: Boolean): Modifier {
    val c = PP.colors
    val shape = RoundedCornerShape(radius)
    val border by animateColorAsState(
        when { invalid -> c.danger; focused -> c.accent; else -> c.borderStrong }, tween(PPDuration.fast), label = "border",
    )
    return this
        .then(if (focused) Modifier.border(3.dp, PP.focusRing, RoundedCornerShape(radius + 3.dp)).padding(3.dp) else Modifier.padding(3.dp))
        .clip(shape)
        .background(if (enabled) c.surface else c.surfaceSunken)
        .border(1.dp, border, shape)
}

/**
 * Text field with label, hint and error (css .pp-field + .pp-input). The error replaces the hint
 * and is announced with the field. Height follows PPControl (46 by default on mobile).
 */
@Composable
fun PPTextField(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    label: String? = null,
    placeholder: String? = null,
    hint: String? = null,
    error: String? = null,
    leadingIcon: PPIcon? = null,
    trailing: (@Composable () -> Unit)? = null,
    required: Boolean = false,
    enabled: Boolean = true,
    mono: Boolean = false,
    height: Dp = PPControl.l,
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
    visualTransformation: VisualTransformation = VisualTransformation.None,
) {
    val c = PP.colors
    val source = remember { MutableInteractionSource() }
    val focused by source.collectIsFocusedAsState()
    Column(modifier, verticalArrangement = Arrangement.spacedBy(3.dp)) {
        if (label != null) {
            Row(Modifier.padding(horizontal = 3.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(label, style = PP.type.body.copy(fontSize = 14.sp, lineHeight = 20.sp, fontWeight = FontWeight.Medium), color = c.text)
                if (required) Text("*", style = PP.type.body.copy(fontSize = 14.sp, fontWeight = FontWeight.SemiBold), color = c.danger)
            }
        }
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            enabled = enabled,
            singleLine = true,
            interactionSource = source,
            keyboardOptions = keyboardOptions,
            visualTransformation = visualTransformation,
            cursorBrush = SolidColor(c.accent),
            textStyle = (if (mono) PP.type.mono.copy(fontSize = 15.sp) else PP.type.body).copy(color = if (enabled) c.text else c.textFaint),
            modifier = Modifier
                .fillMaxWidth()
                .semantics {
                    if (label != null) contentDescription = label
                    if (error != null) error(error)
                },
            decorationBox = { inner ->
                Row(
                    Modifier.fillMaxWidth().fieldFrame(focused, error != null, PPRadius.m, enabled).height(height).padding(horizontal = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    if (leadingIcon != null) PPIconView(leadingIcon, size = 18.dp, tint = c.textFaint)
                    Box(Modifier.weight(1f)) {
                        if (value.isEmpty() && placeholder != null) Text(placeholder, style = PP.type.body, color = c.textFaint, maxLines = 1)
                        inner()
                    }
                    trailing?.invoke()
                }
            },
        )
        val note = error ?: hint
        if (note != null) {
            Row(Modifier.padding(horizontal = 3.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                if (error != null) PPIconView(PPIcon.AlertCircle, Modifier.padding(top = 1.5.dp), size = 15.dp, tint = c.danger)
                Text(note, style = PP.type.bodyS.copy(fontSize = 13.sp, lineHeight = 18.sp), color = if (error != null) c.danger else c.textMuted)
            }
        }
    }
}

/**
 * One-time code (css .pp-otp): [length] cells, Geist Mono 22, split into groups of [groupSize]
 * with a dash. One hidden text field drives the cells so paste and SMS autofill work.
 */
@Composable
fun PPOTPField(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    length: Int = 6,
    groupSize: Int? = 3,
    isError: Boolean = false,
    enabled: Boolean = true,
    label: String = "Verification code",
) {
    val c = PP.colors
    val source = remember { MutableInteractionSource() }
    val focused by source.collectIsFocusedAsState()
    BasicTextField(
        value = value,
        onValueChange = { v -> onValueChange(v.filter { it.isDigit() }.take(length)) },
        enabled = enabled,
        singleLine = true,
        interactionSource = source,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword),
        cursorBrush = SolidColor(Color.Transparent),
        textStyle = PP.type.mono.copy(color = Color.Transparent),
        modifier = modifier.semantics {
            contentDescription = label
            if (isError) error("Code is not correct")
        },
        decorationBox = { inner ->
            Box {
                Box(Modifier.size(1.dp).alpha(0f)) { inner() }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    for (i in 0 until length) {
                        if (groupSize != null && i > 0 && i % groupSize == 0) {
                            Box(Modifier.width(10.dp).height(2.dp).clip(RoundedCornerShape(1.dp)).background(c.borderStrong))
                        }
                        val ch = value.getOrNull(i)?.toString() ?: ""
                        val active = focused && (i == value.length || (i == length - 1 && value.length == length))
                        val cellShape = RoundedCornerShape(PPRadius.m)
                        Box(
                            Modifier
                                .then(if (active && !isError) Modifier.border(3.dp, PP.focusRing, RoundedCornerShape(PPRadius.m + 3.dp)).padding(3.dp) else Modifier.padding(3.dp))
                                .size(width = 40.dp, height = 48.dp)
                                .clip(cellShape)
                                .background(
                                    when {
                                        !enabled -> c.surfaceSunken
                                        isError -> lerp(c.surface, c.dangerFill, 0.6f)
                                        else -> c.surface
                                    },
                                )
                                .border(
                                    1.dp,
                                    when {
                                        isError -> c.danger
                                        active -> c.accent
                                        ch.isNotEmpty() -> lerp(c.borderStrong, c.textFaint, 0.5f)
                                        else -> c.borderStrong
                                    },
                                    cellShape,
                                ),
                            contentAlignment = Alignment.Center,
                        ) {
                            Text(
                                ch,
                                style = PP.type.mono.copy(fontSize = 22.sp, lineHeight = 22.sp, fontWeight = FontWeight.SemiBold),
                                color = if (enabled) c.text else c.textFaint,
                                textAlign = TextAlign.Center,
                            )
                        }
                    }
                }
            }
        },
    )
}

/** The kit switch (css .pp-switch): 36 x 22 track, 18 thumb. Use inside [PPToggleRow] for a labelled row. */
@Composable
fun PPSwitch(checked: Boolean, modifier: Modifier = Modifier, enabled: Boolean = true) {
    val c = PP.colors
    val track by animateColorAsState(if (checked) c.accent else c.borderStrong, tween(PPDuration.base), label = "track")
    val x by animateDpAsState(if (checked) 14.dp else 0.dp, tween(PPDuration.base), label = "thumb")
    Box(
        modifier.alpha(if (enabled) 1f else 0.5f).size(36.dp, 22.dp).clip(CircleShape).background(track).padding(2.dp),
    ) {
        Box(Modifier.offset(x = x).size(18.dp).shadow(2.dp, CircleShape).clip(CircleShape).background(c.controlThumb))
    }
}

/** Settings row with a switch (css .pp-list-item + .pp-switch). The whole row toggles; 52dp min height. */
@Composable
fun PPToggleRow(
    title: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    icon: PPIcon? = null,
    enabled: Boolean = true,
) {
    val c = PP.colors
    Row(
        modifier
            .fillMaxWidth()
            .toggleable(checked, enabled = enabled, role = Role.Switch, onValueChange = onCheckedChange)
            .defaultMinSize(minHeight = 52.dp)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        if (icon != null) {
            Box(Modifier.size(36.dp).clip(RoundedCornerShape(PPRadius.m)).background(c.surfaceSunken), contentAlignment = Alignment.Center) {
                PPIconView(icon, size = 18.dp, tint = c.textMuted)
            }
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = PP.type.body.copy(fontWeight = FontWeight.Medium), color = if (enabled) c.text else c.textFaint)
            if (subtitle != null) Text(subtitle, style = PP.type.bodyS, color = c.textMuted)
        }
        PPSwitch(checked, enabled = enabled)
    }
}

@PPPreviews
@Composable
private fun InputsPreview() = PPPreviewSurface {
    var handle by remember { mutableStateOf("/ada") }
    PPTextField(handle, { handle = it }, label = "Handle", hint = "Your address on Pigeonpost, like /ada or /wodo/hr.", leadingIcon = PPIcon.AtSign, mono = true, required = true)
    PPTextField("ada@", {}, label = "Email", error = "Enter a full email address.")
    PPTextField("", {}, label = "Search", placeholder = "Search messages", leadingIcon = PPIcon.Search)
    PPOTPField("4821", {})
    PPOTPField("482190", {}, isError = true)
    Column(Modifier.widthIn(max = 420.dp).clip(RoundedCornerShape(PPRadius.l)).background(PP.colors.surface)) {
        var a by remember { mutableStateOf(true) }
        PPToggleRow("Message previews", a, { a = it }, subtitle = "Show the first line in notifications", icon = PPIcon.Bell)
        PPToggleRow("Let agents read receipts", false, {}, icon = PPIcon.Bot)
        PPToggleRow("Calls from unknown senders", false, {}, enabled = false, icon = PPIcon.Phone)
    }
    Spacer(Modifier.height(4.dp))
}
