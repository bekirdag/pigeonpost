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

@RunWith(AndroidJUnit4::class)
class PolicyUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private fun launch(mode: String) = ActivityScenario.launch<MainActivity>(
        Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", mode))
    private fun shown(text: String) {
        ui.waitUntil(10000) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() }
    }
    private fun agree() {
        ui.onNodeWithText("I agree to the Terms of service").performScrollTo().performClick()
        ui.onNodeWithText("Agree and continue").performScrollTo().performClick()
        shown("Inbox")
    }

    @Test fun agreementIsRequiredAndRecreationCannotSkipIt() {
        launch("policy").use { scenario ->
            shown("Before you start")
            ui.onNodeWithText("Agree and continue").assertIsNotEnabled()
            ui.onNodeWithContentDescription("New conversation").assertDoesNotExist()
            ui.onNodeWithText("Terms of service").assertExists()
            ui.onNodeWithText("Privacy policy").assertExists()
            ui.onNodeWithText("Delete account").assertExists()
            ui.onNodeWithText("I agree to the Terms of service").performScrollTo().performClick()
            ui.onNodeWithText("Agree and continue").assertIsEnabled()
            scenario.recreate()
            shown("Before you start")
            ui.onNodeWithContentDescription("New conversation").assertDoesNotExist()
            ui.onNodeWithText("Agree and continue").assertIsEnabled().performScrollTo().performClick()
            shown("Inbox")
            ui.onNodeWithContentDescription("New conversation").assertExists()
        }
    }

    @Test fun policiesAreAccessibleBeforeSignInAndFreshSignInNeedsConsent() {
        launch("signin").use {
            shown("A direct line to your agents.")
            ui.onNodeWithText("Privacy policy").performScrollTo().assertIsDisplayed()
            ui.onNodeWithText("Terms of service").assertExists()
            ui.onNodeWithText("Support").assertExists()
            ui.onNodeWithText("Sign in").performScrollTo().performClick()
            shown("Before you start")
            agree()
            ui.onNodeWithContentDescription("Settings").performClick()
            ui.onNodeWithText("Sign out").performScrollTo().performClick()
            ui.onNodeWithText("Sign out").performClick()
            shown("A direct line to your agents.")
            ui.onNodeWithText("Sign in").performScrollTo().performClick()
            shown("Before you start")
            ui.onNodeWithText("Agree and continue").assertIsNotEnabled()
        }
    }

    @Test fun reportConfirmationPreservesMessageAndSenderCanBeBlocked() {
        launch("inbox").use {
            shown("Inbox")
            ui.onNodeWithText("demo/builder").performClick()
            shown("The Android build is ready for review.")
            ui.onAllNodesWithContentDescription("Message actions").onLast().performClick()
            ui.onNodeWithText("Report message").performClick()
            shown("Report this message?")
            ui.onNodeWithText("Cancel").performClick()
            shown("The Android build is ready for review.")
            ui.onAllNodesWithContentDescription("Message actions").onLast().performClick()
            ui.onNodeWithText("Report message").performClick()
            ui.onNodeWithText("Report", substring = false).performClick()
            shown("The Android build is ready for review.")
            ui.onNodeWithContentDescription("Conversation info").performClick()
            ui.onNodeWithText("Block sender").performScrollTo().performClick()
            ui.onNodeWithText("Confirm").performClick()
            shown("Unblock sender")
        }
    }
}
