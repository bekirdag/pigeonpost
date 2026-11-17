package dev.pigeonpost.inbox

import android.content.Context
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.Shader
import android.view.View
import android.widget.FrameLayout
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.compose
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.pigeonpost.core.widgets.WidgetSnapshot
import dev.pigeonpost.inbox.widgets.AgendaWidget
import dev.pigeonpost.inbox.widgets.InboxWidget
import dev.pigeonpost.inbox.widgets.NestsWidget
import dev.pigeonpost.inbox.widgets.QuickActionsWidget
import dev.pigeonpost.inbox.widgets.WidgetSamples
import dev.pigeonpost.inbox.widgets.WidgetStore
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * Every widget, at every size it declares, rendered by the system's RemoteViews on this device in
 * light and dark: the same inflation a launcher does. With
 * `-Pandroid.testInstrumentationRunnerArguments.widgetShots=<name>` it also writes PNGs (single
 * widgets and a home-screen board) under the app's external files directory, widgets/<name>, for
 * the PR and the store listing. Sample data only.
 */
@RunWith(AndroidJUnit4::class)
class WidgetRenderTest {
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private val shots = InstrumentationRegistry.getArguments().getString("widgetShots")
    private val density get() = context.resources.displayMetrics.density

    private val widgets: List<Pair<String, Pair<GlanceAppWidget, List<DpSize>>>> = listOf(
        "inbox" to (InboxWidget() to InboxWidget.SIZES.toList()),
        "quick" to (QuickActionsWidget() to listOf(QuickActionsWidget.ONE, QuickActionsWidget.TWO)),
        "nests" to (NestsWidget() to listOf(NestsWidget.SQUARE, NestsWidget.MEDIUM, NestsWidget.LARGE, NestsWidget.TABLET)),
        "agenda" to (AgendaWidget() to listOf(AgendaWidget.SQUARE, AgendaWidget.MEDIUM, AgendaWidget.LARGE, AgendaWidget.TABLET)),
    )

    @After fun forget() = runBlocking { WidgetStore.clear(context); Unit }

    private fun themed(dark: Boolean): Context {
        val config = Configuration(context.resources.configuration)
        config.uiMode = (config.uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or (if (dark) Configuration.UI_MODE_NIGHT_YES else Configuration.UI_MODE_NIGHT_NO)
        return context.createConfigurationContext(config)
    }

    /** Compose with Glance, then inflate and lay out the RemoteViews exactly as a launcher would. */
    private fun render(widget: GlanceAppWidget, size: DpSize, dark: Boolean, drawSize: DpSize = size): Bitmap {
        val themed = themed(dark)
        val views = runBlocking { widget.compose(themed, size = size) }
        val width = (drawSize.width.value * density).toInt(); val height = (drawSize.height.value * density).toInt()
        var bitmap: Bitmap? = null
        // A widget list only fills its rows once attached to a window, as on a launcher: host it in one.
        androidx.test.core.app.ActivityScenario.launch(androidx.activity.ComponentActivity::class.java).use { scenario ->
            var parent: FrameLayout? = null
            scenario.onActivity { activity ->
                val root = FrameLayout(themed)
                // Lists only bind inside an AppWidgetHostView, exactly as on a launcher.
                val holder = android.appwidget.AppWidgetHostView(themed).apply { setPadding(0, 0, 0, 0); updateAppWidget(views) }
                root.addView(holder, FrameLayout.LayoutParams(width, height))
                activity.setContentView(root)
                parent = holder
            }
            InstrumentationRegistry.getInstrumentation().waitForIdleSync()
            Thread.sleep(300)
            InstrumentationRegistry.getInstrumentation().waitForIdleSync()
            scenario.onActivity {
                val holder = parent!!
                val spec = { px: Int -> View.MeasureSpec.makeMeasureSpec(px, View.MeasureSpec.EXACTLY) }
                holder.measure(spec(width), spec(height)); holder.layout(0, 0, width, height)
                // A still picture taken right after layout would catch the list's fading scrollbar.
                fun quiet(v: View) { v.isVerticalScrollBarEnabled = false; (v as? android.view.ViewGroup)?.let { g -> for (i in 0 until g.childCount) quiet(g.getChildAt(i)) } }
                quiet(holder)
                bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888).also { holder.draw(Canvas(it)) }
            }
        }
        return bitmap!!
    }

