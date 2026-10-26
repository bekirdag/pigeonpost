package dev.pigeonpost.inbox

import android.app.Application
import android.content.Intent
import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.AvatarStore
import dev.pigeonpost.inbox.ui.Avatar
import dev.pigeonpost.inbox.ui.LocalAvatars
import dev.pigeonpost.inbox.ui.PigeonpostTheme
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AvatarAvailabilityUiTest {
    @get:Rule val ui = createAndroidComposeRule<ComponentActivity>()

    @Test fun anAlreadyVisibleFaceLoadsWhenAvatarCapabilityArrives() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val graph = Development.graph(app, Intent().putExtra("pigeonpost.fixtures", "safety"))!!
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val store = AvatarStore(graph.api, scope)
        val pictures = AvatarPictures(app, store)
        try {
            ui.setContent { PigeonpostTheme(false) {
                CompositionLocalProvider(LocalAvatars provides pictures) { Avatar("/demo/builder", "Build agent") }
            } }
            ui.onNodeWithTag("avatar:/demo/builder", useUnmergedTree = true).assertExists()
            ui.waitForIdle() // The initial lookup sees that capability discovery has not finished.
            ui.runOnIdle { store.enable(true) }
            ui.waitUntil(5_000) { ui.onAllNodesWithTag("avatar:/demo/builder:photo", useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty() }
        } finally { scope.cancel() }
    }
}
