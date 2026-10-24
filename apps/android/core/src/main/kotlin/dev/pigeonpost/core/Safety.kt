package dev.pigeonpost.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * Phase 1b wire models: capabilities, filtering, call history, blocking and profile pictures
 * (docs/planning SDS §12, §13, §15, §16). Every feature is gated on `GET /v1/capabilities`; an
 * older postbox simply leaves them all off, and the app then looks exactly as before.
 */
object Features {
    const val MESSAGE_FILTERING = "message_filtering"
    const val CALL_ADMISSION = "call_admission"
    const val CALL_HISTORY = "call_history"
    const val AVATARS = "avatars"
    const val GROUPS = "groups"
    const val DISAPPEARING = "disappearing"
}

@Serializable
data class Capabilities(val features: List<String>? = null, @SerialName("api_version") val apiVersion: String? = null) {
    val enabled get() = features.orEmpty().toSet()
}

/** `GET/PUT /v1/policy`. Unknown fields are kept by the server; the client sends only what changed. */
@Serializable
data class FilterPolicy(
    @SerialName("unknown_calls") val unknownCalls: String? = null,
    @SerialName("unnamed_calls") val unnamedCalls: String? = null,
    @SerialName("unnamed_messages") val unnamedMessages: String? = null,
    @SerialName("unknown_messages") val unknownMessages: String? = null,
    @SerialName("low_reputation") val lowReputation: String? = null,
    @SerialName("call_history_days") val callHistoryDays: Int? = null,
) {
    fun value(field: String): String? = when (field) {
        "unknown_calls" -> unknownCalls
        "unnamed_calls" -> unnamedCalls
        "unnamed_messages" -> unnamedMessages
        "unknown_messages" -> unknownMessages
        "low_reputation" -> lowReputation
        "call_history_days" -> callHistoryDays?.toString()
        else -> null
    }
    fun with(field: String, value: String): FilterPolicy = when (field) {
        "unknown_calls" -> copy(unknownCalls = value)
        "unnamed_calls" -> copy(unnamedCalls = value)
        "unnamed_messages" -> copy(unnamedMessages = value)
        "unknown_messages" -> copy(unknownMessages = value)
        "low_reputation" -> copy(lowReputation = value)
        "call_history_days" -> copy(callHistoryDays = value.toInt())
        else -> this
    }
}

/** One Settings → Privacy & filtering control, in the web inbox's order and words. */
data class PolicyRow(val field: String, val feature: String, val title: String, val note: String?, val options: List<Pair<String, String>>)

val POLICY_ROWS = listOf(
    PolicyRow("unknown_calls", Features.CALL_ADMISSION, "Calls from people you don’t know",
        "Silence keeps the call out of your way and lists it under Filtered calls.", listOf("ring" to "Ring", "silence" to "Silence", "block" to "Block")),
    PolicyRow("unnamed_messages", Features.MESSAGE_FILTERING, "Messages from addresses without a name",
        "Addresses like /k/… that have no handle.", listOf("show" to "Show", "filter" to "Move to Filtered", "block" to "Block")),
    PolicyRow("unknown_messages", Features.MESSAGE_FILTERING, "Messages from people you don’t know",
        "People you know are your contacts and anyone you have written to first.", listOf("show" to "Show", "filter" to "Move to Filtered", "block" to "Block")),
    PolicyRow("unnamed_calls", Features.CALL_ADMISSION, "Calls from addresses without a name", null,
        listOf("ring" to "Ring", "silence" to "Silence", "block" to "Block")),
    PolicyRow("low_reputation", Features.MESSAGE_FILTERING, "Low-reputation senders",
        "Senders others have reported as spam.", listOf("filter" to "Filter", "allow" to "Allow")),
    PolicyRow("call_history_days", Features.CALL_HISTORY, "Keep call history",
        "Off stops keeping new calls. Entries older than this are deleted.", listOf("0" to "Off", "30" to "30 days", "180" to "180 days", "365" to "1 year")),
)

