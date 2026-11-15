package dev.pigeonpost.design

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/** Presence dot states (css .pp-presence[data-presence]). */
enum class PPPresence(val label: String) { Online("Online"), Away("Away"), Busy("Busy"), Offline("Offline") }

/** Presence dot with a 2dp ring in [ring] (the surface it sits on). */
@Composable
fun PPPresenceDot(presence: PPPresence, modifier: Modifier = Modifier, size: Dp = 10.dp, ring: Color = PP.colors.surface) {
    val c = PP.colors
    val fill = when (presence) {
        PPPresence.Online -> c.successSolid
        PPPresence.Away -> c.warningSolid
        PPPresence.Busy -> c.dangerSolid
        PPPresence.Offline -> c.surface
    }
    Box(
        modifier
            .size(size + 4.dp)
            .clip(CircleShape)
            .background(ring)
            .padding(2.dp)
            .clip(CircleShape)
            .background(fill)
            .then(if (presence == PPPresence.Offline) Modifier.border(2.dp, c.textFaint, CircleShape) else Modifier)
            .semantics { contentDescription = presence.label },
    )
}

/** css .pp-initials sizes. */
object PPAvatarSize {
    val xs = 20.dp; val s = 28.dp; val m = 36.dp; val l = 44.dp; val xl = 64.dp; val x2l = 96.dp
}

/**
 * Initials avatar (css .pp-initials). Initials and tone come from [PeerFace] so a peer keeps the
 * same face on every client; pass [tone] to override, [icon] for agents or Nests, [image] for a photo.
 * Decorative by default: the name beside it is what screen readers read.
 */
@Composable
fun PPAvatar(
    peer: String?,
    modifier: Modifier = Modifier,
    name: String? = null,
    size: Dp = PPAvatarSize.m,
    tone: PPTone? = null,
    icon: PPIcon? = null,
    presence: PPPresence? = null,
    square: Boolean = false,
    image: (@Composable () -> Unit)? = null,
) {
    val colors = ppToneColors(tone ?: PeerFace.tone(peer), PP.colors, PP.isDark)
    val shape = if (square) RoundedCornerShape(size * 0.28f) else CircleShape
    Box(modifier.size(size).clearAndSetSemantics { }) {
        Box(
            Modifier
                .size(size)
                .clip(shape)
                .background(colors.background)
                .then(if (colors.border != null) Modifier.border(1.dp, colors.border, shape) else Modifier),
            contentAlignment = Alignment.Center,
        ) {
            when {
                image != null -> image()
                icon != null -> PPIconView(icon, size = size * 0.52f, tint = colors.foreground)
                else -> Text(
                    PeerFace.initials(peer, name),
                    style = PP.type.body.copy(fontSize = (size.value * 0.36f).sp, lineHeight = (size.value * 0.36f).sp, fontWeight = FontWeight.SemiBold, letterSpacing = 0.02.em),
                    color = colors.foreground,
                )
            }
        }
        if (presence != null) {
            val big = size >= PPAvatarSize.l
            PPPresenceDot(
                presence,
                Modifier.align(Alignment.BottomEnd).offset(x = if (big) (-1).dp else 1.dp, y = if (big) (-1).dp else 1.dp),
                size = if (big) 12.dp else 10.dp,
            )
        }
    }
}

/** Badge tones (css .pp-badge--*). */
enum class PPBadgeTone { Neutral, Info, Accent, Success, Warning, Danger, Violet }

