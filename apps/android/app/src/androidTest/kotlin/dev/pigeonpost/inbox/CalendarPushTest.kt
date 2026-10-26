package dev.pigeonpost.inbox

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.SdkSuppress
import androidx.test.rule.GrantPermissionRule
import dev.pigeonpost.inbox.push.PushNotifications
import dev.pigeonpost.inbox.push.displayPush
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
@SdkSuppress(minSdkVersion = 33)
class CalendarPushTest {
    @get:Rule val permission: GrantPermissionRule = GrantPermissionRule.grant(Manifest.permission.POST_NOTIFICATIONS)
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private val manager get() = context.getSystemService(NotificationManager::class.java)
    private val payload = mapOf("type" to "calendar", "identity" to "/k/demomain", "mailbox" to "/display/only", "peer" to "/k/organizer",
        "message_id" to "cal:synthetic-native-test:reminder", "calendar_event_id" to "evt_review", "calendar_occurrence_id" to "occ_original", "title" to "Private synthetic title")
    private fun isolated(work: () -> Unit) {
        val preferences = context.getSharedPreferences("push", Context.MODE_PRIVATE)
        val identity = preferences.getString("identity", null)
        val foreground = PushNotifications.foreground
        val poke = PushNotifications.calendarPoke
        try {
            preferences.edit().putString("identity", "/k/demomain").commit()
            PushNotifications.foreground = false; PushNotifications.calendarPoke = null
            PushNotifications.channel(context)
            work()
        } finally {
            manager.activeNotifications.filter { it.tag?.startsWith("calendar:") == true }.forEach { manager.cancel(it.tag, it.id) }
            preferences.edit().apply { if (identity == null) remove("identity") else putString("identity", identity) }.commit()
            PushNotifications.foreground = foreground; PushNotifications.calendarPoke = poke
        }
    }
    @Test fun calendarNotificationUsesEventCategoryAndRedactsItsPublicLockscreenVersion() = isolated {
        displayPush(context, payload)
        val deadline = android.os.SystemClock.elapsedRealtime() + 5_000
        while (manager.activeNotifications.none { it.tag?.startsWith("calendar:") == true } && android.os.SystemClock.elapsedRealtime() < deadline) android.os.SystemClock.sleep(20)
        val item = manager.activeNotifications.single { it.tag?.startsWith("calendar:") == true }.notification
        assertEquals(Notification.CATEGORY_EVENT, item.category)
        assertEquals(Notification.VISIBILITY_PRIVATE, item.visibility)
        assertEquals("Private synthetic title", item.extras.getString(Notification.EXTRA_TITLE))
        assertEquals("Calendar reminder", item.extras.getString(Notification.EXTRA_TEXT))
        assertEquals("Pigeonpost", item.publicVersion.extras.getString(Notification.EXTRA_TITLE))
        assertEquals("Calendar update", item.publicVersion.extras.getString(Notification.EXTRA_TEXT))
        assertTrue(item.contentIntent.isImmutable)
        assertFalse(item.extras.containsKey(Notification.EXTRA_PEOPLE_LIST))
    }
    @Test fun staleRecipientsAreIgnoredAndForegroundCalendarPushesOnlyRefresh() = isolated {
        var pokes = 0
        PushNotifications.foreground = true; PushNotifications.calendarPoke = { pokes++ }
        displayPush(context, payload + ("identity" to "/k/another"))
        displayPush(context, payload + ("identity" to "/handle"))
        assertEquals(0, pokes)
        displayPush(context, payload)
        assertEquals(1, pokes)
        assertTrue(manager.activeNotifications.none { it.tag?.startsWith("calendar:") == true })
    }
}
