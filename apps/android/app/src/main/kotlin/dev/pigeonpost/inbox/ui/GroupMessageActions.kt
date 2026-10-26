package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pigeonpost.core.Features
import dev.pigeonpost.core.groups.*
import dev.pigeonpost.inbox.InboxViewModel
import java.text.DateFormat
import java.util.Date

@Composable
internal fun GroupReceiptMark(item: HistoryItem, groups: GroupsState, shareReads: Boolean, meta: Color) {
    if (!item.pending && Features.RECEIPTS !in groups.features) return
    val receipts = groups.receipts[groups.open]?.let { if (shareReads) it else it.copy(readReceipts = false) }
    val state = if (item.pending && item.sendFailed) "unconfirmed" else GroupActions.receipt(item, receipts)
    val icon = when (state) {
        "sending" -> Icons.Outlined.Schedule
        "unconfirmed" -> Icons.Outlined.WarningAmber
        "seen", "delivered" -> Icons.Outlined.DoneAll
        else -> Icons.Outlined.Check
    }
    val label = when (state) {
        "sending" -> "Sending"
        "unconfirmed" -> "Delivery unconfirmed"
        "seen" -> "Seen"
        "delivered" -> "Delivered"
        else -> "Sent"
    }
    Icon(icon, label, Modifier.size(15.dp).testTag("groupReceipt:$state"), tint = when (state) {
        "seen" -> Pp.colors.green
        "unconfirmed" -> Pp.colors.blockedText
        else -> meta
    })
}

@Composable
internal fun GroupComposerContext(gid: String, groups: GroupsState, store: GroupStore, model: InboxViewModel, name: (String?) -> String) {
    val p = Pp.colors
    val mode = groups.composers[gid]
    val pending = groups.history[gid].orEmpty().firstOrNull { it.kind == "message" && it.pending }
    var stop by remember(groups.contextId, gid, pending?.cmid) { mutableStateOf(false) }
    if (mode != null) Row(Modifier.fillMaxWidth().padding(start = 12.dp, end = 4.dp, top = 8.dp).testTag("groupComposerMode"), verticalAlignment = Alignment.CenterVertically) {
        Icon(if (mode.kind == GroupComposerKind.EDIT) Icons.Outlined.Edit else Icons.Outlined.Reply, null, Modifier.size(18.dp), tint = p.accent)
        Column(Modifier.weight(1f).padding(horizontal = 8.dp)) {
            Text(if (mode.kind == GroupComposerKind.EDIT) "Editing message" else "Replying to ${name(mode.sender)}", style = geist(12.5.sp, FontWeight.SemiBold), color = p.accent)
            Text(mode.excerpt, style = geist(12.sp), color = p.secondary, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        IconButton({ store.cancelAction(gid) }, enabled = !groups.sending && pending == null, modifier = Modifier.size(36.dp)) {
            Icon(Icons.Outlined.Close, if (mode.kind == GroupComposerKind.EDIT) "Cancel edit" else "Cancel reply", Modifier.size(18.dp), tint = p.secondary)
        }
    }
    if (pending != null && !groups.sending) Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp).testTag("groupUnconfirmedSend")) {
        Text("We couldn't confirm this send. Retrying checks the same message.", style = geist(12.5.sp), color = p.heldText)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            val cmid = pending.cmid
            if (cmid != null && cmid in groups.pendingIds[gid].orEmpty()) TextButton({ model.retryGroup(gid, cmid) }) { Text("Retry") }
            TextButton({ stop = true }) { Text("Stop retrying") }
        }
    }
    if (stop && pending?.cmid != null) Confirm("Stop retrying?", "The message may already have arrived. This stops local retries; it does not delete a delivered message.", "Stop retrying",
        { stop = false; store.discard(gid, pending.cmid!!) }, { stop = false })
}

/** Only versions decrypted by this device are displayed; unavailable history stays explicit. */
@Composable
internal fun GroupMessageInfoDialog(item: HistoryItem, history: List<HistoryItem>, groups: GroupsState, shareReads: Boolean, name: (String?) -> String, dismiss: () -> Unit) {
    val p = Pp.colors
    val versions = GroupActions.versions(item, history)
    val receipts = groups.receipts[groups.open]
    val date = remember { DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT) }
    PageDialog("Message info", dismiss) {
        Text(name(item.sender), style = geist(14.sp, FontWeight.SemiBold), color = p.ink)
        if (item.createdAt > 0) Text(date.format(Date(item.createdAt * 1000)), style = geist(12.5.sp), color = p.secondary)
        if (versions.firstOrNull()?.editOf != null) Text("Earlier versions aren't on this device.", style = geist(13.sp), color = p.heldText)
        if (versions.size > 1 || item.editOf != null) Text("Versions", style = geist(14.sp, FontWeight.SemiBold), color = p.ink)
        versions.forEachIndexed { index, version ->
            val label = if (index == 0 && version.editOf == null) "Original" else "Edit $index"
            Text(label + if (version.createdAt > 0) " · " + date.format(Date(version.createdAt * 1000)) else "", style = geist(12.sp, FontWeight.Medium), color = p.muted)
            if (version.deleted || version.undecryptable || version.text == null) Text("This version isn't available on this device.", style = geist(13.sp), color = p.secondary)
            else SelectionContainer { Text(version.text!!, style = geist(14.sp), color = p.body) }
        }
        if (item.sender == groups.mailbox && Features.RECEIPTS in groups.features) {
            Text("Delivery", style = geist(14.sp, FontWeight.SemiBold), color = p.ink)
            if (receipts == null) Text("Member delivery information isn't available yet.", style = geist(13.sp), color = p.secondary)
            else if (receipts.members.isEmpty()) Text("No other active members.", style = geist(13.sp), color = p.secondary)
            else receipts.members.forEach { member ->
                val seq = item.seq ?: Long.MAX_VALUE
                val label = when {
                    member.deliveredSeq < seq -> "Sent"
                    shareReads && receipts.readReceipts && (member.seenSeq ?: 0) >= seq -> "Seen"
                    else -> "Delivered"
                }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text(name(member.address), Modifier.weight(1f), style = geist(13.sp), color = p.body)
                    Text(label, style = geist(12.5.sp), color = if (label == "Seen") p.green else p.muted)
                }
            }
        }
    }
}
