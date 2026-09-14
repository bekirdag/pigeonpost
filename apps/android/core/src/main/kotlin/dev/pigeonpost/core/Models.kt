package dev.pigeonpost.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import java.net.URI

val WireJson = Json { ignoreUnknownKeys = true; explicitNulls = false }

@Serializable
data class Attachment(val id: String, val filename: String, @SerialName("media_type") val mediaType: String, val bytes: Long)

@Serializable
data class Message(
    @SerialName("message_id") val id: String,
    val body: String = "",
    val from: String? = null,
    val to: String? = null,
    val direction: String? = null,
    val peer: String? = null,
    @SerialName("peer_handle") val peerHandle: String? = null,
    @SerialName("sender_handle") val senderHandle: String? = null,
    @SerialName("thread_id") val threadId: String? = null,
    @SerialName("received_at") val receivedAt: Long? = null,
    @SerialName("sent_at") val sentAt: Long? = null,
    val read: Boolean? = null,
    val autonomy: String? = null,
    val verb: String? = null,
    @SerialName("held_because") val heldBecause: String? = null,
    val alias: String? = null,
    @SerialName("sender_standing") val standing: String? = null,
    @SerialName("sender_tier") val tier: String? = null,
    val attachments: List<Attachment>? = null,
) {
    val outgoing get() = direction == "out"
    val at get() = if (outgoing) sentAt ?: receivedAt ?: 0 else receivedAt ?: sentAt ?: 0
    val peerKey get() = peerHandle ?: peer ?: senderHandle ?: (if (outgoing) to else from) ?: "unknown"
}

@Serializable
data class InboxPolicy(@SerialName("accept_all") val acceptAll: Boolean? = null, @SerialName("auto_accept_known") val autoAcceptKnown: Boolean? = null)
@Serializable
data class InboxResponse(val messages: List<Message>? = null, val policy: InboxPolicy? = null)
@Serializable
data class IdentityRow(val address: String, val label: String? = null)
@Serializable
data class IdentitiesResponse(val identities: List<IdentityRow>? = null)
@Serializable
data class CreatedIdentity(val address: String)
@Serializable
data class WhoAmI(val address: String? = null, val handle: String? = null)
data class Mailbox(val address: String, val handle: String? = null, val label: String? = null) {
    val key get() = handle ?: address
    val name get() = if (handle != null) displayName(handle) else label?.takeIf { it.isNotBlank() } ?: displayName(address)
}
@Serializable
data class ServerThread(
    @SerialName("thread_id") val id: String,
    val peer: String,
    val title: String? = null,
    @SerialName("is_default") val isDefault: Boolean? = null,
    @SerialName("created_at") val createdAt: Long? = null,
    @SerialName("last_at") val lastAt: Long? = null,
    val archived: Boolean? = null,
)
@Serializable
data class ThreadsResponse(val threads: List<ServerThread>? = null)
@Serializable
data class Contact(val peer: String, val alias: String? = null, val admission: String = "allow", val autonomy: String = "review", @SerialName("allowed_verbs") val allowedVerbs: List<String>? = null) {
    val wildcard get() = peer.endsWith("/*")
}
@Serializable
data class Vocabulary(val grantable: List<String>? = null, @SerialName("never_auto") val neverAuto: List<String>? = null) {
    val safeGrantable get() = grantable.orEmpty().filterNot { it in neverAuto.orEmpty() }.distinct()
}
@Serializable
data class ContactsResponse(val contacts: List<Contact>? = null, val vocabulary: Vocabulary? = null, val policy: InboxPolicy? = null)
@Serializable
data class ArchiveResponse(val archived: List<String>? = null)
@Serializable
data class SendResponse(@SerialName("message_id") val messageId: String? = null, @SerialName("sent_copy_id") val sentCopyId: String? = null)
@Serializable
data class OpenedThread(@SerialName("thread_id") val id: String)
@Serializable
data class Quota(@SerialName("used_bytes") val usedBytes: Long, @SerialName("limit_bytes") val limitBytes: Long, @SerialName("warn_at_bytes") val warnAtBytes: Long, val tier: String) {
    val fraction get() = if (limitBytes <= 0) 0f else (usedBytes.toDouble() / limitBytes).coerceIn(0.0, 1.0).toFloat()
    val full get() = limitBytes > 0 && usedBytes >= limitBytes
    val warning get() = warnAtBytes > 0 && usedBytes >= warnAtBytes
}
@Serializable
data class HandleOffer(
    val namespace: String? = null,
    @SerialName("expires_at") val expiresAt: Long? = null,
    val eligible: Boolean = false,
    val mailbox: String? = null,
    val source: String? = null,
)
@Serializable
data class HandleAvailability(val name: String, val available: Boolean, val reason: String? = null)