/** Small status label (css .pp-badge): 22dp pill, 12sp medium. [dot] adds a leading dot, [solid] fills it. */
@Composable
fun PPBadge(
    text: String,
    modifier: Modifier = Modifier,
    tone: PPBadgeTone = PPBadgeTone.Neutral,
    icon: PPIcon? = null,
    dot: Boolean = false,
    solid: Boolean = false,
    small: Boolean = false,
) {
    val c = PP.colors
    val dark = PP.isDark
    val (fg, bg, bd, dotC) = when (tone) {
        PPBadgeTone.Neutral -> listOf(c.textMuted, c.surfaceSunken, c.border, c.textFaint)
        PPBadgeTone.Info -> listOf(c.info, c.infoFill, c.infoBorder, c.infoSolid)
        PPBadgeTone.Accent -> listOf(c.textLink, c.accentSubtle, c.accentSubtleBorder, c.accent)
        PPBadgeTone.Success -> listOf(c.success, c.successFill, c.successBorder, c.successSolid)
        PPBadgeTone.Warning -> listOf(c.warning, c.warningFill, c.warningBorder, c.warningSolid)
        PPBadgeTone.Danger -> listOf(c.danger, c.dangerFill, c.dangerBorder, c.dangerSolid)
        PPBadgeTone.Violet -> if (dark) listOf(Color(0xFF927ECB), Color(0xFF27273D), Color(0xFF423D67), PPPalette.violet400)
        else listOf(Color(0xFF3D2978), Color(0xFFF1EEFD), Color(0xFFDDD6FB), PPPalette.violet400)
    }
    val solidBg = when (tone) {
        PPBadgeTone.Neutral -> PPPalette.gray600
        PPBadgeTone.Info, PPBadgeTone.Accent -> c.accent
        PPBadgeTone.Success -> PPPalette.green700
        PPBadgeTone.Warning -> PPPalette.amber600
        PPBadgeTone.Danger -> PPPalette.red600
        PPBadgeTone.Violet -> PPPalette.violet600
    }
    val f = if (solid) Color.White else fg
    Row(
        modifier
            .height(if (small) 18.dp else 22.dp)
            .clip(CircleShape)
            .background(if (solid) solidBg else bg)
            .then(if (solid) Modifier else Modifier.border(1.dp, bd, CircleShape))
            .padding(horizontal = if (small) 6.dp else 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(if (small) 4.dp else 6.dp),
    ) {
        if (dot) Box(Modifier.size(6.dp).clip(CircleShape).background(if (solid) Color.White else dotC))
        if (icon != null) PPIconView(icon, size = 13.dp, tint = f)
        Text(text, style = PP.type.caption.copy(fontSize = if (small) 11.sp else 12.sp, lineHeight = 12.sp, fontWeight = FontWeight.Medium), color = f, maxLines = 1)
    }
}

/** Tag / filter chip (css .pp-tag): 28dp, radius 8. [onRemove] adds an x button; [selected] for filters. */
@Composable
fun PPTag(
    text: String,
    modifier: Modifier = Modifier,
    icon: PPIcon? = null,
    selected: Boolean = false,
    onClick: (() -> Unit)? = null,
    onRemove: (() -> Unit)? = null,
) {
    val c = PP.colors
    val shape = RoundedCornerShape(PPRadius.s)
    Row(
        modifier
            .height(28.dp)
            .clip(shape)
            .background(if (selected) c.accentSubtle else c.surface)
            .border(1.dp, if (selected) c.accentSubtleBorder else c.borderStrong, shape)
            .then(if (onClick != null) Modifier.clickable(role = Role.Button, onClick = onClick) else Modifier)
            .padding(start = 10.dp, end = if (onRemove != null) 4.dp else 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (icon != null) PPIconView(icon, size = 14.dp, tint = if (selected) c.textLink else c.textFaint)
        Text(text, style = PP.type.bodyS.copy(fontSize = 13.sp, lineHeight = 13.sp, fontWeight = FontWeight.Medium), color = if (selected) c.textLink else c.textBody, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (onRemove != null) {
            Box(
                Modifier.size(20.dp).clip(CircleShape).clickable(role = Role.Button, onClick = onRemove).semantics { contentDescription = "Remove $text" },
                contentAlignment = Alignment.Center,
            ) { PPIconView(PPIcon.X, size = 13.dp, tint = c.textFaint) }
        }
    }
}

/** Card (css .pp-card): radius 14, 1dp border, surface. [onClick] makes it a single tappable target. */
@Composable
fun PPCard(
    modifier: Modifier = Modifier,
    padding: Dp = PPSpace.s5,
    raised: Boolean = false,
    onClick: (() -> Unit)? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    val c = PP.colors
    val shape = RoundedCornerShape(PPRadius.l)
    Column(
        modifier
            .then(if (raised) Modifier.shadow(4.dp, shape, ambientColor = Color(0x1412161C), spotColor = Color(0x1412161C)) else Modifier)
            .clip(shape)
            .background(c.surface)
            .border(1.dp, c.border, shape)
            .then(if (onClick != null) Modifier.clickable(role = Role.Button, onClick = onClick) else Modifier)
            .padding(padding),
        content = content,
    )
}

/** Leading icon tile tones (css .pp-list-item__icon--*). */
enum class PPIconTone { Neutral, Accent, Success, Warning, Danger }

@Composable
internal fun iconTile(tone: PPIconTone): Pair<Color, Color> {
    val c = PP.colors
    return when (tone) {
        PPIconTone.Neutral -> c.surfaceSunken to c.textMuted
        PPIconTone.Accent -> c.accentSubtle to c.textLink
        PPIconTone.Success -> c.successFill to c.success
        PPIconTone.Warning -> c.warningFill to c.warning
        PPIconTone.Danger -> c.dangerFill to c.danger
    }
}

/**
 * List row / settings cell (css .pp-list-item): 52dp min, 12/16 padding. Leading icon tile or a
 * custom [leading] slot, title + subtitle, optional [value] text, [trailing] slot and chevron.
 */
@Composable
fun PPListRow(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    icon: PPIcon? = null,
    iconTone: PPIconTone = PPIconTone.Neutral,
    leading: (@Composable () -> Unit)? = null,
    value: String? = null,
    trailing: (@Composable RowScope.() -> Unit)? = null,
    chevron: Boolean = false,
    destructive: Boolean = false,
    onClick: (() -> Unit)? = null,
) {
    val c = PP.colors
    Row(
        modifier
            .fillMaxWidth()
            .then(if (onClick != null) Modifier.clickable(role = Role.Button, onClick = onClick) else Modifier)
            .defaultMinSize(minHeight = 52.dp)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        when {
            leading != null -> leading()
            icon != null -> {
                val (bg, fg) = iconTile(if (destructive) PPIconTone.Danger else iconTone)
                Box(Modifier.size(36.dp).clip(RoundedCornerShape(PPRadius.m)).background(bg), contentAlignment = Alignment.Center) {
                    PPIconView(icon, size = 18.dp, tint = fg)
                }
            }
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, style = PP.type.body.copy(fontWeight = FontWeight.Medium), color = if (destructive) c.danger else c.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (subtitle != null) Text(subtitle, style = PP.type.bodyS, color = c.textMuted, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
        if (value != null) Text(value, style = PP.type.bodyS, color = c.textMuted, maxLines = 1)
        trailing?.invoke(this)
        if (chevron) PPIconView(PPIcon.ChevronRight, size = 18.dp, tint = c.textFaint)
    }
}

/** Grouped list section title (mobile settings): overline style, optional trailing action. */
@Composable
fun PPSectionHeader(title: String, modifier: Modifier = Modifier, action: String? = null, onAction: (() -> Unit)? = null) {
    val c = PP.colors
    Row(
        modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 20.dp, bottom = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title.uppercase(), Modifier.weight(1f).semantics { heading() }, style = PP.type.overline, color = c.textMuted)
        if (action != null && onAction != null) {
            Text(
                action,
                Modifier.clip(RoundedCornerShape(PPRadius.s)).clickable(role = Role.Button, onClick = onAction).padding(horizontal = 8.dp, vertical = 6.dp),
                style = PP.type.bodyS.copy(fontWeight = FontWeight.SemiBold),
                color = c.textLink,
            )
        }
    }
}

/** Empty state (css .pp-empty): icon tile 48, title h3, text, actions, hint. */
@Composable
fun PPEmptyState(
    title: String,
    modifier: Modifier = Modifier,
    icon: PPIcon = PPIcon.Inbox,
    text: String? = null,
    hint: String? = null,
    compact: Boolean = false,
    actions: (@Composable RowScope.() -> Unit)? = null,
) {
    val c = PP.colors
    Column(
        modifier.fillMaxWidth().padding(horizontal = if (compact) 16.dp else 24.dp, vertical = if (compact) 32.dp else 48.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(if (compact) 8.dp else 12.dp),
    ) {
        Box(
            Modifier.size(48.dp).clip(RoundedCornerShape(PPRadius.l)).background(c.surfaceSunken).border(1.dp, c.border, RoundedCornerShape(PPRadius.l)),
            contentAlignment = Alignment.Center,
        ) { PPIconView(icon, size = 22.dp, tint = c.textMuted) }
        Text(title, Modifier.widthIn(max = 440.dp).semantics { heading() }, style = PP.type.h3, color = c.text, textAlign = TextAlign.Center)
        if (text != null) Text(text, Modifier.widthIn(max = 420.dp), style = PP.type.body, color = c.textMuted, textAlign = TextAlign.Center)
        if (actions != null) Row(Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp), content = actions)
        if (hint != null) Text(hint, Modifier.padding(top = 4.dp), style = PP.type.caption, color = c.textFaint, textAlign = TextAlign.Center)
    }
}

@PPPreviews
@Composable
private fun DisplayPreview() = PPPreviewSurface {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        PPAvatar("/ada", name = "Ada Yılmaz", size = PPAvatarSize.l, presence = PPPresence.Online)
        PPAvatar("/wodo/hr", size = PPAvatarSize.l)
        PPAvatar("/emre", name = "Emre Dağ")
        PPAvatar("/ozge", name = "Özge Arıç", presence = PPPresence.Away)
        PPAvatar("/marina", name = "Marina Petrova", size = PPAvatarSize.s)
        PPAvatar("/bekir/agent1", icon = PPIcon.Bot, tone = PPTone.Green)
        PPAvatar("/k/9mjp", tone = PPTone.Gray, size = PPAvatarSize.s, presence = PPPresence.Offline)
    }
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        PPBadge("Agent", tone = PPBadgeTone.Success, icon = PPIcon.Bot)
        PPBadge("Held", tone = PPBadgeTone.Warning, dot = true)
        PPBadge("Live", tone = PPBadgeTone.Danger, dot = true, solid = true)
        PPBadge("Beta", tone = PPBadgeTone.Violet)
    }
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        PPTag("Unread", selected = true, onClick = {})
        PPTag("Agents", icon = PPIcon.Bot, onClick = {})
        PPTag("/wodo/hr", onRemove = {})
    }
    PPCard {
        Text("Weekly digest", style = PP.type.h4, color = PP.colors.text)
        Text("12 new messages from 4 people and 2 agents.", style = PP.type.bodyS, color = PP.colors.textMuted)
    }
    Column(Modifier.clip(RoundedCornerShape(PPRadius.l)).background(PP.colors.surface)) {
        PPSectionHeader("Account", action = "Edit", onAction = {})
        PPListRow("Addresses", subtitle = "/bekir, /bekir/agent1", icon = PPIcon.AtSign, iconTone = PPIconTone.Accent, chevron = true, onClick = {})
        HorizontalDivider(color = PP.colors.borderSubtle)
        PPListRow("Credit balance", icon = PPIcon.Wallet, iconTone = PPIconTone.Success, value = "$24.50", chevron = true, onClick = {})
        HorizontalDivider(color = PP.colors.borderSubtle)
        PPListRow("Sign out", icon = PPIcon.LogOut, destructive = true, onClick = {})
    }
    PPEmptyState(
        "No messages yet",
        text = "Share your address, /ada, and messages from people and agents land here.",
        hint = "Tip: agents can write to you too.",
        compact = true,
    ) { PPButton("Copy address", {}, leadingIcon = PPIcon.Copy, variant = PPButtonVariant.Secondary) }
}
