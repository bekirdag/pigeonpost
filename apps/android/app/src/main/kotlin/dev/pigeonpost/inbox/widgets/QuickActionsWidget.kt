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
import androidx.glance.action.Action
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.SizeMode
import androidx.glance.appwidget.provideContent
import androidx.glance.layout.Alignment
import androidx.glance.layout.Box
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxHeight
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.height
import androidx.glance.layout.width
import androidx.glance.semantics.contentDescription
import androidx.glance.semantics.semantics
import androidx.glance.text.FontWeight
import dev.pigeonpost.core.widgets.WidgetSnapshot
import dev.pigeonpost.design.PPIcon

/** Quick actions: 1x1 is "New message"; 2x1 adds "Inbox" with the unread count. */
class QuickActionsWidget : GlanceAppWidget() {
    companion object {
        val ONE = DpSize(48.dp, 48.dp)
        val TWO = DpSize(110.dp, 48.dp)
    }
    override val sizeMode = SizeMode.Responsive(setOf(ONE, TWO))
    override val previewSizeMode = SizeMode.Responsive(setOf(TWO))
    override val stateDefinition = null

    override suspend fun provideGlance(context: Context, id: GlanceId) {
        val snapshots = WidgetStore.snapshots(context)
        provideContent { val snapshot by snapshots.collectAsState(); QuickActionsContent(snapshot) }
    }

    override suspend fun providePreview(context: Context, widgetCategory: Int) {
        provideContent { QuickActionsContent(WidgetSamples.snapshot(nowSeconds())) }
    }
}

class QuickActionsWidgetReceiver : PigeonpostWidgetReceiver() { override val glanceAppWidget: GlanceAppWidget = QuickActionsWidget() }

@Composable
fun QuickActionsContent(snapshot: WidgetSnapshot?) {
    val size = LocalSize.current
    // Signed out, both buttons open the app, which shows sign-in.
    val compose = if (snapshot == null) WidgetTargets.app() else WidgetTargets.compose()
    val inbox = if (snapshot == null) WidgetTargets.app() else WidgetTargets.inbox()
    if (size.width < 100.dp) {
        WidgetCard(compose, padding = 6.dp) {
            Box(GlanceModifier.fillMaxSize(), contentAlignment = Alignment.Center) { ActionButton(PPIcon.Pencil, "New message", compose, 40.dp) }
        }
        return
    }
    WidgetCard(inbox, padding = 6.dp) {
        Row(GlanceModifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
            Tile(PPIcon.Pencil, "New", "New message", compose, filled = true, modifier = GlanceModifier.defaultWeight())
            Spacer(GlanceModifier.width(6.dp))
            Tile(PPIcon.Inbox, "Inbox", "Open inbox", inbox, filled = false, count = snapshot?.unread ?: 0, modifier = GlanceModifier.defaultWeight())
        }
    }
}

@Composable
private fun Tile(icon: PPIcon, label: String, description: String, action: Action, filled: Boolean, count: Int = 0, modifier: GlanceModifier) {
    Column(modifier.fillMaxHeight().clickable(action).semantics { contentDescription = if (count > 0) "$description, $count unread" else description },
        horizontalAlignment = Alignment.CenterHorizontally, verticalAlignment = Alignment.CenterVertically) {
        Box(contentAlignment = Alignment.TopEnd) {
            Disc(30.dp, if (filled) WidgetColors.accent else WidgetColors.accentSubtle) { WidgetIcon(icon, 16.dp, if (filled) WidgetColors.onAccent else WidgetColors.link) }
            if (count > 0) UnreadDot(10.dp)
        }
        Spacer(GlanceModifier.height(2.dp))
        Label(label, WidgetType.overline, WidgetColors.text, FontWeight.Medium)
    }
}
