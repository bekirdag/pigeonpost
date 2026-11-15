package dev.pigeonpost.design

import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.StartOffset
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.keyframes
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
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
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

// ---- Ticks -------------------------------------------------------------------------------------

/** Delivery state of a message (css .pp-tick[data-state]). */
enum class PPTickState(val label: String) {
    Sending("Sending"), Sent("Sent"), Delivered("Delivered"), Seen("Seen"), Working("Agent is working"), Failed("Not sent")
}

/** Delivery ticks: clock, check, check-check (grey or green), a turning gear for agents at work, alert on failure. */
@Composable
fun PPTicks(state: PPTickState, modifier: Modifier = Modifier, tint: Color? = null) {
    val c = PP.colors
    val color = tint ?: when (state) {
        PPTickState.Seen -> c.tickSeen
        PPTickState.Working -> c.tickWorking
        PPTickState.Failed -> c.danger
        else -> c.tickSent
    }
    val (icon, size) = when (state) {
        PPTickState.Sending -> PPIcon.Clock to 13.dp
        PPTickState.Sent -> PPIcon.Check to 16.dp
        PPTickState.Delivered, PPTickState.Seen -> PPIcon.CheckCheck to 16.dp
        PPTickState.Working -> PPIcon.Settings to 14.dp
        PPTickState.Failed -> PPIcon.AlertCircle to 15.dp
    }
    val angle = if (state == PPTickState.Working) {
        val t = rememberInfiniteTransition(label = "tick")
        t.animateFloat(0f, 360f, infiniteRepeatable(tween(2400, easing = LinearEasing)), label = "turn").value
    } else 0f
    Box(modifier.size(16.dp).semantics { contentDescription = state.label }, contentAlignment = Alignment.Center) {
        PPIconView(icon, Modifier.rotate(angle), size = size, tint = color)
    }
}

// ---- Bubble ------------------------------------------------------------------------------------

/** Who wrote the message. Agent is a reply from an AI agent: a theirs-bubble with the green rail. */
enum class PPBubbleRole { Mine, Theirs, Agent }

/** Position in a run of messages from one sender (css data-pos); corners toward the sender tighten. */
enum class PPBubblePosition { Single, First, Middle, Last }

/** The bubble shape: radius 18 with a 6 corner at the tail (and on the joined side of a run). */
fun ppBubbleShape(mine: Boolean, position: PPBubblePosition): RoundedCornerShape {
    val r = PPRadius.bubble
    val t = PPRadius.bubbletail
    val joinedTop = position == PPBubblePosition.Middle || position == PPBubblePosition.Last
    return if (mine) RoundedCornerShape(topStart = r, topEnd = if (joinedTop) t else r, bottomEnd = t, bottomStart = r)
    else RoundedCornerShape(topStart = if (joinedTop) t else r, topEnd = r, bottomEnd = r, bottomStart = t)
}

