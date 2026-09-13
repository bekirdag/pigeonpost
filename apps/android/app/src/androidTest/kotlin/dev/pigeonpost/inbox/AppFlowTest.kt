package dev.pigeonpost.inbox

import android.app.Application
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.core.content.FileProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.MAX_ATTACHMENT_BYTES
import dev.pigeonpost.inbox.auth.SecureStore
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.io.RandomAccessFile

@RunWith(AndroidJUnit4::class)
class AppFlowTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private fun launch(mode: String = "inbox") = ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", mode))
    private fun shown(text: String) {
        ui.waitUntil(10000) { ui.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty() }
    }
    @Test fun conversationsSubjectsComposeFindAndBack() {
        launch().use { scenario ->
            shown("Inbox")
            ui.onNodeWithText("demo/builder").performClick()
            shown("The Android build is ready for review.")
            ui.onNodeWithText("Message").performTextInput("Hello from Android")
            ui.onNodeWithContentDescription("Send message").performClick()
            shown("Hello from Android")
            ui.onNodeWithContentDescription("New subject").performClick()
            shown("New subject")
            ui.onNodeWithText("Subject").performTextInput("Release checklist")
            ui.onNodeWithText("Create").performClick()
            ui.waitUntil(10000) { ui.onAllNodesWithText("New subject").fetchSemanticsNodes().isEmpty() }
            shown("Release checklist")
            ui.onNodeWithText("Message").performTextInput("A separate subject draft")
            ui.onNodeWithText("Android build").performScrollTo().performClick()
            ui.onNodeWithText("A separate subject draft").assertDoesNotExist()
            ui.onNodeWithText("Release checklist").performScrollTo().performClick()
            ui.onNodeWithText("A separate subject draft").assertExists()
            ui.onNodeWithContentDescription("Find in conversation").performClick()
            ui.onNodeWithText("Find in this subject").performTextInput("checklist")
            scenario.onActivity { it.onBackPressedDispatcher.onBackPressed() }
            shown("Inbox")
        }
    }
    @Test fun settingsContactsArchiveAndSignOut() {
        launch().use {
            shown("Inbox")
            ui.onNodeWithContentDescription("Settings").performClick()
            shown("Storage")
            ui.onNodeWithText("Contacts and permissions").performScrollTo().performClick()
            ui.onNodeWithText("Add contact").performClick()
            ui.onNodeWithText("Address or /namespace/*").performTextInput("/demo/new-agent")
            ui.onNodeWithText("Display name (optional)").performTextInput("New agent")
            ui.onNodeWithText("Save contact").performScrollTo().performClick()
            shown("New agent")
            ui.onNodeWithContentDescription("Close Contacts and permissions").performClick()
            ui.onNodeWithContentDescription("Settings").performClick()
            ui.onNodeWithText("Archived conversations").performScrollTo().performClick()
            shown("Archived agent")
            ui.onNodeWithContentDescription("Back to inbox").performClick()
            ui.onNodeWithContentDescription("Settings").performClick()
            ui.onNodeWithText("Sign out").performScrollTo().performClick()
            ui.onNodeWithText("Sign out").performClick()
            shown("A direct line to your agents.")
        }
    }
    @Test fun accountHandlesShowAppleAndGoogleOwnershipWithExpiredStatus() {
        launch().use {
            shown("Inbox")
            ui.onNodeWithContentDescription("Settings").performClick()
            shown("Your handles")
            ui.onNodeWithText("Active · App Store").performScrollTo().assertIsDisplayed()
            ui.onNodeWithText("Active · Google Play").performScrollTo().assertIsDisplayed()
            ui.onNodeWithText("Expired · Google Play").performScrollTo().assertIsDisplayed()
            ui.onNodeWithText("Open /previous").assertDoesNotExist()
            ui.onNodeWithText("Refresh account handles").performScrollTo().performClick()
            shown("Expired · Google Play")
        }
    }
    @Test fun firstInboxAndOfflineStatesRemainUsable() {
        launch("empty").use {
            shown("Your first inbox")
            ui.onNodeWithText("Create inbox").performClick()
            shown("Inbox")
        }
        launch("offline").use {
            shown("Offline")
            ui.onNodeWithText("Refresh").assertExists()
            ui.onNodeWithContentDescription("Settings").performClick()
            shown("Settings")
        }
    }
    @Test fun mailboxSwitchAndServerPermissions() {
        launch().use {
            shown("Inbox")
            ui.onNodeWithText("demo/builder").performClick()
            ui.onNodeWithContentDescription("Conversation info").performClick()
            ui.onNodeWithText("Choose permissions").performScrollTo().performClick()
            shown("Allow selected requests automatically")
            ui.onNodeWithText("run shell").assertDoesNotExist()
            ui.onNodeWithText("run tests").assertExists()
            ui.onNodeWithContentDescription("Close Edit contact").performClick()
            ui.onNodeWithContentDescription("Close Contacts and permissions").performClick()
            ui.onNodeWithContentDescription("Back to conversations").performClick()
            ui.onNodeWithText("Inbox").performClick()
            ui.onNodeWithText("/demo/builder").performClick()
            shown("No conversations yet")
        }
    }
    @Test fun approvedTesterRegistersAFreeHandleAndOpensItsInbox() {
        launch("handles").use {
            shown("Inbox")
            ui.onNodeWithContentDescription("Settings").performClick()
            val registrationField = hasSetTextAction() and hasAnyAncestor(isDialog())
            ui.waitUntil(10000) { ui.onAllNodes(registrationField).fetchSemanticsNodes().isNotEmpty() }
            // Material does not lay out an offscreen text-field label on every device size.
            ui.onNode(registrationField).performScrollTo().performTextInput("support")
            ui.onNodeWithText("Check availability").performScrollTo().performClick()
            shown("That name is reserved")
            ui.onNodeWithText("Handle name").performTextReplacement("alex")
            ui.onNodeWithText("Check availability").performScrollTo().performClick()
            shown("/alex is available")
            ui.onNodeWithText("Register for free").performScrollTo().performClick()
            ui.onNodeWithText("Register handle").performClick()
            shown("/alex/main")
            shown("Free preview handle. No payment or automatic renewal.")
            ui.onNodeWithText("Open this inbox").performScrollTo().performClick()
            shown("No conversations yet")
            ui.onNodeWithText("Inbox").performClick()
            shown("/alex/main")
        }
    }
    @Test fun unapprovedAccountCannotStartRegistration() {
        launch().use {
            shown("Inbox")
            ui.onNodeWithContentDescription("Settings").performClick()
            shown("Free handle registration is available to approved testers")
            ui.onNodeWithText("Handle name").assertDoesNotExist()
            ui.onNodeWithText("Register for free").assertDoesNotExist()
        }
    }
    @Test fun encryptedSessionAndAttachmentsUsePrivateStorage() = runBlocking {
        val root = File(context.cacheDir, "storage-test").apply { mkdirs() }
        try {
            val isolated = object : ContextWrapper(context) { override fun getNoBackupFilesDir() = root }
            val storage = SecureStore(isolated)
            storage.write("a-test-refresh-token")
            assertEquals("a-test-refresh-token", storage.read())
            assertFalse(File(root, "session.enc").readBytes().toString(Charsets.ISO_8859_1).contains("a-test-refresh-token"))
            storage.write(null); assertNull(storage.read())
            val graph = Development.graph(context.applicationContext as Application, Intent().putExtra("pigeonpost.fixtures", "inbox"))!!
            val files = PlatformFiles(context, graph.api)
            val source = File(context.cacheDir, "received/test/source.txt").apply { parentFile!!.mkdirs(); writeText("A native attachment") }
            val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", source)
            val staged = files.stage(listOf(uri)).single()
            assertEquals("A native attachment", staged.file.readText())
            assertTrue(staged.file.canonicalPath.startsWith(File(context.cacheDir, "staged").canonicalPath + "/"))
            staged.file.delete()
            RandomAccessFile(source, "rw").use { it.setLength(MAX_ATTACHMENT_BYTES + 1) }
            try { files.stage(listOf(uri)); fail("Expected size rejection") } catch (_: IllegalArgumentException) {}
            source.delete(); source.parentFile!!.delete()
            assertEquals("evil_file.txt", safeFilename("../../evil:file.txt"))
        } finally { root.deleteRecursively() }
    }
}
