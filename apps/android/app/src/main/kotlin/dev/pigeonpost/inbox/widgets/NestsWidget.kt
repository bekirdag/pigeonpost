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
import androidx.glance.unit.ColorProvider
import dev.pigeonpost.core.widgets.WidgetNest
import dev.pigeonpost.core.widgets.WidgetNests
import dev.pigeonpost.core.widgets.WidgetSnapshot
import dev.pigeonpost.design.PPIcon
import dev.pigeonpost.inbox.ui.nestStatusLabel

/** Nests: each Nest's name, status and running hours, from what the app last loaded. */
class NestsWidget : GlanceAppWidget() {
    companion object {
        val SQUARE = DpSize(110.dp, 110.dp)
        val MEDIUM = DpSize(250.dp, 110.dp)
        val LARGE = DpSize(250.dp, 250.dp)
        val TABLET = DpSize(400.dp, 300.dp)
    }
    override val sizeMode = SizeMode.Responsive(setOf(SQUARE, MEDIUM, LARGE, TABLET))
    override val previewSizeMode = SizeMode.Responsive(setOf(MEDIUM, LARGE))
    override val stateDefinition = null

    override suspend fun provideGlance(context: Context, id: GlanceId) {
        val snapshots = WidgetStore.snapshots(context)
        provideContent { val snapshot by snapshots.collectAsState(); NestsContent(snapshot) }
    }

    override suspend fun providePreview(context: Context, widgetCategory: Int) {
        provideContent { NestsContent(WidgetSamples.snapshot(nowSeconds())) }
    }
}

class NestsWidgetReceiver : PigeonpostWidgetReceiver() { override val glanceAppWidget: GlanceAppWidget = NestsWidget() }

/** The status dot's colours: the kit's success, info, warning and danger. */
internal fun nestTone(status: String, reason: String?): ColorProvider = when (status) {
    "ready" -> WidgetColors.success
    "working" -> WidgetColors.info
    "needs_you", "paused" -> WidgetColors.warning
    else -> if (reason == "setting_up") WidgetColors.info else WidgetColors.danger
}

/** "12 h of 100 h", "12 h this month", or "100 h a month": only what the app has read. */
internal fun nestHours(nest: WidgetNest, limit: Int?): Pair<String, Float?>? {
    val hours = nest.runningSecs?.let { it / 3600 }
    return when {
        hours != null && limit != null -> "$hours h of $limit h" to (hours.toFloat() / limit)
        hours != null -> "$hours h this month" to null
        limit != null -> "$limit h a month" to null
        else -> null
    }
}

@Composable
fun NestsContent(snapshot: WidgetSnapshot?) {
    val size = LocalSize.current
    val nests = snapshot?.nests
    val compact = size.width < 200.dp
    WidgetCard(WidgetTargets.nests(), padding = if (size.width >= 400.dp) WidgetSpace.gutterLarge else WidgetSpace.gutter) {
        Column(GlanceModifier.fillMaxSize()) {
            if (snapshot != null) {
                WidgetHeader(PPIcon.Egg, "Nests", count = null, compact = compact, trailing = if (nests?.enabled == true && nests.count > 0) ({ Label("${nests.count}", WidgetType.bodySmall, WidgetColors.muted, FontWeight.Medium) }) else null)
                Spacer(GlanceModifier.height(8.dp))
            }
            when {
                snapshot == null -> SignedOut(compact = compact)
                nests == null -> WidgetMessage(PPIcon.Egg, "Open Pigeonpost", "Your Nests show here once the app has loaded them.", compact = compact)
                !nests.enabled -> WidgetMessage(PPIcon.Egg, "Nests are not available", "Nests are not available for this account yet.", compact = compact)
                snapshot.locked -> WidgetMessage(PPIcon.Lock, if (nests.count == 1) "1 Nest" else "${nests.count} Nests", "Open Pigeonpost to see them.", compact = compact)
                nests.nests.isEmpty() -> WidgetMessage(PPIcon.Egg, "No Nests yet", if (compact) null else "A Nest is an AI agent with its own private workspace.",
                    "Get a Nest", WidgetTargets.nests(), compact = compact)
                else -> LazyColumn(GlanceModifier.fillMaxSize()) {
                    items(nests.nests, itemId = { it.id.hashCode().toLong() }) { nest -> NestRow(nest, nests, compact, size.width) }
                }
            }
        }
    }
}

@Composable
private fun NestRow(nest: WidgetNest, nests: WidgetNests, compact: Boolean, width: androidx.compose.ui.unit.Dp) {
    val status = nestStatusLabel(nest.status, nest.statusReason)
    val hours = nestHours(nest, nests.hoursLimit)
    Column(GlanceModifier.fillMaxWidth().padding(vertical = 5.dp).clickable(WidgetTargets.nest(nest.id))
        .semantics { contentDescription = listOfNotNull(nest.label, status, hours?.first).joinToString(", ") }) {
        Row(GlanceModifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Disc(8.dp, nestTone(nest.status, nest.statusReason))
            Spacer(GlanceModifier.width(8.dp))
            Label(nest.label, if (compact) WidgetType.bodySmall else WidgetType.body, WidgetColors.text, FontWeight.Medium, GlanceModifier.defaultWeight())
            if (!compact && hours != null) Label(hours.first, WidgetType.caption, WidgetColors.muted)
        }
        Row(GlanceModifier.fillMaxWidth().padding(start = 16.dp), verticalAlignment = Alignment.CenterVertically) {
            Label(status, WidgetType.caption, WidgetColors.muted, modifier = GlanceModifier.defaultWeight())
            val fraction = hours?.second
            if (!compact && fraction != null) { Spacer(GlanceModifier.width(8.dp)); Meter(fraction, if (width >= 400.dp) 120.dp else 72.dp, if (fraction >= 0.9f) WidgetColors.warning else WidgetColors.accent) }
        }
    }
}
