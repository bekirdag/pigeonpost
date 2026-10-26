package dev.pigeonpost.inbox.ui

import android.animation.ValueAnimator
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pigeonpost.core.*
import java.text.DateFormat
import java.util.Date

@Composable
internal fun MessageReceiptMark(message: ThreadMessage, state: InboxState, meta: Color) {
    val delivery = message.deliveryState(state.features, state.policy?.readReceipts) ?: return
    val p = Pp.colors
    if (delivery == MessageDeliveryState.WORKING) {
        val animated = ValueAnimator.areAnimatorsEnabled()
        val angle = if (animated) {
            val transition = rememberInfiniteTransition(label = "agentWorking")
            transition.animateFloat(0f, 360f, infiniteRepeatable(tween(6000, easing = LinearEasing)), label = "agentGear").value
        } else 0f
        Icon(Icons.Outlined.Settings, "Agent working on this", Modifier.size(15.dp).rotate(angle).testTag("receipt:working"), tint = p.green)
        if (!animated) Text("Working…", style = geist(11.sp), color = p.green)
        return
    }
    val label = when (delivery) {
        MessageDeliveryState.SENDING -> "Sending"
        MessageDeliveryState.UNCONFIRMED -> if (message.pendingEditId != null) "Edit unconfirmed" else "Delivery unconfirmed"
        MessageDeliveryState.SENT -> "Sent"
        MessageDeliveryState.DELIVERED -> "Delivered"
        MessageDeliveryState.SEEN -> message.receipt?.seenAt?.takeIf { it > 0 }?.let { "Seen " + clockTime(it) } ?: "Seen"
        MessageDeliveryState.AGENT_FAILED -> "Agent run failed"
        MessageDeliveryState.WORKING -> "Agent working on this"
    }
    val icon = when (delivery) {
        MessageDeliveryState.SENDING -> Icons.Outlined.Schedule
        MessageDeliveryState.SENT -> Icons.Outlined.Check
        MessageDeliveryState.SEEN, MessageDeliveryState.DELIVERED -> Icons.Outlined.DoneAll
        else -> Icons.Outlined.WarningAmber
    }
    val tint = when (delivery) {
        MessageDeliveryState.SEEN -> p.green
        MessageDeliveryState.AGENT_FAILED, MessageDeliveryState.UNCONFIRMED -> p.blockedText
        else -> meta
    }
    Icon(icon, label, Modifier.size(15.dp).testTag("receipt:" + delivery.name.lowercase()), tint = tint)
}

