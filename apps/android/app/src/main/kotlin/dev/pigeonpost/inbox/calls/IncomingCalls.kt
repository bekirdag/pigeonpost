//  Incoming calls while the app is closed, in the background, or behind the lock screen
//  (docs/call-alerting.md, Phase 1c).
//
//  A ring reaches this device two ways: an FCM data message (`call_ring`, which wakes a killed app),
//  and, while the app's process is alive, the call page's own event stream (`ring` over the bridge).
//  Both land here, keyed by room, so the two never show twice. In the background a ring is a
//  CallStyle notification on the `calls` channel: the ringtone loops until the ring ends, the full-
//  screen intent puts IncomingCallActivity over the lock screen, and Answer/Decline act at once.
//  With the app on screen nothing is posted: the call page's own ring screen answers.
//
//  The ringing is the notification's sound, so taking the notification down is what stops it:
//  answer, decline, `call_cancel`, the page's `ring_ended`, the ring's expiry, or sign-out.
package dev.pigeonpost.inbox.calls

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.content.res.ResourcesCompat
import androidx.core.graphics.drawable.IconCompat
import androidx.compose.ui.graphics.toArgb
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import dev.pigeonpost.core.PeerFace
import dev.pigeonpost.inbox.ui.avatarTone
import dev.pigeonpost.core.CallRings
import dev.pigeonpost.core.RingClient
import dev.pigeonpost.core.displayName
import dev.pigeonpost.core.validAddress
import dev.pigeonpost.inbox.Development
import dev.pigeonpost.inbox.MainActivity
import dev.pigeonpost.inbox.R
import dev.pigeonpost.inbox.push.PushNotifications
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import java.lang.ref.WeakReference

/** One ringing call as this device knows it. `ringToken` is null for a ring only the page has seen. */
data class IncomingRing(
    val room: String, val ringToken: String?, val kind: String, val callerName: String, val callerDetail: String?,
    val callerAddress: String, val identity: String, val expiresAt: Long,
    /** A notification is up for it. */
    val posted: Boolean = false,
    /** The call page has it too (its own ring screen can take over in the foreground). */
    val seenByPage: Boolean = false,
) {
    val video get() = kind == "video"
}

object IncomingCalls {
    const val CHANNEL = "calls"
    const val ACTION_ANSWER = "dev.pigeonpost.inbox.calls.ANSWER"
    const val ACTION_DECLINE = "dev.pigeonpost.inbox.calls.DECLINE"
    const val EXTRA_ROOM = "pigeonpost.ring.room"
    const val EXTRA_TOKEN = "pigeonpost.ring.token"
    const val EXTRA_KIND = "pigeonpost.ring.kind"
    const val EXTRA_NAME = "pigeonpost.ring.name"
    const val EXTRA_DETAIL = "pigeonpost.ring.detail"
    const val EXTRA_ADDRESS = "pigeonpost.ring.address"
    const val EXTRA_IDENTITY = "pigeonpost.ring.identity"
    const val EXTRA_EXPIRES = "pigeonpost.ring.expires"
    private const val NOTIFICATION_ID = 2
    private val VIBRATION = longArrayOf(0, 900, 700, 900, 700)

    private val live = MutableStateFlow<Map<String, IncomingRing>>(emptyMap())
    /** Rings still ringing, by room. */
    val rings: StateFlow<Map<String, IncomingRing>> = live.asStateFlow()
    /** Why each recently ended ring ended, for the incoming-call screen's last words. */
    val endings = MutableStateFlow<Map<String, String>>(emptyMap())
    private val main = Handler(Looper.getMainLooper())
    /** Declines outlive the screen that asked for them. */
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    /** The live call page, when the app's process has one. */
    @Volatile var host: WeakReference<CallHost>? = null

    fun tag(room: String) = "call-$room"

    /** The mailbox this device rings for: the one push is registered for. */
    fun identity(context: Context): String? =
        context.getSharedPreferences("push", Context.MODE_PRIVATE).getString("identity", null) ?: Development.callIdentity(context)

    /** Where `/calls/v1` lives: beside the call page. */
    fun origin(context: Context): String = Development.callsOrigin(context) ?: CallHost.PRODUCTION_ORIGIN

