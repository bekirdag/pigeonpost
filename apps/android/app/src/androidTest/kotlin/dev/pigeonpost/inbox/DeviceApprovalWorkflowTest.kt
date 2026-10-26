package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Real native screens against explicit, in-memory debug fixtures. No browser or live decisions. */
@RunWith(AndroidJUnit4::class)
class DeviceApprovalWorkflowTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private val fixture get() = EnterpriseFixture.current!!
    private fun shown(text: String) { ui.waitUntil(10_000) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() } }
    private fun tag(value: String) = ui.onNodeWithTag("device:$value")
    private fun intent(mode: String, link: Boolean = true) = Intent(context, MainActivity::class.java).apply {
        putExtra("pigeonpost.fixtures", mode)
        if (link) { action = Intent.ACTION_VIEW; data = Uri.parse("https://inbox.pigeonpost.dev/pair.html#ABCD-1234") }
    }
    private fun launch(mode: String = "enterprise-ready", link: Boolean = true): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        val scenario = ActivityScenario.launch<MainActivity>(intent(mode, link))
        try {
            when {
                mode == "enterprise-signedout" -> shown("Continue with Google")
                link && mode != "enterprise-off" -> { shown("Sign in a machine"); shown("/example/build") }
                else -> shown("Inbox")
            }
            return scenario
        } catch (failure: Throwable) { scenario.close(); throw failure }
    }
    private fun confirm() {
        reviewAccessibly()
        shown("Approve this machine?")
        ui.onNodeWithText("Approve machine").performClick()
    }
    private fun reviewAccessibly() = tag("hold").performScrollTo().performSemanticsAction(SemanticsActions.OnClick) { it() }
    private fun noDecisions() { assertEquals(0, fixture.approvals.get()); assertEquals(0, fixture.denials.get()) }

    @Test fun settingsEntryValidatesBeforeLookingUpAndCancelDoesNotDecide() {
        launch(link = false).use {
            ui.onNodeWithContentDescription("Settings").performClick()
            shown("Sign in a machine"); ui.onNodeWithText("Sign in a machine").performScrollTo().performClick()
            tag("code").performTextInput("TOO-SHORT")
            tag("review").performClick(); ui.waitForIdle()
            assertEquals(0, fixture.resolves.get()); noDecisions()
            tag("code").performTextClearance(); tag("code").performTextInput("ABCD-1234")
            tag("review").performClick(); shown("/example/build")
            tag("number").assertTextEquals("123 456"); noDecisions()
            reviewAccessibly(); shown("Approve this machine?"); noDecisions()
            ui.onNodeWithText("Cancel").performClick(); noDecisions()
            tag("deny").performScrollTo().performClick(); shown("Request denied")
            assertEquals(1, fixture.denials.get()); assertEquals(0, fixture.approvals.get())
        }
    }

    @Test fun aLinkIsReadOnlyUntilExplicitHumanConfirmation() {
        launch().use {
            assertEquals(1, fixture.resolves.get()); noDecisions()
            reviewAccessibly(); shown("Approve this machine?"); noDecisions()
            ui.onNodeWithText("Approve machine").performClick(); shown("Machine approved")
            assertEquals(1, fixture.approvals.get()); assertEquals(0, fixture.denials.get())
        }
    }

    @Test fun verificationReloadsThePreviewAndRequiresAnotherExplicitApproval() {
        launch("enterprise").use {
            val old = fixture.session.grant!!
            confirm(); shown("Verify sign-in")
            assertEquals(1, fixture.approvals.get()); assertEquals(0, fixture.session.verifications.get())
            tag("verify").performScrollTo().performClick()
            ui.waitUntil(10_000) { ui.onAllNodesWithTag("device:hold").fetchSemanticsNodes().isNotEmpty() }
            assertFalse(old.isValid); assertNotSame(old, fixture.session.grant)
            assertEquals(1, fixture.session.verifications.get()); assertEquals(1, fixture.approvals.get())
            assertEquals(2, fixture.resolves.get())
            confirm(); shown("Machine approved"); assertEquals(2, fixture.approvals.get())
        }
    }

    @Test fun aNewAgentIsClearlyShownAndCreatedOnlyAfterConfirmation() {
        launch("enterprise-new-agent").use {
            shown("Approval will create this agent inbox."); assertFalse(fixture.agentCreated); noDecisions()
            reviewAccessibly(); shown("Approve this machine?")
            assertFalse(fixture.agentCreated); noDecisions()
            ui.onNodeWithText("Approve machine").performClick(); shown("Machine approved")
            assertTrue(fixture.agentCreated); assertEquals(1, fixture.approvals.get())
        }
    }

    @Test fun denialAcknowledgmentCannotHideAConcurrentApproval() {
        launch("enterprise-deny-race").use {
            tag("deny").performScrollTo().performClick(); shown("Machine approved")
            tag("denied").assertDoesNotExist()
            assertEquals(1, fixture.denials.get()); assertEquals(0, fixture.approvals.get())
        }
    }

    @Test fun aLostDecisionResponseSurvivesRotationAndOnlyChecksStatus() {
        launch("enterprise-unconfirmed").use { scenario ->
            confirm(); shown("Check current status"); assertEquals(1, fixture.approvals.get())
            scenario.recreate(); shown("Check current status")
            assertEquals(1, fixture.approvals.get()); assertEquals(1, fixture.resolves.get())
            tag("check").performScrollTo().performClick(); shown("Machine approved")
            assertEquals(1, fixture.approvals.get()); assertEquals(2, fixture.resolves.get())
        }
    }

    @Test fun missingDelegationDisablesBothDecisions() {
        launch("enterprise-no-permission").use {
            shown("Ask an agent admin to add you as an operator.")
            tag("hold").performScrollTo().assertIsNotEnabled()
            tag("deny").performScrollTo().assertIsNotEnabled(); noDecisions()
        }
    }

    @Test fun aServerWithoutEnterpriseSupportDoesNotResolveTheLinkOrOfferTheEntry() {
        launch("enterprise-off").use {
            ui.onNodeWithContentDescription("Settings").performClick(); shown("Appearance")
            ui.onNodeWithText("Sign in a machine").assertDoesNotExist()
            assertEquals(0, fixture.resolves.get()); noDecisions()
        }
    }

    @Test fun aPairLinkWaitsUntilAnUnsentCalendarDraftIsExplicitlyClosed() {
        launch().use { scenario ->
            ui.onNodeWithContentDescription("Close Sign in a machine").performClick()
            ui.waitUntil(10_000) { ui.onAllNodes(hasTestTag("tab:calendar") or hasTestTag("sidebar:calendar")).fetchSemanticsNodes().isNotEmpty() }
            ui.onNode(hasTestTag("tab:calendar") or hasTestTag("sidebar:calendar")).performClick()
            shown("Planning review"); ui.onNodeWithTag("calendar:new").performClick()
            ui.onNodeWithTag("calendar:title").performTextInput("Keep before the machine")
            scenario.onActivity { it.startActivity(intent("enterprise-ready").addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)) }
            shown("Another calendar item is waiting")
            ui.onNodeWithTag("calendar:title").assertTextContains("Keep before the machine"); noDecisions()
            ui.onNodeWithContentDescription("Close New event").performClick()
            shown("Discard this draft?"); ui.onNodeWithText("Discard draft").performClick()
            shown("Sign in a machine"); shown("/example/build"); noDecisions()
        }
    }

    @Test fun aColdLinkWaitsForSignInAndTermsBeforeLookingUp() {
        launch("enterprise-signedout").use {
            assertEquals(0, fixture.resolves.get()); noDecisions()
            ui.onNodeWithText("Sign in", useUnmergedTree = false).performClick()
            shown("Before you start"); assertEquals(0, fixture.resolves.get()); noDecisions()
            ui.onNodeWithText("I agree to the Terms of service").performScrollTo().performClick()
            ui.onNodeWithText("Agree and continue").performScrollTo().performClick()
            shown("Sign in a machine"); shown("/example/build")
            assertEquals(1, fixture.resolves.get()); noDecisions()
        }
    }

    @Test fun aShortPointerTapDoesNothingAndAFullHoldStillRequiresConfirmation() {
        launch().use {
            tag("hold").performScrollTo().performTouchInput { click() }
            ui.waitForIdle(); ui.onNodeWithText("Approve this machine?").assertDoesNotExist(); noDecisions()
            tag("hold").performTouchInput { longClick(durationMillis = 1600) }
            shown("Approve this machine?"); noDecisions()
            ui.onNodeWithText("Cancel").performClick(); noDecisions()
        }
    }

    @Test fun keyboardReviewStillNeedsExplicitHumanConfirmation() {
        launch().use {
            // Android starts in touch mode; a real navigation key enables button keyboard focus.
            tag("hold").performScrollTo()
            InstrumentationRegistry.getInstrumentation().sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_TAB)
            tag("hold").performScrollTo().performSemanticsAction(SemanticsActions.RequestFocus) { it() }
            tag("hold").assertIsFocused()
            InstrumentationRegistry.getInstrumentation().sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_ENTER)
            shown("Approve this machine?"); noDecisions()
            ui.onNodeWithText("Cancel").performClick(); noDecisions()
        }
    }
}