/** The lowercase "agent" pill (css .pp-agent-badge). */
@Composable
fun PPAgentBadge(modifier: Modifier = Modifier) {
    val c = PP.colors
    Row(
        modifier.height(18.dp).clip(CircleShape).background(c.successFill).border(1.dp, c.successBorder, CircleShape).padding(horizontal = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        PPIconView(PPIcon.Bot, size = 11.dp, tint = c.success)
        Text("agent", style = PP.type.caption.copy(fontSize = 10.5.sp, lineHeight = 10.5.sp, fontWeight = FontWeight.SemiBold, letterSpacing = 0.04.em), color = c.success)
    }
}

/**
 * A message (css .pp-msg + .pp-bubble). Theirs and agent bubbles sit left with a 28dp avatar column
 * (the face shows on the last bubble of a run); mine sit right. Time, "edited" and ticks float at the
 * end. Max width 78% of the row. The whole bubble reads as one sentence to screen readers.
 */
@Composable
fun PPMessageBubble(
    text: String,
    time: String,
    role: PPBubbleRole,
    modifier: Modifier = Modifier,
    position: PPBubblePosition = PPBubblePosition.Single,
    sender: String? = null,
    senderPeer: String? = null,
    ticks: PPTickState? = null,
    edited: Boolean = false,
    showAvatar: Boolean = true,
    onLongPress: (() -> Unit)? = null,
) {
    val c = PP.colors
    val mine = role == PPBubbleRole.Mine
    val agent = role == PPBubbleRole.Agent
    val shape = ppBubbleShape(mine, position)
    val textColor = if (mine) c.bubbleMineText else c.bubbleTheirsText
    val metaColor = if (mine) c.bubbleMineText.copy(alpha = 0.62f) else c.textFaint
    val spoken = buildString {
        append(if (mine) "You" else sender ?: senderPeer?.let { PeerFace.displayName(it) } ?: "Them")
        if (agent) append(", agent")
        append(": ").append(text).append(". ").append(time)
        if (edited) append(", edited")
        if (ticks != null) append(", ").append(ticks.label)
    }
    BoxWithConstraints(modifier.fillMaxWidth()) {
        val maxBubble = maxWidth * 0.78f
        Row(
            Modifier.fillMaxWidth().padding(top = if (position == PPBubblePosition.Middle || position == PPBubblePosition.Last) 2.dp else 0.dp),
            horizontalArrangement = if (mine) Arrangement.End else Arrangement.Start,
            verticalAlignment = Alignment.Bottom,
        ) {
            if (!mine && showAvatar) {
                Box(Modifier.width(28.dp)) {
                    if (position == PPBubblePosition.Single || position == PPBubblePosition.Last) {
                        PPAvatar(senderPeer, name = sender, size = 28.dp, icon = if (agent) PPIcon.Bot else null, tone = if (agent) PPTone.Green else null)
                    }
                }
                Spacer(Modifier.width(8.dp))
            }
            Box(
                Modifier
                    .widthIn(max = maxBubble)
                    .then(if (!mine) Modifier.shadow(1.dp, shape, ambientColor = Color(0x0F12161C), spotColor = Color(0x0F12161C)) else Modifier)
                    .clip(shape)
                    .background(if (mine) c.bubbleMine else c.bubbleTheirs)
                    .border(1.dp, if (mine) c.bubbleMineBorder else c.bubbleTheirsBorder, shape)
                    .then(if (onLongPress != null) Modifier.clickable(onClickLabel = "Message actions", onClick = onLongPress) else Modifier)
                    .clearAndSetSemantics { contentDescription = spoken }
                    .padding(start = if (agent) 18.dp else 12.dp, end = 12.dp, top = 8.dp, bottom = 8.dp),
            ) {
                Column {
                    if (agent || (!mine && sender != null && (position == PPBubblePosition.Single || position == PPBubblePosition.First))) {
                        Row(
                            Modifier.padding(bottom = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            if (agent) PPIconView(PPIcon.Bot, size = 14.dp, tint = c.success)
                            Text(sender ?: PeerFace.displayName(senderPeer), style = PP.type.caption.copy(fontWeight = FontWeight.SemiBold), color = c.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            if (agent) PPAgentBadge()
                        }
                    }
                    Text(text, style = PP.type.body, color = textColor)
                    Row(
                        Modifier.align(Alignment.End).padding(top = 4.dp).offset(x = 4.dp, y = 2.dp).height(16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        if (edited) Text("edited", style = PP.type.caption.copy(fontSize = 11.5.sp, lineHeight = 16.sp, fontStyle = FontStyle.Italic), color = metaColor)
                        Text(time, style = PP.type.caption.copy(fontSize = 11.5.sp, lineHeight = 16.sp), color = metaColor)
                        if (ticks != null) PPTicks(ticks)
                    }
                }
            }
        }
        if (agent) {
            // The green rail (css .pp-bubble--agent::before): 3dp, 7dp in, 9dp from top and bottom.
            Box(
                Modifier
                    .matchParentSize()
                    .padding(start = if (showAvatar) 36.dp + 7.dp else 7.dp, top = 9.dp + if (position == PPBubblePosition.Middle || position == PPBubblePosition.Last) 2.dp else 0.dp, bottom = 9.dp),
            ) { Box(Modifier.width(3.dp).fillMaxHeight().clip(CircleShape).background(c.bubbleAgentRail)) }
        }
    }
}

/** Day separator in a conversation (css .pp-day). */
@Composable
fun PPDayDivider(label: String, modifier: Modifier = Modifier) {
    val c = PP.colors
    Box(modifier.fillMaxWidth().padding(vertical = 8.dp), contentAlignment = Alignment.Center) {
        Text(
            label,
            Modifier.clip(CircleShape).background(c.surfaceSunken).border(1.dp, c.borderSubtle, CircleShape).padding(horizontal = 10.dp, vertical = 3.dp).semantics { heading() },
            style = PP.type.caption.copy(fontWeight = FontWeight.Medium),
            color = c.textMuted,
        )
    }
}

// ---- Typing --------------------------------------------------------------------------------------

/**
 * Typing indicator (css .pp-typing): three dots bouncing in a theirs-bubble. [inline] is the bare dots
 * for thread rows and headers.
 */
@Composable
fun PPTypingIndicator(modifier: Modifier = Modifier, inline: Boolean = false, color: Color? = null, label: String = "Typing") {
    val c = PP.colors
    val t = rememberInfiniteTransition(label = "typing")
    val dot = if (inline) 5.dp else 7.dp
    val dotColor = color ?: if (inline) c.success else c.textFaint
    val dots = @Composable {
        Row(horizontalArrangement = Arrangement.spacedBy(if (inline) 3.dp else 4.dp), verticalAlignment = Alignment.CenterVertically) {
            for (i in 0 until 3) {
                val p by t.animateFloat(
                    0f, 0f,
                    infiniteRepeatable(
                        keyframes {
                            durationMillis = 1200
                            0f at 0 using FastOutSlowInEasing
                            1f at 360 using FastOutSlowInEasing
                            0f at 720
                            0f at 1200
                        },
                        RepeatMode.Restart,
                        StartOffset(i * 150),
                    ),
                    label = "dot$i",
                )
                Box(
                    Modifier
                        .graphicsLayer { translationY = -4.dp.toPx() * p; alpha = 0.45f + 0.55f * p }
                        .size(dot)
                        .clip(CircleShape)
                        .background(dotColor),
                )
            }
        }
    }
    if (inline) {
        Box(modifier.semantics { contentDescription = label }) { dots() }
    } else {
        val shape = RoundedCornerShape(topStart = PPRadius.bubble, topEnd = PPRadius.bubble, bottomEnd = PPRadius.bubble, bottomStart = PPRadius.bubbletail)
        Box(
            modifier
                .height(36.dp)
                .clip(shape)
                .background(c.bubbleTheirs)
                .border(1.dp, c.bubbleTheirsBorder, shape)
                .padding(horizontal = 14.dp)
                .semantics { contentDescription = label },
            contentAlignment = Alignment.Center,
        ) { dots() }
    }
}

// ---- Composer ------------------------------------------------------------------------------------

/**
 * Message composer (css .pp-composer): a 24dp-radius box with attach button, growing text field
 * (40–168dp) and a 40dp send circle. Optional [modes] row (Message · Do this work · Ask a question)
 * for writing to agents. Send is disabled while the text is blank.
 */
@Composable
fun PPComposer(
    value: String,
    onValueChange: (String) -> Unit,
    onSend: () -> Unit,
    modifier: Modifier = Modifier,
    placeholder: String = "Message",
    onAttach: (() -> Unit)? = null,
    onVoice: (() -> Unit)? = null,
    sending: Boolean = false,
    modes: List<Pair<PPIcon, String>> = emptyList(),
    selectedMode: Int = 0,
    onModeSelected: (Int) -> Unit = {},
) {
    val c = PP.colors
    val source = remember { MutableInteractionSource() }
    val focused by source.collectIsFocusedAsState()
    val shape = RoundedCornerShape(PPRadius.x2xl)
    val canSend = value.isNotBlank() && !sending
    Column(
        modifier
            .fillMaxWidth()
            .then(if (focused) Modifier.border(3.dp, PP.focusRing, RoundedCornerShape(PPRadius.x2xl + 3.dp)).padding(3.dp) else Modifier.padding(3.dp))
            .shadow(if (focused) 0.dp else 2.dp, shape, ambientColor = Color(0x0A12161C), spotColor = Color(0x0F12161C))
            .clip(shape)
            .background(c.surface)
            .border(1.dp, if (focused) c.accent else c.border, shape)
            .padding(6.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (modes.isNotEmpty()) {
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                modes.forEachIndexed { i, (icon, label) ->
                    val sel = i == selectedMode
                    Row(
                        Modifier
                            .height(30.dp)
                            .clip(RoundedCornerShape(PPRadius.s))
                            .background(if (sel) c.surfaceSelected else Color.Transparent)
                            .selectable(sel, role = Role.Tab) { onModeSelected(i) }
                            .padding(horizontal = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        PPIconView(icon, size = 15.dp, tint = if (sel) c.text else c.textMuted)
                        Text(label, style = PP.type.bodyS.copy(fontWeight = FontWeight.Medium, lineHeight = 13.5.sp), color = if (sel) c.text else c.textMuted)
                    }
                }
            }
        }
        Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(2.dp)) {
            if (onAttach != null) ComposerButton(PPIcon.Paperclip, "Attach a file", onAttach)
            BasicTextField(
                value = value,
                onValueChange = onValueChange,
                interactionSource = source,
                textStyle = PP.type.body.copy(color = c.text),
                cursorBrush = SolidColor(c.accent),
                modifier = Modifier.weight(1f).heightIn(min = 40.dp, max = 168.dp).semantics { contentDescription = placeholder },
                decorationBox = { inner ->
                    Box(Modifier.padding(horizontal = 6.dp, vertical = 9.dp)) {
                        if (value.isEmpty()) Text(placeholder, style = PP.type.body, color = c.textFaint)
                        inner()
                    }
                },
            )
            if (onVoice != null && value.isBlank()) ComposerButton(PPIcon.Mic, "Record a voice note", onVoice)
            Box(
                Modifier
                    .size(40.dp)
                    .then(if (canSend) Modifier.shadow(6.dp, CircleShape, ambientColor = c.accent.copy(alpha = 0.3f), spotColor = c.accent.copy(alpha = 0.3f)) else Modifier)
                    .clip(CircleShape)
                    .background(if (canSend || sending) c.accent else c.surfaceSunken)
                    .then(if (canSend || sending) Modifier else Modifier.border(1.dp, c.border, CircleShape))
                    .clickable(enabled = canSend, role = Role.Button, onClick = onSend)
                    .semantics {
                        contentDescription = "Send"
                        if (sending) stateDescription = "Sending"
                    },
                contentAlignment = Alignment.Center,
            ) {
                if (sending) PPSpinner(size = 18.dp, color = c.onAccent)
                else PPIconView(PPIcon.SendHorizontal, size = 19.dp, tint = if (canSend) c.onAccent else c.textFaint)
            }
        }
    }
}

@Composable
private fun ComposerButton(icon: PPIcon, label: String, onClick: () -> Unit) {
    Box(
        Modifier.size(40.dp).clip(CircleShape).clickable(role = Role.Button, onClick = onClick).semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) { PPIconView(icon, size = 21.dp, tint = PP.colors.textMuted) }
}

// ---- Thread row ----------------------------------------------------------------------------------

/**
 * Inbox row (css .pp-thread): 44dp avatar, name + handle, time, one-line preview and unread count.
 * [typing] replaces the preview with a green "typing" line; [held] shows the amber HELD flag for a
 * request waiting for review.
 */
@Composable
fun PPThreadRow(
    peer: String,
    preview: String,
    time: String,
    modifier: Modifier = Modifier,
    name: String? = null,
    unread: Int = 0,
    isAgent: Boolean = false,
    muted: Boolean = false,
    pinned: Boolean = false,
    typing: Boolean = false,
    held: Boolean = false,
    ticks: PPTickState? = null,
    presence: PPPresence? = null,
    selected: Boolean = false,
    previewSender: String? = null,
    onClick: () -> Unit = {},
) {
    val c = PP.colors
    val shown = name ?: PeerFace.displayName(peer)
    val isUnread = unread > 0
    Row(
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(PPRadius.l))
            .background(if (selected) c.surfaceSelected else Color.Transparent)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) {
                stateDescription = buildList {
                    if (isUnread) add("$unread unread")
                    if (muted) add("muted")
                    if (held) add("held for review")
                }.joinToString(", ")
            }
            .padding(10.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box {
            PPAvatar(peer, name = name, size = PPAvatarSize.l, icon = if (isAgent) PPIcon.Bot else null, tone = if (isAgent) PPTone.Green else null)
            if (presence != null) PPPresenceDot(presence, Modifier.align(Alignment.BottomEnd).offset(1.dp, 1.dp), ring = c.bgSubtle)
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
              Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(shown, Modifier.weight(1f, fill = false), style = PP.type.body.copy(fontWeight = FontWeight.SemiBold), color = c.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
                if (name != null || shown != peer) {
                    Text(peer, Modifier.weight(1f, fill = false), style = PP.type.mono.copy(fontSize = 12.sp, lineHeight = 12.sp), color = c.textFaint, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                if (isAgent) PPIconView(PPIcon.Bot, size = 14.dp, tint = c.success)
                if (held) HeldFlag()
              }
                Text(
                    time,
                    style = PP.type.caption.copy(fontWeight = if (isUnread) FontWeight.SemiBold else FontWeight.Normal),
                    color = if (isUnread && !muted) c.textLink else c.textFaint,
                    maxLines = 1,
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
                    if (typing) {
                        Text("typing", style = PP.type.bodyS.copy(fontWeight = FontWeight.Medium), color = c.success)
                        Spacer(Modifier.width(6.dp))
                        PPTypingIndicator(inline = true)
                    } else {
                        if (ticks != null) { PPTicks(ticks); Spacer(Modifier.width(2.dp)) }
                        Text(
                            (if (previewSender != null) "$previewSender: " else "") + preview,
                            style = PP.type.bodyS.copy(fontWeight = if (isUnread) FontWeight.Medium else FontWeight.Normal),
                            color = if (isUnread) c.textBody else c.textMuted,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
                if (muted) PPIconView(PPIcon.BellOff, size = 15.dp, tint = c.textFaint)
                if (pinned) PPIconView(PPIcon.Pin, size = 15.dp, tint = c.textFaint)
                if (isUnread) {
                    Box(
                        Modifier.height(20.dp).defaultMinSize(minWidth = 20.dp).clip(CircleShape).background(if (muted) c.textFaint else c.accent).padding(horizontal = 6.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(if (unread > 99) "99+" else "$unread", style = PP.type.caption.copy(fontSize = 11.5.sp, lineHeight = 20.sp, fontWeight = FontWeight.SemiBold), color = c.onAccent)
                    }
                }
            }
        }
    }
}

@Composable
private fun HeldFlag() {
    val c = PP.colors
    Row(
        Modifier.height(18.dp).clip(RoundedCornerShape(PPRadius.xs)).background(c.warningFill).padding(horizontal = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Box(Modifier.size(5.dp).clip(CircleShape).background(c.warning))
        Text("HELD", style = PP.type.caption.copy(fontSize = 10.5.sp, lineHeight = 10.5.sp, fontWeight = FontWeight.Bold, letterSpacing = 0.05.em), color = c.warning)
    }
}

// ---- Conversation header -------------------------------------------------------------------------

/** One action in a header or bar: icon + accessible label. */
data class PPAction(val icon: PPIcon, val label: String, val onClick: () -> Unit)

/**
 * Conversation header (css .pp-convhead): back, avatar with presence, name (+ verified mark) and a
 * subtitle line (handle · online / typing / last seen), then up to three 44dp actions.
 */
@Composable
fun PPConversationHeader(
    peer: String,
    modifier: Modifier = Modifier,
    name: String? = null,
    subtitle: String? = null,
    online: Boolean = false,
    typing: Boolean = false,
    isAgent: Boolean = false,
    verified: Boolean = false,
    onBack: (() -> Unit)? = null,
    actions: List<PPAction> = emptyList(),
) {
    val c = PP.colors
    Column(modifier.fillMaxWidth().background(c.surface)) {
        Row(
            Modifier.fillMaxWidth().defaultMinSize(minHeight = 56.dp).padding(start = if (onBack != null) 4.dp else 16.dp, end = 8.dp, top = 6.dp, bottom = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (onBack != null) PPIconButton(PPIcon.ChevronLeft, "Back", onBack)
            Row(Modifier.weight(1f).semantics(mergeDescendants = true) { heading() }, verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                PPAvatar(
                    peer, name = name, size = 38.dp,
                    icon = if (isAgent) PPIcon.Bot else null, tone = if (isAgent) PPTone.Green else null,
                    presence = if (online) PPPresence.Online else null,
                )
                Column(verticalArrangement = Arrangement.spacedBy(1.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text(name ?: PeerFace.displayName(peer), Modifier.weight(1f, fill = false), style = PP.type.h4, color = c.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        if (verified) PPIconView(PPIcon.BadgeCheck, size = 16.dp, tint = c.accent, contentDescription = "Verified")
                        if (isAgent) PPAgentBadge()
                    }
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text(peer, Modifier.weight(1f, fill = false), style = PP.type.mono.copy(fontSize = 12.sp, lineHeight = 16.sp), color = c.textMuted, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        val extra = when {
                            typing -> "typing"
                            online -> "online"
                            else -> subtitle
                        }
                        if (extra != null) {
                            Text("·", style = PP.type.caption, color = c.textFaint)
                            Text(extra, style = PP.type.caption, color = if (typing || online) c.success else c.textMuted, maxLines = 1)
                            if (typing) PPTypingIndicator(inline = true)
                        }
                    }
                }
            }
            actions.take(3).forEach { PPIconButton(it.icon, it.label, it.onClick) }
        }
        Box(Modifier.fillMaxWidth().height(1.dp).background(c.border))
    }
}

// ---- Address chip --------------------------------------------------------------------------------

/** css .pp-addr variants. */
enum class PPAddressVariant { Default, Key, Org, Agent, Error, Plain }

/** css .pp-addr sizes: S 22, M 28, L 34. */
enum class PPAddressSize(val height: Dp, val font: Float, val padStart: Dp) { S(22.dp, 12f, 8.dp), M(28.dp, 13f, 10.dp), L(34.dp, 14f, 12.dp) }

/**
 * Address / handle chip (css .pp-addr), Geist Mono. [onCopy] adds a copy button, [onRemove] an x
 * (recipient fields). Long key addresses are truncated in the middle.
 */
@Composable
fun PPAddressChip(
    address: String,
    modifier: Modifier = Modifier,
    variant: PPAddressVariant = PPAddressVariant.Default,
    size: PPAddressSize = PPAddressSize.M,
    verified: Boolean = false,
    onCopy: (() -> Unit)? = null,
    onRemove: (() -> Unit)? = null,
) {
    val c = PP.colors
    val (bg, bd, fg) = when (variant) {
        PPAddressVariant.Default -> Triple(c.surfaceSunken, c.border, c.text)
        PPAddressVariant.Key -> Triple(c.accentSubtle, c.accentSubtleBorder, c.textLink)
        PPAddressVariant.Org -> Triple(c.warningFill, c.warningBorder, c.text)
        PPAddressVariant.Agent -> Triple(c.successFill, c.successBorder, c.text)
        PPAddressVariant.Error -> Triple(c.dangerFill, c.dangerBorder, c.danger)
        PPAddressVariant.Plain -> Triple(Color.Transparent, Color.Transparent, c.text)
    }
    val shown = if (variant == PPAddressVariant.Key && address.length > 18) address.take(10) + "…" + address.takeLast(4) else address
    val hasButton = onCopy != null || onRemove != null
    Row(
        modifier
            .height(size.height)
            .clip(CircleShape)
            .background(bg)
            .border(1.dp, bd, CircleShape)
            .semantics(mergeDescendants = true) { contentDescription = "Address $address" + if (verified) ", verified" else "" }
            .padding(start = if (variant == PPAddressVariant.Plain) 0.dp else size.padStart, end = if (hasButton) 4.dp else size.padStart),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        when (variant) {
            PPAddressVariant.Key -> PPIconView(PPIcon.KeyRound, Modifier.offset(x = (-2).dp), size = 14.dp, tint = fg)
            PPAddressVariant.Agent -> PPIconView(PPIcon.Bot, Modifier.offset(x = (-2).dp), size = 14.dp, tint = c.success)
            else -> Unit
        }
        Text(shown, Modifier.weight(1f, fill = false), style = PP.type.mono.copy(fontSize = size.font.sp, lineHeight = size.font.sp, fontWeight = FontWeight.Medium), color = fg, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (verified) PPIconView(PPIcon.BadgeCheck, size = 15.dp, tint = if (variant == PPAddressVariant.Org) c.warningSolid else c.accent)
        if (onCopy != null) ChipButton(PPIcon.Copy, "Copy $address", onCopy)
        if (onRemove != null) ChipButton(PPIcon.X, "Remove $address", onRemove)
    }
}

@Composable
private fun ChipButton(icon: PPIcon, label: String, onClick: () -> Unit) {
    Box(
        Modifier.size(22.dp).clip(CircleShape).clickable(role = Role.Button, onClick = onClick).semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) { PPIconView(icon, size = 13.dp, tint = PP.colors.textFaint) }
}

// ---- Previews ------------------------------------------------------------------------------------

@PPPreviews
@Composable
private fun ConversationPreview() = PPPreviewSurface {
    PPDayDivider("Today")
    PPMessageBubble("Morning. Is the offer letter for Özge ready to send?", "09:12", PPBubbleRole.Theirs, sender = "Ada Yılmaz", senderPeer = "/ada", position = PPBubblePosition.First)
    PPMessageBubble("She starts on 1 December.", "09:12", PPBubbleRole.Theirs, sender = "Ada Yılmaz", senderPeer = "/ada", position = PPBubblePosition.Last)
    PPMessageBubble("Yes, I asked /wodo/hr to draft it.", "09:14", PPBubbleRole.Mine, ticks = PPTickState.Seen, position = PPBubblePosition.First)
    PPMessageBubble("Will forward it after lunch.", "09:14", PPBubbleRole.Mine, ticks = PPTickState.Delivered, edited = true, position = PPBubblePosition.Last)
    PPMessageBubble("Draft is ready: offer letter for Özge Arıç, start date 1 December, signed by Emre Dağ.", "09:20", PPBubbleRole.Agent, sender = "HR helper", senderPeer = "/wodo/hr")
    PPMessageBubble("Thanks", "09:21", PPBubbleRole.Mine, ticks = PPTickState.Sending)
    PPTypingIndicator(Modifier.padding(start = 36.dp))
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        PPTickState.entries.forEach { PPTicks(it) }
    }
}

@PPPreviews
@Composable
private fun ComposerPreview() = PPPreviewSurface {
    var text by remember { mutableStateOf("") }
    PPComposer(text, { text = it }, {}, onAttach = {}, onVoice = {})
    PPComposer(
        "Summarise this week's messages from /wodo/hr", {}, {}, onAttach = {},
        modes = listOf(PPIcon.MessageCircle to "Message", PPIcon.Zap to "Do this work", PPIcon.HelpCircle to "Ask a question"),
        selectedMode = 1, placeholder = "Ask /bekir/agent1",
    )
    PPComposer("Sending now", {}, {}, sending = true)
}

@PPPreviews
@Composable
private fun ThreadsPreview() = PPPreviewSurface {
    Column(Modifier.clip(RoundedCornerShape(PPRadius.l)).background(PP.colors.bgSubtle)) {
        PPThreadRow("/ada", "Can you join the 14:30 call?", "14:05", name = "Ada Yılmaz", unread = 2, presence = PPPresence.Online)
        PPThreadRow("/wodo/hr", "Draft is ready: offer letter for Özge", "13:40", isAgent = true, ticks = PPTickState.Working)
        PPThreadRow("/emre", "", "12:02", name = "Emre Dağ", typing = true, selected = true)
        PPThreadRow("/marina", "Sounds good, see you Monday", "Mon", name = "Marina Petrova", ticks = PPTickState.Seen, previewSender = "You", muted = true, unread = 4)
        PPThreadRow("/k/9mjpq4x7v2hz8r", "Wants to send you a contract to review", "Sun", held = true)
    }
}

@PPPreviews
@Composable
private fun HeaderAndChipsPreview() = PPPreviewSurface {
    PPConversationHeader(
        "/ada", name = "Ada Yılmaz", online = true, verified = true, onBack = {},
        actions = listOf(PPAction(PPIcon.Phone, "Call", {}), PPAction(PPIcon.Video, "Video call", {}), PPAction(PPIcon.MoreVertical, "More", {})),
    )
    PPConversationHeader("/wodo/hr", name = "HR helper", isAgent = true, typing = true, onBack = {})
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        PPAddressChip("/bekir", verified = true, onCopy = {})
        PPAddressChip("/wodo/hr", variant = PPAddressVariant.Org, verified = true)
    }
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        PPAddressChip("/k/9mjpq4x7v2hz8rw3", variant = PPAddressVariant.Key, size = PPAddressSize.S)
        PPAddressChip("/bekir/agent1", variant = PPAddressVariant.Agent, onRemove = {})
        PPAddressChip("/nobody", variant = PPAddressVariant.Error, size = PPAddressSize.S)
    }
}
