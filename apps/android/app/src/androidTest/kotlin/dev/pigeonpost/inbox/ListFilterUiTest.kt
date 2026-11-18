package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** The quick filters above the conversation list (All · Unread · Agents · People), on the debug inbox fixture. */
@RunWith(AndroidJUnit4::class)
class ListFilterUiTest {
    @get:Rule val ui = createEmptyComposeRule()

    private val context get() = ApplicationProvider.getApplicationContext<Context>()

    private fun waitTag(tag: String) {
        ui.waitUntil(30_000) {
            ui.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun gone(tag: String) {
        ui.waitUntil(10_000) {
            ui.onAllNodesWithTag(tag).fetchSemanticsNodes().isEmpty()
        }
    }

    private fun launch(): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        val scene = ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java)
            .putExtra("pigeonpost.fixtures", "inbox").putExtra("pigeonpost.appearance", "light"))
        waitTag("conversation:/demo/builder")
        return scene
    }

    @Test
    fun agentsAndPeopleSplitTheListExactly() {
        launch().use {
            ui.onNodeWithTag("listFilter:agents").performClick()
            waitTag("conversation:/demo/builder")
            gone("conversation:/demo/reviewer")
            gone("conversation:/demo/docdex")

            ui.onNodeWithTag("listFilter:people").performClick()
            waitTag("conversation:/demo/reviewer")
            waitTag("conversation:/demo/docdex")
            gone("conversation:/demo/builder")

            ui.onNodeWithTag("listFilter:all").performClick()
            waitTag("conversation:/demo/builder")
            waitTag("conversation:/demo/reviewer")
            waitTag("conversation:/demo/docdex")
        }
    }

    @Test
    fun unreadKeepsOnlyUnreadConversations() {
        launch().use {
            ui.onNodeWithTag("listFilter:unread").performClick()
            waitTag("conversation:/demo/builder")
            waitTag("conversation:/demo/reviewer")
            gone("conversation:/demo/docdex")
        }
    }

    @Test
    fun theChosenFilterSurvivesRecreation() {
        launch().use { scenario ->
            ui.onNodeWithTag("listFilter:people").performClick()
            waitTag("listFilter:people")
            ui.onNodeWithTag("listFilter:people").assertIsSelected()
            gone("conversation:/demo/builder")

            scenario.recreate()
            waitTag("conversation:/demo/reviewer")
            ui.onNodeWithTag("listFilter:people").assertIsSelected()
            gone("conversation:/demo/builder")
        }
    }
}
