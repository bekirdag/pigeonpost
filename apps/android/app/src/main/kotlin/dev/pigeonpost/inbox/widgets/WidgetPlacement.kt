package dev.pigeonpost.inbox.widgets

import android.appwidget.AppWidgetManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import androidx.glance.appwidget.GlanceAppWidgetReceiver

/**
 * Which Pigeonpost widgets are on a home screen. Asked once per process, then kept current by the
 * widgets' own receivers (every placement, removal, restore or update reaches one of them), so the
 * app does not query the widget service after every state change.
 *
 * With no widget placed, [WidgetSync] still keeps the snapshot file current but never calls Glance
 * or AppWidgetManager. Doing that after every app state change (getGlanceIds + updateAll for four
 * widgets) is what made the tablet emulator's Compose workflow tests fail at random (PR #321): with
 * the widget providers declared those calls reach the system widget service each time, while the
 * same sync with the providers removed, or no sync at all, passed.
 */
internal object WidgetPlacement {
    private val receivers: List<Class<out GlanceAppWidgetReceiver>> = listOf(
        InboxWidgetReceiver::class.java, QuickActionsWidgetReceiver::class.java, NestsWidgetReceiver::class.java, AgendaWidgetReceiver::class.java,
    )

    /** The receivers with at least one placed widget; null until asked (or after a receiver heard a change). */
    @Volatile private var placed: Set<Class<out GlanceAppWidgetReceiver>>? = null

    private fun placed(context: Context): Set<Class<out GlanceAppWidgetReceiver>> = placed ?: run {
        val app = context.applicationContext
        val now = runCatching {
            val manager = AppWidgetManager.getInstance(app)
            receivers.filter { manager.getAppWidgetIds(ComponentName(app, it)).isNotEmpty() }.toSet()
        }.getOrDefault(receivers.toSet()) // unknown: behave as if placed, as before
        placed = now
        now
    }

    fun any(context: Context): Boolean = placed(context).isNotEmpty()

    fun agenda(context: Context): Boolean = AgendaWidgetReceiver::class.java in placed(context)

    /** A widget receiver got a broadcast: placement may have changed; ask again next time. */
    fun changed() { placed = null }
}

/** The widgets' receivers: Glance's, plus keeping [WidgetPlacement] current. */
abstract class PigeonpostWidgetReceiver : GlanceAppWidgetReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        WidgetPlacement.changed()
        super.onReceive(context, intent)
    }
}
