package dev.pigeonpost.inbox.push

import android.app.PendingIntent
import android.content.Intent
import android.content.Context
import android.net.Uri
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import dev.pigeonpost.core.displayName
import dev.pigeonpost.core.validAddress
import dev.pigeonpost.inbox.MainActivity
import dev.pigeonpost.inbox.R

class PigeonpostMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        if (getSharedPreferences("push", MODE_PRIVATE).getString("identity", null) != null)
            PushNotifications.enqueue(this)
    }

    override fun onMessageReceived(message: RemoteMessage) = displayPush(this, message.data)
}

internal fun displayPush(context: Context, data: Map<String, String>) {
    val identity = data["identity"] ?: return
    val peer = data["peer"] ?: return
    val id = data["message_id"] ?: return
    val preferences = context.getSharedPreferences("push", Context.MODE_PRIVATE)
    if (preferences.getString("identity", null) != identity || !validAddress(identity) || !validAddress(peer)
        || id.isBlank() || id.length > 200 || !PushNotifications.allowed(context) || PushNotifications.foreground) return
    val intent = Intent(context, MainActivity::class.java)
        .setData(Uri.Builder().scheme("pigeonpost-notification").authority("message").appendPath(id).build())
        .putExtra("push_identity", identity).putExtra("push_peer", peer)
        .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    val pending = PendingIntent.getActivity(context, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    val public = NotificationCompat.Builder(context, PushNotifications.CHANNEL)
        .setSmallIcon(R.drawable.ic_notification).setContentTitle("Pigeonpost").setContentText("You have a new message.").build()
    val notification = NotificationCompat.Builder(context, PushNotifications.CHANNEL)
        .setSmallIcon(R.drawable.ic_notification).setContentTitle(displayName(peer))
        .setContentText("New message in ${displayName(data["mailbox"] ?: identity)}")
        .setCategory(NotificationCompat.CATEGORY_MESSAGE).setPriority(NotificationCompat.PRIORITY_HIGH)
        .setVisibility(NotificationCompat.VISIBILITY_PRIVATE).setPublicVersion(public)
        .setContentIntent(pending).setAutoCancel(true).setOnlyAlertOnce(true).build()
    try { NotificationManagerCompat.from(context).notify(id, 1, notification) }
    catch (_: SecurityException) { /* Permission may be revoked between the check and posting. */ }
}
