package dev.pigeonpost.inbox.push

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import androidx.core.app.NotificationManagerCompat
import androidx.work.*
import com.google.firebase.messaging.FirebaseMessaging
import dev.pigeonpost.core.DevicePushApi
import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.SessionExpired
import dev.pigeonpost.inbox.PigeonpostApplication
import dev.pigeonpost.inbox.calls.IncomingCalls
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.tasks.await
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.withContext
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
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
        IncomingCalls.clear(context)
        return token
    }

    suspend fun unregister(api: DevicePushApi?, token: String?, isCurrent: () -> Boolean = { true }) {
        if (token != null && api != null) withTimeoutOrNull(3000) {
            try { api.unregisterPushDevice(token) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { /* The local display gate is already closed. */ }
        }
        tokenLifecycle.withLock {
            if (!isCurrent()) return@withLock
            FirebaseMessaging.getInstance().isAutoInitEnabled = false
            try { withTimeoutOrNull(3000) { FirebaseMessaging.getInstance().deleteToken().await() } }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { /* A stale token cannot display while signed out. */ }
        }
    }

    companion object {
        const val CHANNEL = "messages"
        const val WORK = "pigeonpost-push-registration"
        @Volatile var foreground = false
        /** While the app is in front, a group push only asks the open app to look now. */
        @Volatile var groupPoke: (() -> Unit)? = null
        @Volatile var calendarPoke: (() -> Unit)? = null
        /** A new registration fetches its token after any old deletion has completed. */
        internal val tokenLifecycle = Mutex()

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
        val authorization = graph.session.authorizationLease ?: return Result.retry()
        val lease = authorization.child()
        fun current() = graph.session.authorizationLease === authorization && lease.isValid &&
            preferences.getString("identity", null) == identity && graph.session.state.value.termsAccepted
        val api = graph.api as? DevicePushApi ?: return Result.failure()
        return try {
            withContext(AuthorizationContext(lease)) {
                val token = PushNotifications.tokenLifecycle.withLock {
                    if (!current()) return@withContext Result.success()
                    FirebaseMessaging.getInstance().isAutoInitEnabled = true
                    FirebaseMessaging.getInstance().token.await()
                }
                if (!current()) return@withContext Result.success()
                api.registerPushDevice(identity, token)
                if (current()) preferences.edit().putString("token", token).apply()
                Result.success()
            }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (_: SessionExpired) { if (graph.session.authorizationLease === authorization || graph.session.authorizationLease == null) PushNotifications(applicationContext).clear(); Result.success() }
        catch (_: Exception) { Result.retry() }
        finally { lease.retire() }
    }
}
