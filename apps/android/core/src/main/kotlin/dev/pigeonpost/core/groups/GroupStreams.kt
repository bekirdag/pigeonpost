package dev.pigeonpost.core.groups

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject

/**
 * Live streams in groups (Phase 6, docs/streaming.md), the app's side of them. The media, the stream
 * key and the stage itself run in the call page (`site-inbox/live.js`) inside the call web view; the
 * app shows the group's LIVE banner, asks the page to watch or go live, and tells the page what only
 * the postbox knows (a stage invitation, a stream that ended), as the web inbox's `groups.js` does
 * with the account's `group.stream` events.
 */
@Serializable
data class StreamStage(val address: String, val state: String)

@Serializable
data class GroupStream(
    val id: String,
    @SerialName("group_id") val groupId: String = "",
    val title: String = "",
    val sources: List<String> = emptyList(),
    val by: String = "",
    @SerialName("by_handle") val byHandle: String? = null,
    val state: String = "live",
    @SerialName("started_at") val startedAt: Long? = null,
    @SerialName("ends_by") val endsBy: Long? = null,
    val viewers: Int = 0,
    val stage: List<StreamStage> = emptyList(),
    val revision: Long = 0,
) {
    val live get() = state == "live"
    /** `invited`, `speaker`, or null when [address] is not on the stage. */
    fun stageOf(address: String?) = stage.firstOrNull { it.address == address }?.state
}

@Serializable
data class GroupStreamsResponse(val streams: List<GroupStream>? = null)

@Serializable
data class GroupStreamResponse(val stream: GroupStream? = null, val role: String? = null)

/** What the call page is showing (`PigeonpostLive.current()`): the stream, its group and our role there. */
data class LiveSession(val streamId: String?, val groupId: String, val role: String) {
    val hosting get() = role == "host"
    /** Our microphone (and camera) are in use: the streamer, or a speaker on the stage. */
    val publishing get() = role == "host" || role == "speaker"
}

/** What one look at `GET /v1/streams/{id}` found. */
sealed class StreamCheck {
    data class Live(val stream: GroupStream) : StreamCheck()
    /** Ended, or no longer ours to see (`410 stream_ended`, `404 no_stream`). */
    data object Ended : StreamCheck()
    /** The network or the server failed: try again later, change nothing. */
    data object Unknown : StreamCheck()
}

object GroupStreams {
    val GROUP_ID = Regex("grp_[a-z2-7]{26}")
    val STREAM_ID = Regex("str_[a-z2-7]{26}")
    const val TITLE_MAX = 100

    /** The group's own rule: active, unrestricted members, and only admins unless the group lets everyone. */
    fun canGoLive(group: Group): Boolean =
        group.active && group.my?.role != "restricted" && (group.streamPolicy == "everyone" || group.admin)

    /** "Jane is live: Sprint demo", or "You are live: …". */
    fun bannerText(stream: GroupStream, me: String?, name: (String?) -> String): String =
        (if (stream.by == me) "You are live" else name(stream.by) + " is live") + (if (stream.title.isBlank()) "" else ": " + stream.title)

    /** The banner's button: back into a stream this device is showing, the streamer's own, or Watch. */
    fun actionLabel(stream: GroupStream, me: String?, showing: LiveSession?): String = when {
        showing?.streamId == stream.id -> "Return"
        stream.by == me -> "Open"
        else -> "Watch"
    }

    /** The second line under the banner, when the stage concerns us. */
    fun stageNote(stream: GroupStream, me: String?, showing: LiveSession?, name: (String?) -> String): String? = when (stream.stageOf(me)) {
        "invited" -> if (showing?.streamId == stream.id) "${name(stream.by)} invited you onto the stage."
            else "${name(stream.by)} invited you onto the stage. Watch to answer; your microphone and camera turn on only if you join."
        "speaker" -> "You are on the stage."
        else -> null
    }

    /** The stream's title as the Go live sheet sends it, or null when it cannot be sent. */
    fun title(raw: String): String? = raw.trim().takeIf { it.isNotEmpty() && it.length <= TITLE_MAX && it.none { c -> c.isISOControl() } }

    /**
     * How a person reads a refusal from the page's `watchStream`/`goLive` or the postbox (the web's
     * `live.js` wording). Null: nothing to say (the person cancelled).
     */
    fun explain(code: String?): String? = when (code) {
        null, "", "cancelled" -> null
        "needs_e2ee" -> "Live streams need a newer Android System WebView — one that can encrypt and decrypt audio and video itself. Update it from Google Play and try again."
        "stream_restricted", "not_admin" -> "Only admins can go live in this group."
        "stream_in_progress" -> "Someone in this group is already live."
        "stream_capacity" -> "Live streams are very busy right now. Please try again in a few minutes."
        "stream_full" -> "This stream is full."
        "stream_ended", "no_stream", "not_started" -> "The stream has ended."
        "group_not_ready" -> "This group is still setting up its encryption. Try again in a moment."
        "not_member" -> "You are not a member of this group."
        "human_required" -> "Going live needs you to be signed in yourself."
        "rate_limited" -> "You have started too many streams. Try again later."
        "busy" -> "Leave the current stream first."
        "signed_out", "identity_changed" -> "Your inbox changed. Open the group again."
        "bridge_unavailable", "invalid_stream", "invalid_group" -> "Live streams need an updated call page. Close and reopen the app, then try again."
        "permission" -> "Allow the camera and microphone in Android settings to go live."
        else -> "Couldn’t open the live stream. Try again."
    }

    /**
     * What the call page must hear (live.js `onGroupStream`) after the postbox's view of a stream
     * changed: a stage invitation for [me], being taken off the stage, or the end. The web inbox gets
     * these from the account's `group.stream` events; the app reads them from the postbox while the
     * stream is on screen.
     */
    fun pageEvents(before: GroupStream?, check: StreamCheck, me: String, streamId: String): List<JsonObject> {
        if (check is StreamCheck.Unknown) return emptyList()
        val after = (check as? StreamCheck.Live)?.stream
        if (check is StreamCheck.Ended || after?.live == false) return listOf(buildJsonObject {
            put("stream_id", streamId); put("group_id", after?.groupId ?: before?.groupId ?: ""); put("state", "ended")
        })
        after ?: return emptyList()
        val was = before?.stageOf(me)
        val now = after.stageOf(me)
        fun stage(state: String) = buildJsonObject {
            put("stream_id", after.id); put("group_id", after.groupId); put("state", "live"); put("revision", after.revision)
            putJsonObject("stage") { put("address", me); put("state", state); put("by", after.by) }
        }
        return when {
            now == "invited" && was != "invited" -> listOf(stage("invited"))
            now == null && (was == "invited" || was == "speaker") -> listOf(stage("viewer"))
            else -> emptyList()
        }
    }
}