    private fun seed(snapshot: WidgetSnapshot?) = runBlocking { if (snapshot == null) WidgetStore.clear(context) else WidgetStore.write(context, snapshot) }

    @Test fun everyWidgetRendersAtEverySizeInBothThemes() {
        for (state in listOf(WidgetSamples.snapshot(System.currentTimeMillis() / 1000), WidgetSamples.empty(System.currentTimeMillis() / 1000), null)) {
            seed(state)
            // Both themes for the full state; the empty and signed-out states once, to keep the CI leg short.
            for ((name, entry) in widgets) for (size in entry.second) for (dark in if (state?.conversations?.isNotEmpty() == true) listOf(false, true) else listOf(false)) {
                val bitmap = render(entry.first, size, dark)
                // Not blank: the card and its content painted something other than one colour.
                val first = bitmap.getPixel(bitmap.width / 2, bitmap.height / 2)
                var varied = false
                loop@ for (x in 0 until bitmap.width step 4) for (y in 0 until bitmap.height step 4) if (bitmap.getPixel(x, y) != first) { varied = true; break@loop }
                assertTrue("$name at $size (dark=$dark, state=${state != null}) drew nothing", varied)
            }
        }
    }

    // ---- screenshots (opt-in) -----------------------------------------------------------------------

    private fun save(bitmap: Bitmap, name: String) {
        val folder = File(context.getExternalFilesDir(null), "widgets/$shots").apply { mkdirs() }
        File(folder, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }

    private data class Place(val widget: GlanceAppWidget, val size: DpSize, val x: Float, val y: Float)

    /** A plain wallpaper with the widgets where a launcher would put them. */
    private fun board(width: Float, height: Float, dark: Boolean, places: List<Place>): Bitmap {
        val bitmap = Bitmap.createBitmap((width * density).toInt(), (height * density).toInt(), Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        val top = if (dark) 0xFF0B1730.toInt() else 0xFFB9CCF2.toInt(); val bottom = if (dark) 0xFF1B1030.toInt() else 0xFFE9DDF7.toInt()
        canvas.drawRect(0f, 0f, bitmap.width.toFloat(), bitmap.height.toFloat(),
            Paint().apply { shader = LinearGradient(0f, 0f, 0f, bitmap.height.toFloat(), top, bottom, Shader.TileMode.CLAMP) })
        for (place in places) canvas.drawBitmap(render(place.widget, place.size, dark), place.x * density, place.y * density, null)
        return bitmap
    }

    @Test fun capture() {
        assumeTrue(shots != null)
        seed(WidgetSamples.snapshot(System.currentTimeMillis() / 1000))
        val tablet = context.resources.configuration.smallestScreenWidthDp >= 600
        for (dark in listOf(false, true)) {
            val look = if (dark) "dark" else "light"
            for ((name, entry) in widgets) for (size in entry.second) save(render(entry.first, size, dark), "$name-${size.width.value.toInt()}x${size.height.value.toInt()}-$look")
            val places = if (!tablet) listOf(
                Place(InboxWidget(), DpSize(380.dp, 250.dp), 16f, 24f),
                Place(QuickActionsWidget(), DpSize(182.dp, 70.dp), 16f, 290f),
                Place(InboxWidget(), DpSize(182.dp, 70.dp), 214f, 290f),
                Place(AgendaWidget(), DpSize(380.dp, 200.dp), 16f, 376f),
                Place(NestsWidget(), DpSize(380.dp, 180.dp), 16f, 592f),
            ) else listOf(
                Place(InboxWidget(), DpSize(460.dp, 340.dp), 24f, 32f),
                Place(AgendaWidget(), DpSize(420.dp, 340.dp), 504f, 32f),
                Place(NestsWidget(), DpSize(300.dp, 340.dp), 944f, 32f),
                Place(QuickActionsWidget(), DpSize(150.dp, 70.dp), 24f, 392f),
                Place(InboxWidget(), DpSize(260.dp, 70.dp), 194f, 392f),
                Place(InboxWidget(), DpSize(170.dp, 170.dp), 474f, 392f),
                Place(AgendaWidget(), DpSize(170.dp, 170.dp), 664f, 392f),
                Place(NestsWidget(), DpSize(170.dp, 170.dp), 854f, 392f),
            )
            save(if (!tablet) board(412f, 800f, dark, places) else board(1268f, 600f, dark, places), "${if (tablet) "tablet" else "phone"}-home-$look")
        }
    }
}
