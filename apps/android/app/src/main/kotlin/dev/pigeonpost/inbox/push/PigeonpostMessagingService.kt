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
import dev.pigeonpost.inbox.AvatarPictures
import dev.pigeonpost.core.groups.GroupLinks
import androidx.core.app.Person
import androidx.core.graphics.drawable.IconCompat
import dev.pigeonpost.inbox.R
import dev.pigeonpost.inbox.calls.IncomingCalls
import dev.pigeonpost.core.calendar.CalendarNotice

class PigeonpostMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        if (getSharedPreferences("push", MODE_PRIVATE).getString("identity", null) != null)
            PushNotifications.enqueue(this)
    }

    // Calls first: a call push carries no `peer`/`message_id` and never reaches the mail notice.
    override fun onMessageReceived(message: RemoteMessage) {
        if (!IncomingCalls.push(this, message.data)) displayPush(this, message.data)
    }
}

internal fun displayPush(context: Context, data: Map<String, String>) {
    if (CalendarNotice.matches(data)) { displayCalendarPush(context, data); return }
    val identity = data["identity"] ?: return
    val peer = data["peer"] ?: return
    val id = data["message_id"] ?: return
    val preferences = context.getSharedPreferences("push", Context.MODE_PRIVATE)
    // A group push names the group (`grp_…`) as its peer and the group's name as its title; it never
    // carries content, because the postbox cannot read group messages.
    val group = GroupLinks.isGroupId(peer)
    if (preferences.getString("identity", null) != identity || !validAddress(identity) || !(group || validAddress(peer))
        || id.isBlank() || id.length > 200 || !PushNotifications.allowed(context)) return
    if (PushNotifications.foreground) { if (group) PushNotifications.groupPoke?.invoke(); return }
    val intent = Intent(context, MainActivity::class.java)
        .setData(Uri.Builder().scheme("pigeonpost-notification").authority("message").appendPath(id).build())
        .putExtra("push_identity", identity).putExtra("push_peer", peer)
        .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    val pending = PendingIntent.getActivity(context, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    val public = NotificationCompat.Builder(context, PushNotifications.CHANNEL)
        .setSmallIcon(R.drawable.ic_notification).setContentTitle("Pigeonpost").setContentText("You have a new message.").build()
    val title = if (group) data["title"]?.filter { !it.isISOControl() }?.take(80)?.ifBlank { null } ?: "Group" else displayName(peer)
    // The sender's face, as last seen in the app (SDS §16.1: Android `Person` icon). Never fetched here.
    val face = if (group) null else AvatarPictures.forNotification(context, peer)
    val person = Person.Builder().setName(title).setKey(peer).apply { face?.let { setIcon(IconCompat.createWithBitmap(it)) } }.build()
    val notification = NotificationCompat.Builder(context, PushNotifications.CHANNEL)
        .setSmallIcon(R.drawable.ic_notification).setContentTitle(title)
        .setContentText(if (group) "New message" else "New message in ${displayName(data["mailbox"] ?: identity)}")
        .addPerson(person).apply { face?.let { setLargeIcon(it) } }
        .setCategory(NotificationCompat.CATEGORY_MESSAGE).setPriority(NotificationCompat.PRIORITY_HIGH)
        .setVisibility(NotificationCompat.VISIBILITY_PRIVATE).setPublicVersion(public)
        .setContentIntent(pending).setAutoCancel(true).setOnlyAlertOnce(true).build()
    try { NotificationManagerCompat.from(context).notify(id, 1, notification) }
    catch (_: SecurityException) { /* Permission may be revoked between the check and posting. */ }
}

private fun displayCalendarPush(context: Context, data: Map<String, String>) {
    val notice = CalendarNotice.parse(data) ?: return
    val id = data["message_id"]?.takeIf { it.isNotBlank() && it.length <= 200 && it.none(Char::isISOControl) } ?: return
    val preferences = context.getSharedPreferences("push", Context.MODE_PRIVATE)
    if (preferences.getString("identity", null) != notice.mailbox) return
    if (PushNotifications.foreground) { PushNotifications.calendarPoke?.invoke(); return }
    if (!PushNotifications.allowed(context)) return
    val reference = java.security.MessageDigest.getInstance("SHA-256").digest(id.toByteArray()).joinToString("") { "%02x".format(it) }
    val intent = Intent(context, MainActivity::class.java)
        .setData(Uri.Builder().scheme("pigeonpost-notification").authority("calendar").appendPath(reference).build())
        .putExtra("push_type", "calendar").putExtra("push_identity", notice.mailbox)
        .putExtra("push_message_id", id).putExtra("calendar_event_id", notice.event?.eventId)
        .putExtra("calendar_occurrence_id", notice.event?.occurrenceId)
        .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    val pending = PendingIntent.getActivity(context, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    val public = NotificationCompat.Builder(context, PushNotifications.CHANNEL)
        .setSmallIcon(R.drawable.ic_notification).setContentTitle("Pigeonpost").setContentText("Calendar update").build()
    val title = data["title"]?.filter { !it.isISOControl() }?.take(100)?.ifBlank { null } ?: "Calendar"
    val notification = NotificationCompat.Builder(context, PushNotifications.CHANNEL)
        .setSmallIcon(R.drawable.ic_notification).setContentTitle(title)
        .setContentText(if (notice.event?.occurrenceId != null) "Calendar reminder" else "Calendar update")
        .setCategory(NotificationCompat.CATEGORY_EVENT).setPriority(NotificationCompat.PRIORITY_HIGH)
        .setVisibility(NotificationCompat.VISIBILITY_PRIVATE).setPublicVersion(public)
        .setContentIntent(pending).setAutoCancel(true).setOnlyAlertOnce(true).build()
    try { NotificationManagerCompat.from(context).notify("calendar:$reference", 1, notification) }
    catch (_: SecurityException) { /* The device owner may revoke permission while this is built. */ }
}