fun tidyHandle(raw: String): String = raw.trim().trim('/').lowercase(java.util.Locale.ROOT)
fun validHandleName(raw: String): Boolean {
    val name = tidyHandle(raw)
    return name.length in 1..32 && name !in setOf("k", "gh") && !name.startsWith('-') && !name.endsWith('-') &&
        name.all { it in 'a'..'z' || it in '0'..'9' || it in "._-" }
}

enum class Delivery { SENT, SENDING, FAILED }
data class PendingMessage(
    val id: String,
    val mailbox: String,
    val to: String,
    val body: String,
    val at: Long,
    val threadId: String? = null,
    val status: Delivery = Delivery.SENDING,
    val sentCopyId: String? = null,
    val attachments: List<Attachment> = emptyList(),
)
data class ThreadMessage(
    val id: String,
    val body: String,
    val outgoing: Boolean,
    val at: Long,
    val threadId: String? = null,
    val read: Boolean = true,
    val autonomy: String? = null,
    val verb: String? = null,
    val heldBecause: String? = null,
    val address: String? = null,
    val standing: String? = null,
    val tier: String? = null,
    val status: Delivery = Delivery.SENT,
    val attachments: List<Attachment> = emptyList(),
) {
    val display get() = displayBody(body, outgoing)
}
data class DisplayBody(val text: String, val requestVerb: String? = null, val unattended: Boolean = false, val failed: Boolean = false)

fun workEnvelope(text: String): String = buildJsonObject {
    put("v", 1)
    put("verb", "full_access")
    put("args", buildJsonObject { put("task", text) })
    put("note", text)
}.toString()

/** Presentation only. No value parsed from a body changes admission or autonomy. */
fun displayBody(body: String, outgoing: Boolean = false): DisplayBody {
    val root = runCatching { WireJson.parseToJsonElement(body) as? JsonObject }.getOrNull()
    if ((root?.get("v") as? JsonPrimitive)?.intOrNull == 1) {
        val verb = (root["verb"] as? JsonPrimitive)?.contentOrNull
        val args = root["args"] as? JsonObject
        val text = (args?.get("task") as? JsonPrimitive)?.contentOrNull
            ?: (args?.get("question") as? JsonPrimitive)?.contentOrNull
            ?: (root["note"] as? JsonPrimitive)?.contentOrNull
        if (verb != null && text != null) return DisplayBody(text, requestVerb = verb)
    }
    if (!outgoing && body.startsWith("pigeonpost-auto-reply v1")) {
        val lines = body.lines()
        val header = lines.first()
        val rest = lines.drop(1).let { if (it.firstOrNull()?.startsWith("Generated unattended") == true) it.drop(1) else it }
        return DisplayBody(rest.dropWhile { it.isBlank() }.joinToString("\n"), unattended = true, failed = header.contains("outcome=failed"))
    }
    return DisplayBody(body)
}

fun displayName(peer: String): String {
    if (peer.startsWith("/k/")) return peer.removePrefix("/k/").let { if (it.length > 12) it.take(7) + "…" + it.takeLast(4) else it }
    return if (peer.startsWith('/') && peer.endsWith("/main") && peer.count { it == '/' } > 1) peer.removeSuffix("/main") else peer.trim('/')
}

/** Supply the address prefix without changing the destination's spelling or internal segments. */
fun conversationAddressInput(input: String): String = input.trim().let { if (it.startsWith('/')) it else "/$it" }

fun validAddress(input: String, wildcard: Boolean = false): Boolean {
    if (input.length !in 2..512 || !input.startsWith('/') || input.any { it.code > 127 }) return false
    val parts = input.drop(1).split('/')
    return parts.withIndex().all { (index, part) ->
        if (part == "*") wildcard && index == parts.lastIndex && parts.size > 1
        else part.isNotEmpty() && part != "." && part != ".." && (!part.contains('*') || part.contains('@')) &&
            part.all { it.isLetterOrDigit() || it in "!\$&'*+-=^_`{|}~.@" }
    }
}

/** A scanned code is allowed to open only the issuer's HTTPS origin, never an arbitrary site. */
fun verifiedSignInUrl(text: String, issuer: String = "https://auth.pigeonpost.dev/realms/pigeonpost-prod"): String? = runCatching {
    val url = URI(text)
    val expected = URI(issuer)
    text.takeIf {
        text.length <= 4096 && url.scheme.equals("https", true) && url.host.equals(expected.host, true)
            && url.rawUserInfo == null && (url.port == -1 || url.port == 443)
            && url.rawFragment == null && !text.any { it.isWhitespace() || it.code < 32 }
    }
}.getOrNull()
