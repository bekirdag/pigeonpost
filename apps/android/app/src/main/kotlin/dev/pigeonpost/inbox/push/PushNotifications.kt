package dev.pigeonpost.inbox.push

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import androidx.core.app.NotificationManagerCompat
import androidx.work.*
import com.google.firebase.messaging.FirebaseMessaging
import dev.pigeonpost.core.DevicePushApi
import dev.pigeonpost.core.SessionExpired
import dev.pigeonpost.inbox.PigeonpostApplication
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.tasks.await
import kotlinx.coroutines.withTimeoutOrNull
import java.util.concurrent.TimeUnit

class PushNotifications(private val context: Context) {
    private val preferences = context.getSharedPreferences("push", Context.MODE_PRIVATE)

    fun update(identity: String?) {
        if (identity == null || !allowed(context)) { clear(); return }
        preferences.edit().putString("identity", identity).apply()
        FirebaseMessaging.getInstance().isAutoInitEnabled = true
        enqueue(context)
    }

    fun clear(): String? {
        val token = preferences.getString("token", null)
        // Clear the display permission synchronously before asynchronous revocation/sign-out.
        preferences.edit().remove("identity").remove("token").apply()
        WorkManager.getInstance(context).cancelUniqueWork(WORK)
        NotificationManagerCompat.from(context).cancelAll()
        return token
    }

    suspend fun unregister(api: DevicePushApi?, token: String?) {
        if (token != null && api != null) withTimeoutOrNull(3000) {
            try { api.unregisterPushDevice(token) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { /* The local display gate is already closed. */ }
        }
        FirebaseMessaging.getInstance().isAutoInitEnabled = false
        try { withTimeoutOrNull(3000) { FirebaseMessaging.getInstance().deleteToken().await() } }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { /* A stale token cannot display while signed out. */ }
    }

    companion object {
        const val CHANNEL = "messages"
        const val WORK = "pigeonpost-push-registration"
        @Volatile var foreground = false

        fun channel(context: Context) {
            context.getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(CHANNEL, "Messages", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "New messages in your selected inbox"
                    lockscreenVisibility = android.app.Notification.VISIBILITY_PRIVATE
                })
        }

        fun allowed(context: Context): Boolean {
            channel(context)
            return NotificationManagerCompat.from(context).areNotificationsEnabled() &&
                context.getSystemService(NotificationManager::class.java).getNotificationChannel(CHANNEL).importance != NotificationManager.IMPORTANCE_NONE
        }

        fun enqueue(context: Context) {
            val work = OneTimeWorkRequestBuilder<PushRegistrationWorker>()
                .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS).build()
            // Serial work reads the latest selected identity; a delayed older request cannot win.
            WorkManager.getInstance(context).enqueueUniqueWork(WORK, ExistingWorkPolicy.APPEND_OR_REPLACE, work)
        }
    }
}

class PushRegistrationWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val preferences = applicationContext.getSharedPreferences("push", Context.MODE_PRIVATE)
        val identity = preferences.getString("identity", null) ?: return Result.success()
        if (!PushNotifications.allowed(applicationContext)) return Result.success()
        val graph = (applicationContext as PigeonpostApplication).graph
        val session = graph.session.state.first { !it.loading }
        if (!session.signedIn || !session.termsAccepted) { PushNotifications(applicationContext).clear(); return Result.success() }
        val api = graph.api as? DevicePushApi ?: return Result.failure()
        return try {
            val token = FirebaseMessaging.getInstance().token.await()
            if (preferences.getString("identity", null) != identity) return Result.success()
            api.registerPushDevice(identity, token)
            if (preferences.getString("identity", null) == identity && graph.session.state.value.signedIn)
                preferences.edit().putString("token", token).apply()
            Result.success()
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (_: SessionExpired) { PushNotifications(applicationContext).clear(); Result.success() }
        catch (_: Exception) { Result.retry() }
    }
}