@Serializable
data class CallLogEntry(
    val id: String,
    val direction: String = "incoming",
    val outcome: String? = null,
    val kind: String = "audio",
    val peer: String? = null,
    @SerialName("peer_handle") val peerHandle: String? = null,
    val participants: List<String>? = null,
    @SerialName("group_id") val groupId: String? = null,
    @SerialName("started_at") val startedAt: Long = 0,
    @SerialName("answered_at") val answeredAt: Long? = null,
    @SerialName("ended_at") val endedAt: Long? = null,
    val duration: Long? = null,
    val seen: Boolean? = null,
) {
    val incoming get() = direction == "incoming"
    val video get() = kind == "video"
    /** The address to open or call back: the handle when there is one. */
    val key get() = peerHandle ?: peer
    val missed get() = incoming && (outcome == "missed" || outcome == "cancelled")
    val label get() = when (outcome) {
        "answered" -> if (incoming) "Incoming" else "Outgoing"
        "missed" -> if (incoming) "Missed" else "No answer"
        "declined" -> "Declined"
        "cancelled" -> if (incoming) "Missed" else "Cancelled"
        "busy" -> "Busy"
        "filtered" -> "Filtered"
        "failed" -> "Failed"
        else -> "In progress"
    }
    val talked get() = duration ?: if (answeredAt != null && endedAt != null) endedAt - answeredAt else null
    /** "Missed · Video · 2:05 · 4 people", the row's second line. */
    val summary get() = buildList {
        add(label); add(if (video) "Video" else "Voice")
        if (outcome == "answered") talked?.let { add(callDuration(it)) }
        val people = participants.orEmpty().size
        if (groupId != null || people > 2) add("$people people")
    }.joinToString(" · ")
}

fun callDuration(seconds: Long): String {
    val total = seconds.coerceAtLeast(0)
    val h = total / 3600; val m = (total % 3600) / 60; val s = total % 60
    return if (h > 0) "%d:%02d:%02d".format(h, m, s) else "%d:%02d".format(m, s)
}

@Serializable
data class CallLogPage(val entries: List<CallLogEntry>? = null, @SerialName("unseen_missed") val unseenMissed: Int = 0, @SerialName("next_before") val nextBefore: Long? = null)
@Serializable
data class CallsSeen(@SerialName("unseen_missed") val unseenMissed: Int = 0)

@Serializable
data class AvatarUrls(
    @SerialName("64") val small: String? = null,
    @SerialName("128") val medium: String? = null,
    @SerialName("256") val large: String? = null,
    @SerialName("512") val huge: String? = null,
    val jpeg: String? = null,
)

/** A picture as the server describes it: never the bytes, only where to fetch them. */
@Serializable
data class AvatarInfo(
    val id: String? = null,
    val version: Int? = null,
    val hash: String? = null,
    val visibility: String? = null,
    val urls: AvatarUrls? = null,
) {
    /** Cache identity: the content hash, or the id and version (which change with visibility). */
    val cacheKey get() = hash ?: "${id}:${version}"
    /** Only same-origin paths are fetched; anything else is ignored. */
    fun path(large: Boolean = false): String? =
        (if (large) urls?.large ?: urls?.medium else urls?.medium ?: urls?.large ?: urls?.jpeg)?.takeIf { it.startsWith("/v1/") && !it.contains("..") }
}
@Serializable
data class AvatarsResponse(val avatars: Map<String, AvatarInfo?>? = null)
@Serializable
data class OwnAvatarResponse(val avatar: AvatarInfo? = null, val avatars: List<AddressedAvatar>? = null)
@Serializable
data class AddressedAvatar(val address: String? = null, val avatar: AvatarInfo? = null)

/** A square crop in the uploaded picture's own pixels. */
data class CropRect(val x: Int, val y: Int, val size: Int)

/**
 * Where "Block everyone at …" applies, or null when nobody owns the namespace (a provider or a
 * key address) and a wildcard would catch strangers who merely share it. Mirrors the web inbox.
 */
fun wildcardFor(peer: String): String? {
    if (peer.startsWith("/k/") || peer.endsWith("/*")) return null
    val parts = peer.split('/').filter { it.isNotEmpty() }
    if (parts.isEmpty()) return null
    val parent = if (parts.size == 1) parts[0] else parts.dropLast(1).joinToString("/")
    if (parent in setOf("github", "pp", "k")) return null
    return "/$parent/*"
}

/** The allowed disappearing-message timers, in seconds; null is off. */
val DISAPPEARING_TIMERS: List<Long?> = listOf(null, 300, 3600, 28800, 86400, 604800, 2419200)

fun timerLabel(seconds: Long?): String = when (seconds) {
    null, 0L -> "Off"
    300L -> "5 minutes"
    3600L -> "1 hour"
    28800L -> "8 hours"
    86400L -> "1 day"
    604800L -> "1 week"
    2419200L -> "4 weeks"
    else -> "${seconds / 60} minutes"
}

const val DISAPPEARING_LIMITS = "Recipients can still take screenshots or copy text, notifications already shown may stay on screen, and copies made outside Pigeonpost are not affected."
