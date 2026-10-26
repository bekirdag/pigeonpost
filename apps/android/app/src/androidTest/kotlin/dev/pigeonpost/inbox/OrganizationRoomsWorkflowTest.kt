package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.lifecycle.ViewModelProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Native UI and the actual MLS library; all people, rooms and requests are synthetic/in-memory. */
@RunWith(AndroidJUnit4::class)
class OrganizationRoomsWorkflowTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private val fixture get() = OrganizationRoomsFixture.current!!
    private lateinit var model: InboxViewModel
    private fun shown(text: String) {
        try { ui.waitUntil(15_000) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() } }
        catch (failure: ComposeTimeoutException) { throw AssertionError("Room screen unavailable. ${windowInputStatus()}", failure) }
    }
    private fun tag(value: String) = ui.onNodeWithTag("org:$value")
    private fun launch(mode: String = "org-rooms"): ActivityScenario<MainActivity> {
        context.getSharedPreferences("fixture-settings", Context.MODE_PRIVATE).edit().clear().commit()
        val intent = Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", mode).putExtra("pigeonpost.appearance", "light")
        val scenario = ActivityScenario.launch<MainActivity>(intent)
        try {
            awaitAndroidWindowFocus(ui) { var focused = false; scenario.onActivity { focused = it.hasWindowFocus() }; focused }
            scenario.onActivity { model = ViewModelProvider(it)[InboxViewModel::class.java] }
            ui.waitUntil(20_000) { model.groups?.state?.value?.let { it.enabled || it.unavailable } == true }
            assertFalse(model.groups!!.state.value.unavailable); shown("Inbox")
            return scenario
        } catch (failure: Throwable) { scenario.close(); throw failure }
    }
    private fun openSettingsEntry() {
        ui.onNodeWithContentDescription("Settings").performClick(); shown("Organization rooms")
        ui.onNode(hasText("Organization rooms") and hasAnyAncestor(isDialog())).performScrollTo().performClick()
        shown("Example")
    }
    private fun inspect(name: String) { tag("room:" + fixture.roomIds.getValue(name)).performScrollTo().performClick(); shown(name) }
    private fun opened(id: String) {
        ui.waitUntil(20_000) { !model.organizationRoomsOpen.value && model.groups!!.state.value.open == id }
        assertEquals("org_fixture", model.groups!!.state.value.current!!.orgId)
        ui.waitUntil(20_000) { id in model.groups!!.state.value.inGroup }
    }
    private fun noDecisions() { assertEquals(0, fixture.creates.get()); assertEquals(0, fixture.joins.get()); assertEquals(0, fixture.accepts.get()) }

    @Test fun dashboardDirectoryIsReadOnlyAndPrivateRoomsRequireAnInvitation() {
        launch().use {
            if (ui.onAllNodesWithTag("sidebar:organizationRooms").fetchSemanticsNodes().isNotEmpty()) ui.onNodeWithTag("sidebar:organizationRooms").performClick()
            else { ui.onNodeWithContentDescription("New conversation").performClick(); ui.onNodeWithTag("menu:organizationRooms").performClick() }
            shown("Example"); shown("Design room"); noDecisions()
            inspect("Private room"); tag("action").assertDoesNotExist(); shown("An invitation")
            noDecisions(); assertNull(model.groups!!.state.value.open)
        }
    }
    @Test fun joiningAnOrganizationRoomRechecksMembershipAndOpensRealMls() {
        launch().use {
            openSettingsEntry(); inspect("Design room"); noDecisions()
            tag("action").assert(hasText("Join room")).performScrollTo().performClick()
            opened(fixture.roomIds.getValue("Design room"))
            assertEquals(1, fixture.joins.get()); assertTrue(fixture.groupReads.get() >= 2); assertEquals(0, fixture.creates.get())
        }
    }
    @Test fun aPrivateInvitationUsesAcceptanceAndOpensRealMls() {
        launch().use {
            openSettingsEntry(); inspect("Finance room"); noDecisions()
            tag("action").assert(hasText("Accept invitation")).performScrollTo().performClick()
            opened(fixture.roomIds.getValue("Finance room"))
            assertEquals(1, fixture.accepts.get()); assertEquals(0, fixture.joins.get())
        }
    }
    @Test fun acceptedCreationWithALostResponseSurvivesRotationAndReusesTheExactBody() {
        launch("org-rooms-lost").use { scenario ->
            openSettingsEntry(); tag("new").performScrollTo().performClick()
            tag("name").performTextInput("Roadmap"); tag("description").performTextInput("Team planning")
            tag("create").performScrollTo().performClick(); shown("Creation is not confirmed")
            tag("name").assertIsNotEnabled(); assertEquals(1, fixture.creates.get())
            scenario.recreate(); shown("Review unconfirmed creation"); tag("resume").performScrollTo().performClick()
            tag("name").assertTextContains("Roadmap").assertIsNotEnabled(); assertEquals(1, fixture.creates.get())
            tag("retry").performScrollTo().performClick()
            try { ui.waitUntil(20_000) { !model.organizationRoomsOpen.value && model.groups!!.state.value.current?.name == "Roadmap" } }
            catch (failure: ComposeTimeoutException) {
                val rooms = model.organizationRooms!!.state.value; val groups = model.groups!!.state.value
                val created = fixture.fake.groups.values.filter { it.name == "Roadmap" }.map { "activation=${it.activation},mine=${it.members[fixture.me]?.state}" }
                throw AssertionError("Room page=${model.organizationRoomsOpen.value},preparing=${model.organizationRoomPreparing.value},pending=${rooms.pendingCreation != null},completed=${rooms.completedCreation != null},message=${rooms.message},groupOpen=${groups.open != null},groupNotice=${groups.notice},creates=${fixture.creates.get()},fake=$created", failure)
            }
            opened(model.groups!!.state.value.open!!)
            assertEquals(2, fixture.creates.get()); assertEquals(1, fixture.creationBodies.distinct().size)
            assertEquals(1, fixture.fake.groups.values.count { it.name == "Roadmap" })
        }
    }
    @Test fun anUnsentRoomDraftSurvivesRotationAndNeedsExplicitDiscard() {
        launch().use { scenario ->
            openSettingsEntry(); tag("new").performScrollTo().performClick(); tag("name").performTextInput("Keep this room")
            tag("description").performTextInput("Keep these notes")
            scenario.recreate(); shown("Continue room draft"); tag("resume").performScrollTo().performClick()
            tag("name").assertTextContains("Keep this room"); tag("description").assertTextContains("Keep these notes"); noDecisions()
            ui.onNodeWithContentDescription("Close New room").performClick(); shown("Discard this room draft?")
            ui.onNodeWithText("Cancel").performClick(); tag("name").assertTextContains("Keep this room")
            ui.onNodeWithContentDescription("Close New room").performClick(); ui.onNodeWithText("Discard draft").performClick()
            ui.waitUntil(10_000) { !model.organizationRoomsOpen.value }; noDecisions()
        }
    }
    @Test fun readonlyOrganizationsOfferNoCreationOrNewJoin() {
        launch("org-rooms-readonly").use {
            openSettingsEntry(); shown("This organization is read only."); tag("new").assertDoesNotExist()
            inspect("Design room"); shown("This organization is read only."); tag("action").assertDoesNotExist(); noDecisions()
        }
    }
    @Test fun absentOrganizationMembershipCannotOpenTheCachedGroupDirectory() {
        launch("org-rooms-no-membership").use {
            ui.onNodeWithContentDescription("Settings").performClick(); shown("Organization rooms")
            ui.onNode(hasText("Organization rooms") and hasAnyAncestor(isDialog())).performScrollTo().performClick(); shown("No organizations are available")
            tag("new").assertDoesNotExist(); assertEquals(0, fixture.directoryGets.get()); assertEquals(0, fixture.groupReads.get())
            assertNull(model.groups!!.state.value.open); noDecisions()
        }
    }
    @Test fun aServerWithoutOrganizationsOffersNoRoomEntryOrCatalogRequest() {
        launch("org-rooms-off").use {
            ui.onNodeWithContentDescription("Settings").performClick(); shown("Appearance")
            ui.onNodeWithText("Organization rooms").assertDoesNotExist(); assertEquals(0, fixture.catalogGets.get()); noDecisions()
        }
    }
    @Test fun revocationAtTheFinalInboxCheckNeverOpensCachedMessages() {
        launch("org-rooms-revoked").use {
            openSettingsEntry(); inspect("Design room"); tag("action").performScrollTo().performClick()
            shown("Access could not be confirmed")
            assertTrue(model.organizationRoomsOpen.value); assertNull(model.groups!!.state.value.open)
            assertNull(model.organizationRooms!!.state.value.preview); assertTrue(fixture.groupReads.get() >= 2)
        }
    }
    @Test fun selectingTheOrganizationInboxRequiresAnExplicitConfirmation() {
        launch().use { scenario ->
            val other = model.inbox.state.value.mailboxes.first { it.address != fixture.me }
            scenario.onActivity { model.inbox.switchMailbox(other) }
            ui.waitUntil(10_000) { model.inbox.state.value.loaded }
            openSettingsEntry(); shown("Choose your organization inbox")
            tag("switch:org_fixture").performScrollTo().performClick(); shown("Use this organization inbox?")
            assertEquals(other.address, model.inbox.state.value.acting?.address)
            ui.onNodeWithText("Cancel").performClick(); assertEquals(other.address, model.inbox.state.value.acting?.address)
            tag("switch:org_fixture").performScrollTo().performClick(); ui.onNodeWithText("Use inbox").performClick()
            shown("Design room"); assertEquals(fixture.me, model.inbox.state.value.acting?.address); noDecisions()
        }
    }
}
