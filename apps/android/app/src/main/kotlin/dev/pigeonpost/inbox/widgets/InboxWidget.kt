package dev.pigeonpost.inbox.widgets

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.glance.GlanceId
import androidx.glance.GlanceModifier
import androidx.glance.LocalSize
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetReceiver
import androidx.glance.appwidget.SizeMode
import androidx.glance.appwidget.lazy.LazyColumn
import androidx.glance.appwidget.lazy.items
import androidx.glance.appwidget.provideContent
import androidx.glance.layout.Alignment
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.height
import androidx.glance.layout.padding
import androidx.glance.layout.width
import androidx.glance.semantics.contentDescription
import androidx.glance.semantics.semantics
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import dev.pigeonpost.core.widgets.WidgetConversation
import dev.pigeonpost.core.widgets.WidgetSnapshot
import dev.pigeonpost.design.PPIcon

/** Inbox: the unread count and the latest conversations, from 2x1 up to tablet sizes. */
class InboxWidget : GlanceAppWidget() {
    companion object {
        val STRIP = DpSize(110.dp, 48.dp)
        val SQUARE = DpSize(110.dp, 110.dp)
        val WIDE_STRIP = DpSize(250.dp, 48.dp)
        val MEDIUM = DpSize(250.dp, 110.dp)
        val LARGE = DpSize(250.dp, 250.dp)
        val TABLET = DpSize(400.dp, 300.dp)
        val SIZES = setOf(STRIP, SQUARE, WIDE_STRIP, MEDIUM, LARGE, TABLET)
    }
    override val sizeMode = SizeMode.Responsive(SIZES)
    override val previewSizeMode = SizeMode.Responsive(setOf(SQUARE, MEDIUM, LARGE))
    override val stateDefinition = null

    override suspend fun provideGlance(context: Context, id: GlanceId) {
        val snapshots = WidgetStore.snapshots(context)
        provideContent { val snapshot by snapshots.collectAsState(); InboxContent(snapshot, nowSeconds()) }
    }

    override suspend fun providePreview(context: Context, widgetCategory: Int) {
        provideContent { InboxContent(WidgetSamples.snapshot(nowSeconds()), nowSeconds()) }
    }
}

class InboxWidgetReceiver : GlanceAppWidgetReceiver() { override val glanceAppWidget: GlanceAppWidget = InboxWidget() }

internal fun nowSeconds() = System.currentTimeMillis() / 1000

@Composable
fun InboxContent(snapshot: WidgetSnapshot?, now: Long) {
    val size = LocalSize.current
    val short = size.height < 100.dp
    WidgetCard(WidgetTargets.inbox(), padding = when { short -> 8.dp; size.width >= 400.dp -> WidgetSpace.gutterLarge; else -> WidgetSpace.gutter }) {
        when {
            snapshot == null -> SignedOut(compact = short)
            short -> InboxStrip(snapshot, now, wide = size.width >= 200.dp)
            else -> InboxList(snapshot, now, compact = size.width < 180.dp, roomy = size.width >= 400.dp)
        }
    }
}

@Composable
internal fun SignedOut(compact: Boolean) =
    WidgetMessage(PPIcon.Bird, "Sign in to Pigeonpost", "Your latest conversations show here.", compact = compact)

private fun unreadLabel(count: Int) = when (count) { 0 -> "No new messages"; 1 -> "1 new message"; else -> "$count new messages" }

/** 2x1 and 4x1: the count, and on a wide strip the latest unread conversation. */
@Composable
private fun InboxStrip(snapshot: WidgetSnapshot, now: Long, wide: Boolean) {
    Row(GlanceModifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
        val latest = snapshot.conversations.firstOrNull { it.unread > 0 }.takeIf { wide }
        // With a conversation to show, its face replaces the inbox icon: a 4x1 strip has no room for both.
        if (latest == null) { Disc(32.dp, WidgetColors.accentSubtle) { WidgetIcon(PPIcon.Inbox, 18.dp, WidgetColors.link) }; Spacer(GlanceModifier.width(10.dp)) }
        // The count keeps its width; whatever is left goes to the latest conversation's name.
        Column(if (latest == null) GlanceModifier.defaultWeight() else GlanceModifier) {
            Text(if (snapshot.unread > 999) "999+" else snapshot.unread.toString(),
                style = WidgetType.style(WidgetType.title, if (snapshot.unread > 0) WidgetColors.link else WidgetColors.text, FontWeight.Bold), maxLines = 1)
            Label(if (snapshot.unread == 0) "No new messages" else "unread")
        }
        if (latest != null) {
            Spacer(GlanceModifier.width(12.dp))
            Row(GlanceModifier.defaultWeight().clickable(WidgetTargets.conversation(snapshot.mailbox, latest.peer)), verticalAlignment = Alignment.CenterVertically) {
                Initials(latest.peer, latest.name, 28.dp, latest.group)
                Spacer(GlanceModifier.width(8.dp))
                Label(latest.name, WidgetType.bodySmall, WidgetColors.text, FontWeight.Bold, GlanceModifier.defaultWeight())
            }
        }
        if (wide) { Spacer(GlanceModifier.width(8.dp)); ActionButton(PPIcon.Pencil, "New message", WidgetTargets.compose(), 32.dp) }
    }
}

