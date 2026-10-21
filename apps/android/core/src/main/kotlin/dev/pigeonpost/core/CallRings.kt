package dev.pigeonpost.core

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import java.util.concurrent.TimeUnit

/**
 * An incoming call woken by a push (docs/call-alerting.md, Phase 1c). Every field came from the push
 * and was checked by [CallRings.ring]; nothing here is trusted beyond being well formed.
 */
data class CallRing(
    val room: String, val code: String?, val kind: String, val ringToken: String,
    val fromAddress: String, val fromHandle: String?, val fromDisplay: String?,
    val to: String, val identity: String, val expiresAt: Long,
) {
    val video get() = kind == "video"
    /** What the call screen calls the caller: the name they gave, else their handle, else their address. */
    val callerName: String get() = fromDisplay ?: fromHandle?.let(::displayName) ?: displayName(fromAddress)
    /** The second line: the handle (or address) under a display name. */
    val callerDetail: String? get() = if (fromDisplay != null) fromHandle ?: fromAddress else if (fromHandle != null) null else fromAddress
}

data class CallCancel(val room: String, val reason: String, val identity: String)

object CallRings {
    val ROOM = Regex("^[A-Za-z0-9_-]{10,64}$")
    val TOKEN = Regex("^[A-Za-z0-9_-]{43}$")
    private val CODE = Regex("^[A-Za-z0-9]{1,16}$")
    private val REASONS = setOf("answered_elsewhere", "declined", "busy", "no_answer", "cancelled", "ended")
    /** A ring lasts 45 s; anything claiming to ring for much longer is not a ring this app shows. */
    const val LONGEST_SECONDS = 180L

    /**
     * A `call_ring` data message, or null when it is not one this device should show: for another
     * signed-in mailbox, malformed, or already over.
     */
    fun ring(data: Map<String, String>, signedIn: String?, nowSeconds: Long): CallRing? {
        if (data["type"] != "call_ring" || signedIn == null) return null
        val identity = data["identity"] ?: return null
        if (identity != signedIn || !validAddress(identity)) return null
        val room = data["room"]?.takeIf { ROOM.matches(it) } ?: return null
        val token = data["ring_token"]?.takeIf { TOKEN.matches(it) } ?: return null
        val kind = data["kind"]?.takeIf { it == "audio" || it == "video" } ?: return null
        val from = data["from_address"]?.takeIf { validAddress(it) } ?: return null
        val handle = data["from_handle"]?.takeIf { it.isNotEmpty() }?.let { if (validAddress(it)) it else return null }
        val to = data["to"]?.takeIf { validAddress(it) } ?: return null
        val expires = data["expires_at"]?.toLongOrNull() ?: return null
        if (expires <= nowSeconds || expires > nowSeconds + LONGEST_SECONDS) return null
        val code = data["code"]?.takeIf { CODE.matches(it) }
        return CallRing(room, code, kind, token, from, handle, cleanDisplay(data["from_display"]), to, identity, expires)
    }

    /** A `call_cancel` data message for the signed-in mailbox. */
    fun cancel(data: Map<String, String>, signedIn: String?): CallCancel? {
        if (data["type"] != "call_cancel" || signedIn == null) return null
        val identity = data["identity"]?.takeIf { it == signedIn } ?: return null
        val room = data["room"]?.takeIf { ROOM.matches(it) } ?: return null
        val reason = data["reason"]?.takeIf { it in REASONS } ?: "ended"
        return CallCancel(room, reason, identity)
    }

    /** A display name fit for a lock screen: no control or bidi characters, at most 60 characters. */
    fun cleanDisplay(value: String?): String? {
        val cleaned = value.orEmpty().filterNot { Character.isISOControl(it) || it in BIDI || Character.getType(it) == Character.FORMAT.toInt() }
            .replace(Regex("\\s+"), " ").trim().take(60).trim()
        return cleaned.ifEmpty { null }
    }
    private const val BIDI = "‎‏‪‫‬‭‮⁦⁧⁨⁩؜"

    /** Short words for why a ring ended, for the ending call screen. */
    fun endedText(reason: String?) = when (reason) {
        "answered_elsewhere" -> "Answered on another device"
        "declined" -> "Declined"
        "busy" -> "Busy"
        "cancelled" -> "Call cancelled"
        else -> "Missed call"
    }
}

/** What `GET /calls/v1/rings/{room}` says. `ringing == false` covers both ended and unknown rings. */
data class RingState(val ringing: Boolean, val reason: String? = null)

/**
 * The two things a ring token allows without a member token: asking whether the ring is still live,
 * and declining it. The token is the credential, so these work from a lock screen with no session.
 */
class RingClient(private val origin: String, private val client: OkHttpClient = DEFAULT) {
    init { require(origin.startsWith("https://") || origin.startsWith("http://")) }

    /** Null when the answer could not be had (offline, server trouble): keep ringing until told otherwise. */
    fun state(room: String, token: String): RingState? {
        if (!CallRings.ROOM.matches(room) || !CallRings.TOKEN.matches(token)) return RingState(false, "ended")
        val request = Request.Builder().url("${origin.trimEnd('/')}/calls/v1/rings/$room").header("x-ring-token", token)
            .header("accept", "application/json").get().build()
        return try {
            client.newCall(request).execute().use { response ->
                when {
                    response.code == 404 -> RingState(false, "ended")
                    !response.isSuccessful -> null
                    else -> {
                        val body = Json.parseToJsonElement(response.body?.string().orEmpty()).jsonObject
                        val state = body.string("state")
                        RingState(state == "ringing", body.string("reason"))
                    }
                }
            }
        } catch (_: Exception) { null }
    }

    /** True when the server took the decline (or the ring was already over). */
    fun decline(room: String, token: String): Boolean {
        if (!CallRings.ROOM.matches(room) || !CallRings.TOKEN.matches(token)) return false
        val body = """{"ring_token":"$token","reason":"declined"}""".toRequestBody("application/json".toMediaType())
        val request = Request.Builder().url("${origin.trimEnd('/')}/calls/v1/rings/$room/decline").post(body).build()
        return try { client.newCall(request).execute().use { it.isSuccessful || it.code == 404 } } catch (_: Exception) { false }
    }

    private fun JsonObject.string(key: String) = try { this[key]?.jsonPrimitive?.takeIf { it.isString }?.content } catch (_: Exception) { null }

    companion object {
        val DEFAULT: OkHttpClient by lazy {
            OkHttpClient.Builder().connectTimeout(5, TimeUnit.SECONDS).readTimeout(5, TimeUnit.SECONDS).callTimeout(8, TimeUnit.SECONDS).build()
        }
    }
}
