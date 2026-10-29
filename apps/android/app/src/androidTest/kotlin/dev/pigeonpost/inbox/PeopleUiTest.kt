package dev.pigeonpost.inbox

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import android.os.SystemClock
import android.view.accessibility.AccessibilityNodeInfo
import dev.pigeonpost.core.*
import dev.pigeonpost.core.contacts.*
import dev.pigeonpost.inbox.ui.*
import kotlinx.coroutines.*
import org.junit.After
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** No real accounts, address-book rows or SMS providers are used. */
@RunWith(AndroidJUnit4::class)
class PeopleUiTest {
    @get:Rule val ui = createComposeRule()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val identity = PeopleIdentity("fixture", AuthorizationLease(), "/k/fixture", setOf(Features.PHONE_NUMBERS, Features.CONTACT_DISCOVERY, Features.SMS_INVITES, Features.DISPLAY_NAMES))
    private val api = Api()
    private val aliases = object : LocalAliases {
        override fun read(subject: String, mailbox: String) = emptyMap<String, String>()
        override fun write(subject: String, mailbox: String, values: Map<String, String>) {}
        override fun clear() {}
    }
    private val store = PeopleStore(api, scope, { identity }, aliases)
    private fun launch(mode: String) {
        store.synchronize()
        ui.setContent { PigeonpostTheme(false) { PageDialog("People", {}) { PeopleSettings(mode, store) {} } } }
    }
    @After fun cleanup() { identity.authorization.retire(); scope.cancel() }
    @Test fun permissionDisclosureCanBeCancelledWithoutImportOrSms() {
        launch("find")
        ui.onNodeWithTag("findPeople").performScrollTo().performClick()
        ui.onNodeWithText("Find your contacts on Pigeonpost?").assertIsDisplayed()
        ui.onNodeWithText("Cancel").performClick()
        ui.onNodeWithText("Find your contacts on Pigeonpost?").assertDoesNotExist()
        assertEquals(0, api.discoveries); assertEquals(0, api.invites)
    }
    @Test fun invitationNeedsSeparateConfirmationAndCancelDoesNotSend() {
        launch("find")
        ui.runOnIdle { store.discover(store.state.value.context, listOf(DeviceContact("+447700900123", "Fixture friend"))) }
        ui.waitUntil { store.state.value.checked.size == 1 }
        ui.onNodeWithText("Invite by SMS").performScrollTo().performClick()
        ui.onNodeWithText("Send one SMS invitation?").assertIsDisplayed(); assertEquals(0, api.invites)
        ui.onNodeWithText("Cancel").performClick(); assertEquals(0, api.invites)
        ui.onNodeWithText("Invite by SMS").performScrollTo().performClick()
        ui.onNodeWithText("Send invitation").performClick()
        ui.waitUntil { api.invites == 1 }
        ui.onNodeWithText("Invite by SMS").assertDoesNotExist()
    }
    @Test fun deniedSystemPermissionLeavesContactsPrivate() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val app = instrumentation.targetContext.packageName
        for (command in listOf("pm revoke $app android.permission.READ_CONTACTS", "pm clear-permission-flags $app android.permission.READ_CONTACTS user-set user-fixed")) {
            instrumentation.uiAutomation.executeShellCommand(command).use { result -> java.io.FileInputStream(result.fileDescriptor).use { it.readBytes() } }
        }
        launch("find")
        ui.onNodeWithTag("findPeople").performScrollTo().performClick()
        ui.onNodeWithText("Continue").performClick()
        val deadline = SystemClock.uptimeMillis() + 10_000
        var deny: AccessibilityNodeInfo? = null
        while (deny == null && SystemClock.uptimeMillis() < deadline) {
            val root = instrumentation.uiAutomation.rootInActiveWindow
            deny = sequenceOf("com.android.permissioncontroller", "com.google.android.permissioncontroller")
                .mapNotNull { root?.findAccessibilityNodeInfosByViewId("$it:id/permission_deny_button")?.firstOrNull() }.firstOrNull()
            if (deny == null) SystemClock.sleep(100)
        }
        assertNotNull("Android Contacts permission prompt was not shown", deny)
        assertTrue(deny!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
        ui.waitUntil { ui.onAllNodesWithText("Contact access was not allowed.", substring = true).fetchSemanticsNodes().isNotEmpty() }
        assertEquals(0, api.discoveries); assertEquals(0, api.invites); assertTrue(store.state.value.imported.isEmpty())
    }
    @Test fun phoneNormalizationUsesChosenCountryAndRejectsShortCodes() {
        assertEquals("+905321234567", PhoneContacts.normalize("0532 123 45 67", "TR"))
        assertEquals("+442079460123", PhoneContacts.normalize("020 7946 0123", "GB"))
        assertNull(PhoneContacts.normalize("112", "TR")); assertNull(PhoneContacts.normalize("0800-FLOWERS", "US"))
    }
    @Test fun displayNameIsFreeAndValidatesBeforeSending() {
        launch("name"); ui.waitUntil { store.state.value.nameLoaded }
        ui.onNodeWithTag("displayName").performScrollTo().performTextInput("/paid-handle")
        ui.onNodeWithText("Save name").performScrollTo().performClick(); assertEquals(0, api.names)
        ui.onNodeWithTag("displayName").performScrollTo().performTextReplacement("Çağrı")
        ui.onNodeWithText("Save name").performScrollTo().performClick()
        ui.waitUntil { api.names == 1 }; assertEquals("Çağrı", store.state.value.ownName?.name)
    }
    private class Api : PeopleApi {
        var invites = 0; var discoveries = 0; var names = 0
        private fun unused(): Nothing = error("Unexpected fixture call")
        override suspend fun phones(mailbox: String) = emptyList<VerifiedPhone>()
        override suspend fun sendCode(mailbox: String, phone: String): PhoneChallenge = unused()
        override suspend fun verify(mailbox: String, challenge: String, code: String, discoverable: Boolean): VerifiedPhone = unused()
        override suspend fun discoverable(mailbox: String, id: String, value: Boolean): VerifiedPhone = unused()
        override suspend fun unlink(mailbox: String, id: String) = unused()
        override suspend fun discover(mailbox: String, hashes: List<String>): DiscoveryResult { discoveries++; return DiscoveryResult(emptyList(), hashes.size, hashes.size, 100) }
        override suspend fun invite(mailbox: String, phone: String): InvitationResult { invites++; return InvitationResult("sent", id = "inv_fixture") }
        override suspend fun ownName(mailbox: String): ProfileName? = null
        override suspend fun saveName(mailbox: String, name: String, visibility: String): ProfileName { names++; return ProfileName(name, visibility, 1) }
        override suspend fun removeName(mailbox: String) = unused()
        override suspend fun profiles(mailbox: String, addresses: List<String>) = emptyMap<String, PeerProfile?>()
    }
}