@Composable
private fun InboxList(snapshot: WidgetSnapshot, now: Long, compact: Boolean, roomy: Boolean) {
    Column(GlanceModifier.fillMaxSize()) {
        WidgetHeader(PPIcon.Inbox, "Inbox", snapshot.unread, compact = compact, trailing = if (compact) null else ({
            ActionButton(PPIcon.Pencil, "New message", WidgetTargets.compose(), 30.dp)
        }))
        Spacer(GlanceModifier.height(if (compact) 6.dp else 8.dp))
        when {
            snapshot.locked -> WidgetMessage(PPIcon.Lock, unreadLabel(snapshot.unread), "Open Pigeonpost to see who wrote.", compact = compact)
            snapshot.conversations.isEmpty() -> WidgetMessage(PPIcon.MessagesSquare, "No conversations yet", "Start one from Pigeonpost.", compact = compact)
            else -> LazyColumn(GlanceModifier.fillMaxSize()) {
                items(snapshot.conversations, itemId = { it.peer.hashCode().toLong() }) { row ->
                    if (compact) CompactRow(snapshot.mailbox, row) else ConversationRow(snapshot.mailbox, row, now, roomy)
                }
            }
        }
    }
}

/** What the second line says. Message text appears only when the person turned previews on. */
internal fun subtitle(row: WidgetConversation): String = row.preview ?: when {
    row.unread == 1 -> "New message"
    row.unread > 1 -> "${row.unread} new messages"
    row.group -> "Group"
    row.peer.startsWith("/k/") -> "No new messages"
    else -> row.peer
}

@Composable
private fun ConversationRow(mailbox: String, row: WidgetConversation, now: Long, roomy: Boolean) {
    val unread = row.unread > 0
    Row(GlanceModifier.fillMaxWidth().padding(vertical = if (roomy) 7.dp else 5.dp).clickable(WidgetTargets.conversation(mailbox, row.peer))
        .semantics { contentDescription = "${row.name}, ${subtitle(row)}" }, verticalAlignment = Alignment.CenterVertically) {
        Initials(row.peer, row.name, if (roomy) 40.dp else 36.dp, row.group)
        Spacer(GlanceModifier.width(10.dp))
        Column(GlanceModifier.defaultWeight()) {
            Row(GlanceModifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Label(row.name, WidgetType.body, WidgetColors.text, if (unread) FontWeight.Bold else FontWeight.Medium, GlanceModifier.defaultWeight())
                Label(widgetTime(row.at, now), WidgetType.caption, if (unread) WidgetColors.link else WidgetColors.muted, if (unread) FontWeight.Medium else FontWeight.Normal)
            }
            Row(GlanceModifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Label(subtitle(row), WidgetType.bodySmall, if (unread) WidgetColors.body else WidgetColors.muted, if (unread) FontWeight.Medium else FontWeight.Normal, GlanceModifier.defaultWeight())
                if (unread) { Spacer(GlanceModifier.width(6.dp)); UnreadDot() }
            }
        }
    }
}

@Composable
private fun CompactRow(mailbox: String, row: WidgetConversation) {
    Row(GlanceModifier.fillMaxWidth().padding(vertical = 3.dp).clickable(WidgetTargets.conversation(mailbox, row.peer))
        .semantics { contentDescription = "${row.name}, ${subtitle(row)}" }, verticalAlignment = Alignment.CenterVertically) {
        Initials(row.peer, row.name, 24.dp, row.group)
        Spacer(GlanceModifier.width(8.dp))
        Label(row.name, WidgetType.bodySmall, WidgetColors.text, if (row.unread > 0) FontWeight.Bold else FontWeight.Normal, GlanceModifier.defaultWeight())
        if (row.unread > 0) { Spacer(GlanceModifier.width(4.dp)); UnreadDot(8.dp) }
    }
}