    fun channel(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL) != null) return
        manager.createNotificationChannel(NotificationChannel(CHANNEL, "Calls", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "Incoming voice and video calls"
            lockscreenVisibility = Notification.VISIBILITY_PUBLIC
            setSound(RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE), AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE).setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION).build())
            enableVibration(true)
            vibrationPattern = VIBRATION
            setBypassDnd(false)
        })
    }

    fun allowed(context: Context): Boolean {
        channel(context)
        return NotificationManagerCompat.from(context).areNotificationsEnabled() &&
            context.getSystemService(NotificationManager::class.java).getNotificationChannel(CHANNEL).importance != NotificationManager.IMPORTANCE_NONE
    }

    /** Android 14 lets people (and Play policy) turn full-screen call screens off per app. */
    fun canUseFullScreen(context: Context): Boolean =
        Build.VERSION.SDK_INT < 34 || context.getSystemService(NotificationManager::class.java).canUseFullScreenIntent()

    /** An FCM data message. True when it was about a call (shown or not), so mail handling skips it. */
    fun push(context: Context, data: Map<String, String>): Boolean {
        val type = data["type"]
        if (type != "call_ring" && type != "call_cancel") return false
        val me = identity(context)
        if (type == "call_cancel") {
            CallRings.cancel(data, me)?.let { end(context, it.room, it.reason) }
            return true
        }
        val ring = CallRings.ring(data, me, System.currentTimeMillis() / 1000) ?: return true
        ringing(context, IncomingRing(ring.room, ring.ringToken, ring.kind, ring.callerName, ring.callerDetail, ring.fromAddress, ring.identity, ring.expiresAt))
        return true
    }

    /** The page's event stream rang (`{type:"ring"}`): same ring, no token. */
    fun fromPage(context: Context, room: String, kind: String, fromAddress: String, fromHandle: String?, identity: String) {
        if (!CallRings.ROOM.matches(room) || !validAddress(fromAddress)) return
        val handle = fromHandle?.takeIf { validAddress(it) }
        val name = handle?.let { "/" + displayName(it).removePrefix("/") } ?: displayName(fromAddress)
        ringing(context, IncomingRing(room, null, if (kind == "video") "video" else "audio", name, if (handle == null) fromAddress else null,
            fromAddress, identity, System.currentTimeMillis() / 1000 + 45, seenByPage = true))
    }

    private fun ringing(context: Context, ring: IncomingRing) {
        if (endings.value.containsKey(ring.room)) return // already over here (declined, answered, cancelled)
        val before = live.value[ring.room]
        val merged = before?.copy(ringToken = before.ringToken ?: ring.ringToken, seenByPage = before.seenByPage || ring.seenByPage,
            expiresAt = maxOf(before.expiresAt, ring.expiresAt)) ?: ring
        live.value = live.value + (ring.room to merged)
        if (before == null) {
            val left = merged.expiresAt * 1000 - System.currentTimeMillis()
            main.postDelayed({ if (live.value[ring.room]?.let { it.expiresAt * 1000 <= System.currentTimeMillis() + 500 } == true) end(context, ring.room, "no_answer") },
                left.coerceAtLeast(0))
        }
        when {
            // On screen, the call page's own ring screen answers; a notification would ring twice.
            PushNotifications.foreground -> if (merged.posted && merged.seenByPage) unpost(context, merged)
            !merged.posted -> post(context, merged)
        }
    }

    /** A ring whose notification outlived the process that posted it (the screen was opened from it). */
    fun restore(ring: IncomingRing) {
        if (ring.room !in live.value && ring.room !in endings.value) live.value = live.value + (ring.room to ring.copy(posted = true))
    }

    /** The app left the screen while something rings that only the page was showing. */
    fun background(context: Context) { live.value.values.filter { !it.posted }.forEach { post(context, it) } }

    /** The app came on screen: rings the page shows itself need no notification. */
    fun foreground(context: Context) { live.value.values.filter { it.posted && it.seenByPage }.forEach { unpost(context, it) } }

    private fun unpost(context: Context, ring: IncomingRing) {
        NotificationManagerCompat.from(context).cancel(tag(ring.room), NOTIFICATION_ID)
        live.value[ring.room]?.let { live.value = live.value + (ring.room to it.copy(posted = false)) }
    }

    private fun post(context: Context, ring: IncomingRing) {
        if (!allowed(context)) return
        val notification = notification(context, ring)
        try { NotificationManagerCompat.from(context).notify(tag(ring.room), NOTIFICATION_ID, notification) }
        catch (_: SecurityException) { return /* Permission may be revoked between the check and posting. */ }
        live.value[ring.room]?.let { live.value = live.value + (ring.room to it.copy(posted = true)) }
    }

    fun extras(intent: Intent, ring: IncomingRing): Intent = intent.putExtra(EXTRA_ROOM, ring.room).putExtra(EXTRA_TOKEN, ring.ringToken)
        .putExtra(EXTRA_KIND, ring.kind).putExtra(EXTRA_NAME, ring.callerName).putExtra(EXTRA_DETAIL, ring.callerDetail)
        .putExtra(EXTRA_ADDRESS, ring.callerAddress).putExtra(EXTRA_IDENTITY, ring.identity).putExtra(EXTRA_EXPIRES, ring.expiresAt)

    /** The ring an intent was made for, checked again: intents outlive the process that made them. */
    fun fromIntent(intent: Intent?): IncomingRing? {
        intent ?: return null
        val room = intent.getStringExtra(EXTRA_ROOM)?.takeIf { CallRings.ROOM.matches(it) } ?: return null
        val token = intent.getStringExtra(EXTRA_TOKEN)?.takeIf { CallRings.TOKEN.matches(it) }
        val address = intent.getStringExtra(EXTRA_ADDRESS)?.takeIf { validAddress(it) } ?: return null
        val identity = intent.getStringExtra(EXTRA_IDENTITY)?.takeIf { validAddress(it) } ?: return null
        val name = CallRings.cleanDisplay(intent.getStringExtra(EXTRA_NAME)) ?: displayName(address)
        return live.value[room] ?: IncomingRing(room, token, if (intent.getStringExtra(EXTRA_KIND) == "video") "video" else "audio", name,
            CallRings.cleanDisplay(intent.getStringExtra(EXTRA_DETAIL)), address, identity, intent.getLongExtra(EXTRA_EXPIRES, 0))
    }

    private fun notification(context: Context, ring: IncomingRing): Notification {
        val mutability = PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        val code = ring.room.hashCode()
        val screen = PendingIntent.getActivity(context, code, extras(Intent(context, IncomingCallActivity::class.java), ring)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_USER_ACTION), mutability)
        val answer = PendingIntent.getActivity(context, code + 1, answerIntent(context, ring), mutability)
        val decline = PendingIntent.getBroadcast(context, code + 2, extras(Intent(context, CallActionReceiver::class.java).setAction(ACTION_DECLINE), ring), mutability)
        val caller = Person.Builder().setName(ring.callerName).setKey(ring.callerAddress).setImportant(true)
            .setIcon(IconCompat.createWithBitmap(avatar(context, ring))).build()
        val left = (ring.expiresAt * 1000 - System.currentTimeMillis()).coerceAtLeast(1000)
        return NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setStyle(NotificationCompat.CallStyle.forIncomingCall(caller, decline, answer).setIsVideo(ring.video))
            .setContentTitle(ring.callerName)
            .setContentText(if (ring.video) "Incoming video call" else "Incoming voice call")
            .setSubText(ring.callerDetail)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setOngoing(true).setAutoCancel(false)
            .setTimeoutAfter(left)
            .setContentIntent(screen)
            .setFullScreenIntent(screen, true)
            .addPerson(caller)
            // The channel's ringtone, looped until the notification goes (FLAG_INSISTENT), like a phone call.
            .build().apply { flags = flags or Notification.FLAG_INSISTENT }
    }

    /** The app's avatar (tinted disc, initials in the same hue), drawn for the notification. */
    private fun avatar(context: Context, ring: IncomingRing): Bitmap {
        val dark = context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        val (fill, ink) = avatarTone(ring.callerAddress, dark)
        val size = 192
        val bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        canvas.drawCircle(size / 2f, size / 2f, size / 2f, Paint(Paint.ANTI_ALIAS_FLAG).apply { color = fill.toArgb() })
        val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = ink.toArgb(); textSize = size * 0.36f; textAlign = Paint.Align.CENTER
            typeface = try { ResourcesCompat.getFont(context, R.font.geist_bold) } catch (_: Exception) { null } ?: Typeface.DEFAULT_BOLD
        }
        canvas.drawText(PeerFace.initials(ring.callerName), size / 2f, size / 2f - (text.descent() + text.ascent()) / 2, text)
        return bitmap
    }

    /** Answering opens the app's call view; the call page accepts with the ring token. */
    fun answerIntent(context: Context, ring: IncomingRing): Intent = Development.harnessExtras(context,
        extras(Intent(context, MainActivity::class.java).setAction(ACTION_ANSWER), ring))
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)

    /** The ring stopped, for any reason: take every trace of it down, which stops the ringing. */
    fun end(context: Context, room: String, reason: String) {
        endings.value = (endings.value + (room to reason)).entries.toList().takeLast(16).associate { it.toPair() }
        live.value = live.value - room
        NotificationManagerCompat.from(context).cancel(tag(room), NOTIFICATION_ID)
    }

    /** The person answered here: stop ringing and hand the call page the room and token. */
    fun answered(context: Context, ring: IncomingRing) = end(context, ring.room, "answered_here")

    /**
     * Decline: down at once, then tell the calls service with the ring token (no member token needed).
     * A ring only the page knows about is declined through the page. `done` runs when the attempt is over.
     */
    fun decline(context: Context, ring: IncomingRing, done: () -> Unit = {}) {
        val token = live.value[ring.room]?.ringToken ?: ring.ringToken
        end(context, ring.room, "declined")
        val page = host?.get()
        if (token == null) { page?.decline(ring.room, null); done(); return }
        val origin = origin(context.applicationContext)
        scope.launch {
            try {
                // The page, when it is alive, learns of the decline from the calls service itself.
                if (!RingClient(origin).decline(ring.room, token)) page?.decline(ring.room, token)
            } finally { done() }
        }
    }

    /** Sign-out: nothing rings for a mailbox that is no longer here. */
    fun clear(context: Context) {
        live.value.keys.forEach { NotificationManagerCompat.from(context).cancel(tag(it), NOTIFICATION_ID) }
        live.value = emptyMap()
    }
}

/** Decline from the notification, with no screen. */
class CallActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != IncomingCalls.ACTION_DECLINE) return
        val ring = IncomingCalls.fromIntent(intent) ?: return
        val pending = goAsync()
        IncomingCalls.decline(context.applicationContext, ring) { pending.finish() }
    }
}
