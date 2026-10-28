//  Live streams (Phase 6) while they are on screen: audio focus, and a foreground service so a stream
//  keeps playing (and a streamer keeps sending) when the person switches apps.
//
//  Android's rules, followed here:
//  - Audio focus (AudioManager.requestAudioFocus) with media attributes while a stream plays. Losing it
//    (another app plays, a phone call) mutes the stream's players; getting it back unmutes them. Live
//    media cannot be paused and caught up, so muting is the "pause". Ducking is left to the system.
//  - A foreground service of type `mediaPlayback` while watching. Streaming or speaking on the stage
//    also uses the camera and microphone, which Android 11+ blocks in the background unless the
//    service is started with the `camera`/`microphone` types while the app is visible and holds the
//    permissions; Android 14 requires each type to be declared and its FOREGROUND_SERVICE_* permission
//    held. A service started without those types still plays; the camera and microphone then pause
//    in the background, as Android intends.
//  - The service starts only from the app's own screen (a tap on Watch or Go live), never from the
//    background, and stops as soon as the page no longer shows a stream.
package dev.pigeonpost.inbox.calls

import android.Manifest
import android.app.Application
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import dev.pigeonpost.core.groups.LiveSession
import dev.pigeonpost.inbox.MainActivity
import dev.pigeonpost.inbox.R
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import java.lang.ref.WeakReference

class LiveMedia(private val app: Application, private val host: CallHost, scope: CoroutineScope) {
    private val audio = app.getSystemService(AudioManager::class.java)
    private var focus: AudioFocusRequest? = null
    private var shown: LiveSession? = null

    init {
        LiveStreamService.host = WeakReference(host)
        scope.launch { host.live.collect(::update) }
    }

    private fun update(session: LiveSession?) {
        val before = shown
        shown = session
        if (session == null) {
            focus?.let { audio.abandonAudioFocusRequest(it) }; focus = null
            host.setStreamMuted(false)
            if (before != null) LiveStreamService.stop(app)
            return
        }
        if (focus == null) {
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MOVIE).build())
                .setWillPauseWhenDucked(false)
                .setOnAudioFocusChangeListener { change ->
                    when (change) {
                        AudioManager.AUDIOFOCUS_LOSS, AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> host.setStreamMuted(true)
                        AudioManager.AUDIOFOCUS_GAIN -> host.setStreamMuted(false)
                    }
                }.build()
            focus = request
            host.setStreamMuted(audio.requestAudioFocus(request) == AudioManager.AUDIOFOCUS_REQUEST_FAILED)
        }
        if (before == null || before.role != session.role || before.streamId != session.streamId) LiveStreamService.start(app, session)
    }
}

class LiveStreamService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_LEAVE) {
            host?.get()?.stopStream()
            stopSelf(); return START_NOT_STICKY
        }
        val publishing = intent?.getBooleanExtra(EXTRA_PUBLISHING, false) == true
        channel(this)
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val leave = PendingIntent.getService(this, 1, Intent(this, LiveStreamService::class.java).setAction(ACTION_LEAVE), PendingIntent.FLAG_IMMUTABLE)
        val notification = NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(if (intent?.getStringExtra(EXTRA_ROLE) == "host") "You are live" else if (publishing) "You are on the stage" else "Watching a live stream")
            .setContentText(if (publishing) "Your camera and microphone are on. Everyone watching can see and hear you." else "End-to-end encrypted. Tap to return to it.")
            .setOngoing(true).setOnlyAlertOnce(true).setSilent(true).setCategory(NotificationCompat.CATEGORY_TRANSPORT)
            .setContentIntent(open)
            .addAction(0, if (intent?.getStringExtra(EXTRA_ROLE) == "host") "End stream" else "Leave", leave)
            .build()
        if (!startAsForeground(notification, publishing)) { stopSelf(); return START_NOT_STICKY }
        return START_NOT_STICKY
    }

    /** Ask for the camera and microphone types only when they are held; fall back to playback alone. */
    private fun startAsForeground(notification: Notification, publishing: Boolean): Boolean {
        val playback = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK else 0
        var types = playback
        if (publishing && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            if (held(Manifest.permission.CAMERA)) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
            if (held(Manifest.permission.RECORD_AUDIO)) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        }
        for (attempt in listOf(types, playback).distinct()) {
            try { ServiceCompat.startForeground(this, NOTIFICATION, notification, attempt); return true }
            catch (_: Exception) { /* not allowed now (the app went to the background, a permission was revoked) */ }
        }
        return false
    }
    private fun held(permission: String) = ContextCompat.checkSelfPermission(this, permission) == PackageManager.PERMISSION_GRANTED

    companion object {
        const val CHANNEL = "live"
        const val NOTIFICATION = 7301
        private const val ACTION_LEAVE = "dev.pigeonpost.inbox.LIVE_LEAVE"
        private const val EXTRA_PUBLISHING = "publishing"
        private const val EXTRA_ROLE = "role"
        @Volatile var host: WeakReference<CallHost>? = null
        /** Set while the service runs: for tests and for not starting it twice. */
        @Volatile var running = false
            private set

        fun channel(context: Context) {
            val manager = context.getSystemService(NotificationManager::class.java)
            if (manager.getNotificationChannel(CHANNEL) != null) return
            manager.createNotificationChannel(NotificationChannel(CHANNEL, "Live streams", NotificationManager.IMPORTANCE_LOW).apply {
                description = "Shown while you watch, stream or speak in a group's live stream."
                setShowBadge(false)
            })
        }

        fun start(context: Context, session: LiveSession) {
            val intent = Intent(context, LiveStreamService::class.java).putExtra(EXTRA_PUBLISHING, session.publishing).putExtra(EXTRA_ROLE, session.role)
            try { ContextCompat.startForegroundService(context, intent); running = true }
            catch (_: Exception) { /* started from the background: Android refuses, and the stream plays only while visible */ }
        }

        fun stop(context: Context) {
            running = false
            context.stopService(Intent(context, LiveStreamService::class.java))
        }
    }
}
