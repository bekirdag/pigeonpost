package dev.pigeonpost.inbox.widgets

import android.appwidget.AppWidgetProviderInfo
import android.content.Context
import android.os.Build
import androidx.collection.intSetOf
import androidx.glance.appwidget.GlanceAppWidgetManager
import androidx.glance.appwidget.GlanceAppWidgetReceiver
import dev.pigeonpost.inbox.BuildConfig
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlin.reflect.KClass

/**
 * Android 15+: the widget picker shows generated previews, drawn by the widgets themselves from
 * [WidgetSamples]. Published once per app version (the system rate-limits it; a refused or failed
 * call is retried on the next launch). Older versions use the static previewLayout / previewImage.
 */
object WidgetPreviews {
    private const val PREFERENCES = "widget-previews"
    private val receivers: List<KClass<out GlanceAppWidgetReceiver>> =
        listOf(InboxWidgetReceiver::class, QuickActionsWidgetReceiver::class, NestsWidgetReceiver::class, AgendaWidgetReceiver::class)

    fun publish(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.VANILLA_ICE_CREAM) return
        val app = context.applicationContext
        val preferences = app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        if (preferences.getInt("version", -1) == BuildConfig.VERSION_CODE) return
        CoroutineScope(SupervisorJob() + Dispatchers.Default).launch {
            val manager = GlanceAppWidgetManager(app)
            val all = receivers.all { receiver ->
                runCatching { manager.setWidgetPreviews(receiver, intSetOf(AppWidgetProviderInfo.WIDGET_CATEGORY_HOME_SCREEN)) }
                    .getOrNull() == GlanceAppWidgetManager.SET_WIDGET_PREVIEWS_RESULT_SUCCESS
            }
            if (all) preferences.edit().putInt("version", BuildConfig.VERSION_CODE).apply()
        }
    }
}
