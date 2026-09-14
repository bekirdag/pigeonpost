package dev.pigeonpost.inbox

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import androidx.work.WorkManager
import com.google.android.gms.tasks.Tasks
import com.google.firebase.messaging.FirebaseMessaging
import dev.pigeonpost.inbox.push.PushNotifications
import dev.pigeonpost.inbox.push.displayPush
import org.junit.After
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.concurrent.TimeUnit

@RunWith(AndroidJUnit4::class)
class PushNotificationTest {
    @get:Rule val permission = GrantPermissionRule.grant(Manifest.permission.POST_NOTIFICATIONS)
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private val manager get() = context.getSystemService(NotificationManager::class.java)
    private val preferences get() = context.getSharedPreferences("push", Context.MODE_PRIVATE)
    private val payload = mapOf("identity" to "/k/push-test", "peer" to "/bekir/main",
        "message_id" to "push-test", "mailbox" to "/alp/main")

    @Before fun prepare() {
        preferences.edit().clear().commit()
        PushNotifications.foreground = false
        PushNotifications.channel(context)
        manager.cancelAll()
    }

    @After fun cleanUp() {
        PushNotifications(context).clear()
        File(context.cacheDir, "fcm-test-token").delete()
        PushNotifications.foreground = false
    }

    private fun awaitNotification(id: String, seconds: Int = 3): Notification {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(seconds.toLong())
        while (System.nanoTime() < deadline) {
            manager.activeNotifications.firstOrNull { it.tag == id }?.let { return it.notification }
            Thread.sleep(30)
        }
        throw AssertionError("Notification did not arrive: $id")
    }

    @Test fun backgroundAlertUsesNamespaceAndKeepsLockScreenGeneric() {
        preferences.edit().putString("identity", payload.getValue("identity")).commit()
        displayPush(context, payload + ("body" to "Private text must never be displayed"))
        val notification = awaitNotification("push-test")
        assertEquals("/bekir", notification.extras.getCharSequence(Notification.EXTRA_TITLE).toString())
        assertEquals("New message in /alp", notification.extras.getCharSequence(Notification.EXTRA_TEXT).toString())
        assertEquals(Notification.VISIBILITY_PRIVATE, notification.visibility)
        assertEquals("Pigeonpost", notification.publicVersion.extras.getCharSequence(Notification.EXTRA_TITLE).toString())
        assertEquals("You have a new message.", notification.publicVersion.extras.getCharSequence(Notification.EXTRA_TEXT).toString())
        assertNotNull(notification.contentIntent)
    }

    @Test fun signedOutOtherInboxAndForegroundMessagesCannotDisplay() {
        displayPush(context, payload)
        preferences.edit().putString("identity", "/k/someone-else").commit()
        displayPush(context, payload + ("message_id" to "other-account"))
        preferences.edit().putString("identity", payload.getValue("identity")).commit()
        PushNotifications.foreground = true
        displayPush(context, payload + ("message_id" to "foreground"))
        Thread.sleep(100)
        assertTrue(manager.activeNotifications.isEmpty())
        PushNotifications.foreground = false
        displayPush(context, payload)
        awaitNotification("push-test")
        PushNotifications(context).clear()
        displayPush(context, payload + ("message_id" to "after-signout"))
        Thread.sleep(100)
        assertTrue(manager.activeNotifications.isEmpty())
    }

    /** Opt-in operator check: send an FCM data message to the token in the app's private cache. */
    @Test fun liveFcmDelivery() {
        assumeTrue(InstrumentationRegistry.getArguments().getString("pigeonpost.livePush") == "true")
        val token = Tasks.await(FirebaseMessaging.getInstance().token, 30, TimeUnit.SECONDS)
        // This fixture is signed out. Let the asynchronous onNewToken callback
        // finish its account check before opening the fixture's local display gate.
        Thread.sleep(500)
        val work = WorkManager.getInstance(context)
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (work.getWorkInfosForUniqueWork(PushNotifications.WORK).get().any { !it.state.isFinished }
            && System.nanoTime() < deadline) Thread.sleep(30)
        preferences.edit().putString("identity", payload.getValue("identity")).commit()
        File(context.cacheDir, "fcm-test-token").writeText(token)
        val notification = awaitNotification("live-push-test", 90)
        assertEquals("/bekir", notification.extras.getCharSequence(Notification.EXTRA_TITLE).toString())
        assertEquals("New message in /alp", notification.extras.getCharSequence(Notification.EXTRA_TEXT).toString())
    }
}