@Composable
internal fun MessageReplyQuote(reply: MessageReply, conversationName: String, mine: Boolean, open: () -> Unit) {
    val p = Pp.colors
    val author = if (reply.direction == "out" || reply.sender == "You") "You" else conversationName
    val text = reply.quote ?: reply.excerpt ?: "Original message unavailable"
    Row(Modifier.fillMaxWidth().clip(RoundedCornerShape(7.dp)).background(if (mine) p.mineRule else p.tile)
        .clickable(onClickLabel = "Go to quoted message", onClick = open).testTag("replyQuote:" + (reply.messageId ?: reply.msgRef ?: "unavailable"))
        .padding(9.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Box(Modifier.width(2.dp).height(30.dp).background(p.accent))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(author, style = geist(11.5.sp, FontWeight.SemiBold), color = p.accent, maxLines = 1)
            Text(text, style = geist(12.sp), color = if (mine) p.mineMeta else p.secondary, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
    }
}

@Composable
internal fun MessageComposerContext(state: InboxState, store: InboxStore) {
    val p = Pp.colors
    val unconfirmed = state.unconfirmedSend
    var stop by remember(unconfirmed) { mutableStateOf(false) }
    state.composerAction?.let { mode ->
        Row(Modifier.fillMaxWidth().background(p.field).padding(start = 14.dp, end = 4.dp, top = 4.dp, bottom = 4.dp).testTag("composerMode"),
            verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(if (mode.editing) "Editing message" else if (mode.author == "You") "Replying to yourself" else "Replying to ${mode.author}", style = geist(12.sp, FontWeight.SemiBold), color = p.accent)
                Text(mode.quote, style = geist(12.sp), color = p.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            IconButton({ store.cancelMessageAction() }, enabled = !state.isSending && unconfirmed == null, modifier = Modifier.size(36.dp)) {
                Icon(Icons.Outlined.Close, if (mode.editing) "Cancel edit" else "Cancel reply", Modifier.size(16.dp), tint = p.secondary)
            }
        }
    }
    if (unconfirmed != null) Row(Modifier.fillMaxWidth().padding(start = 14.dp, end = 4.dp).testTag("unconfirmedSend"), verticalAlignment = Alignment.CenterVertically) {
        Text("Delivery unconfirmed", Modifier.weight(1f), style = geist(12.sp), color = p.blockedText)
        TextButton({ store.retrySend(unconfirmed) }, enabled = store.canRetrySend(unconfirmed)) { Text("Retry") }
        IconButton({ stop = true }, Modifier.size(36.dp)) { Icon(Icons.Outlined.Close, "Stop retrying", Modifier.size(16.dp), tint = p.secondary) }
    }
    if (!state.isSending && unconfirmed == null) store.messageComposerProblem()?.let {
        Text(it, Modifier.padding(horizontal = 14.dp, vertical = 6.dp), color = p.blockedText, style = geist(12.sp))
    }
    if (stop && unconfirmed != null) Confirm("Stop retrying?", "The message may already have arrived. This clears the pending draft without sending another copy.",
        "Stop retrying", { store.deleteMessage(unconfirmed); stop = false }, { stop = false })
}

private fun messageStamp(at: Long): String = DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(at * 1000))

@Composable
internal fun MessageInfoDialog(value: MessageInfoState, store: InboxStore, state: InboxState) {
    val p = Pp.colors
    PageDialog("Message info", store::clearMessageInfo) {
        when {
            value.loading -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                Text("Loading message info…", style = geist(14.sp), color = p.secondary)
            }
            value.error != null -> {
                Text(value.error.orEmpty(), style = geist(14.sp), color = p.blockedText)
                TextButton({ store.showMessageInfo(value.messageId) }) { Text("Retry") }
            }
            value.info != null -> {
                val info = value.info ?: return@PageDialog
                info.sentAt?.takeIf { it > 0 }?.let { Text("Sent · " + messageStamp(it), style = geist(13.sp), color = p.secondary) }
                if (info.direction == "out" && state.has(Features.RECEIPTS)) {
                    info.receipt?.deliveredAt?.takeIf { it > 0 }?.let { Text("Delivered · " + messageStamp(it), style = geist(13.sp), color = p.secondary) }
                    info.receipt?.seenAt?.takeIf { it > 0 && info.receipt?.state == "seen" && state.policy?.readReceipts != false }?.let { Text("Seen · " + messageStamp(it), style = geist(13.sp), color = p.green) }
                }
                if (info.direction == "out" && state.has(Features.AGENT_STATUS)) {
                    when (info.agentStatus?.state) {
                        "working" -> Text("Agent working on this", style = geist(13.sp), color = p.green)
                        "failed" -> Text("Agent run failed", style = geist(13.sp), color = p.blockedText)
                        "done" -> Text("Agent run completed", style = geist(13.sp), color = p.secondary)
                    }
                }
                Text("Versions", style = geist(14.sp, FontWeight.SemiBold), color = p.ink)
                if (info.versions.isEmpty()) Text("Version history is unavailable.", style = geist(13.sp), color = p.muted)
                info.versions.forEachIndexed { index, version ->
                    Text((if (index == 0) "Original" else "Edit $index") + " · " + messageStamp(version.at), style = geist(12.sp, FontWeight.Medium), color = p.muted)
                    SelectionContainer { Text(displayBody(version.body, info.direction == "out").text, style = geist(14.sp), color = p.body) }
                }
            }
        }
    }
}
