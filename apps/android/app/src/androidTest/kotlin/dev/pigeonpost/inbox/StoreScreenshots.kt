package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/**
 * Google Play screenshots from fixtures only (invented addresses, no account data). Skipped unless run with
 * `-Pandroid.testInstrumentationRunnerArguments.storeShots=<name>`; files land in the app's external files
 * directory under store/<name>, to be pulled with adb.
 */
@RunWith(AndroidJUnit4::class)
class StoreScreenshots {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private val prefix = InstrumentationRegistry.getArguments().getString("storeShots")

    private fun launch(look: String): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        return ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java)
            .putExtra("pigeonpost.fixtures", "store").putExtra("pigeonpost.appearance", look)).also { scenario ->
            ui.waitUntil(10_000) { var focused = false; scenario.onActivity { focused = it.hasWindowFocus() }; focused }
        }
    }
    private fun shown(text: String) = ui.waitUntil(10_000) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() }
    private fun shot(name: String) {
        ui.waitForIdle(); Thread.sleep(1200)
        val bitmap = InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot()
        val folder = File(context.getExternalFilesDir(null), "store/$prefix").apply { mkdirs() }
        File(folder, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }

    @Test fun capture() {
        assumeTrue(prefix != null)
        for (look in (InstrumentationRegistry.getArguments().getString("looks") ?: "dark,light").split(',')) launch(look).use {
            shown("my fleet")
            if (look == "dark") shot("1-$look-inbox")
            ui.onNodeWithText("my fleet").performClick()
            shown("Pinned it")
            shot("2-$look-conversation")
            if (look == "light") {
                ui.onAllNodesWithContentDescription("Back to conversations").fetchSemanticsNodes().firstOrNull()?.let { ui.onNodeWithContentDescription("Back to conversations").performClick() }
                ui.onNodeWithContentDescription("Settings").performClick()
                shown("Appearance")
                shot("3-$look-settings")
            }
        }
    }
}
