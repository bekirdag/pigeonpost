package dev.pigeonpost.inbox

import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.*
import dev.pigeonpost.core.meetings.MeetingNotesCrypto
import kotlinx.serialization.json.*

/**
 * Debug fixture `live` (Phase 6 and 7 on the device): the real-MLS groups world, plus an in-memory
 * stream directory and a stand-in for meeting-intel. Bob is live in Garden club; instrumented tests
 * move the stage and end the stream through [LiveFixtureControl]. Meeting notes are sealed here, on
 * request, for whatever notes keys the app registered: so opening them proves the app's Keystore key.
 */
object LiveFixtureControl {
    const val DEFAULT_STREAM = "str_aaaaaaaaaaaaaaaaaaaaaaaaaa"
    @Volatile var STREAM = DEFAULT_STREAM
    /**
     * tests/e2e-streams/android-app.mjs: read the stream from a loopback mock postbox instead
     * (`base`, the viewer's `token`, the harness's group), so the stage the real streamer sets reaches the app.
     */
    @Volatile var proxy: Triple<String, String, String>? = null
    const val READY = "mtg_ready000000000000000000001"
    const val FAILED = "mtg_failed00000000000000000002"
    const val ELSEWHERE = "mtg_other000000000000000000003"
    @Volatile var live = true
    /** This app's place on the stage: null, "invited" or "speaker". */
    @Volatile var stage: String? = null
    @Volatile var policy = "everyone"
    @Volatile var notesOn = true
    /** Show this app's member as a group admin (the group's own settings then appear). */
    @Volatile var admin = false
    val registered = java.util.concurrent.ConcurrentHashMap<String, String>()
    val deleted = java.util.Collections.synchronizedList(mutableListOf<String>())
    fun reset() { STREAM = DEFAULT_STREAM; proxy = null; live = true; stage = null; policy = "everyone"; notesOn = true; admin = false; registered.clear(); deleted.clear() }
}

internal class LiveFixture(private val base: PostboxApi, handles: AccountHandleApi, private val world: FixtureGroups) : PostboxApi by base, AccountHandleApi by handles {
    private val c = LiveFixtureControl
    override suspend fun capabilities() = Capabilities(listOf(Features.GROUPS, Features.DISAPPEARING, Features.STREAMING) +
        if (c.notesOn) listOf(Features.MEETING_SUMMARIES) else emptyList())

    private fun garden() = world.fake.groups.entries.firstOrNull { it.value.name == "Garden club" }?.key
    private fun bob() = world.fake.handles.entries.first { it.value == "/bob/main" }.key

    private fun stream(gid: String) = buildJsonObject {
        put("id", c.STREAM); put("group_id", gid); put("title", "Seed swap"); putJsonArray("sources") { add("camera") }
        put("by", bob()); put("by_handle", "/bob/main"); put("state", if (c.live) "live" else "ended"); put("started_at", System.currentTimeMillis() / 1000 - 300)
        put("viewers", 2); put("revision", 1)
        putJsonArray("stage") { c.stage?.let { s -> add(buildJsonObject { put("address", world.me); put("state", s) }) } }
    }

    private val brief = buildJsonObject {
        put("v", 1); put("meeting_id", LiveFixtureControl.READY); put("title", "Garden planning"); put("language", "en")
        put("started_at", "2026-10-27T15:00:00.000Z"); put("duration_s", 754)
        putJsonArray("participants") { add("/demo/main"); add("/bob/main") }
        put("summary", "Agreed what to plant this spring; Bob brings the seeds.")
        putJsonArray("decisions") { add("Tomatoes go in the south bed") }
        putJsonArray("action_items") { add(buildJsonObject { put("owner", "/bob/main"); put("task", "Bring the seed packets"); put("due", "Saturday") }) }
        putJsonArray("open_questions") { add("Do we need a second hose?") }
        put("draft", false)
        putJsonArray("transcript") {
            add(buildJsonObject { put("at", "00:04"); put("speaker", "/demo/main"); put("text", "Let's plan the beds.") })
            add(buildJsonObject { put("at", "02:10"); put("speaker", "/bob/main"); put("text", "I'll bring the seeds on Saturday.") })
        }
        putJsonObject("generated_by") { put("stt", "whisper.cpp ggml-small"); put("llm", "qwen-latest-flash-next") }
    }

    private fun row(id: String, state: String, created: Long, failure: String? = null) = buildJsonObject {
        put("id", id); put("kind", "call"); put("state", state); put("created_at", created); put("expires_at", created + 90 * 86400)
        put("host", "/bob/main"); failure?.let { put("failure", it) }
    }

    val transport: GroupTransport = object : GroupTransport by world.fake.transport {
        override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String {
            val gid = garden()
            val now = System.currentTimeMillis() / 1000
            val proxied = c.proxy
            if (proxied != null && method == "GET" && gid != null && (path == "/v1/groups/$gid/streams" || path == "/v1/streams/${c.STREAM}")) {
                val (base, token, group) = proxied
                val target = if (path.startsWith("/v1/groups/")) "/v1/groups/$group/streams" else path
                val text = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    val http = java.net.URL(base + target).openConnection() as java.net.HttpURLConnection
                    http.setRequestProperty("authorization", "Bearer $token"); http.setRequestProperty("x-pigeonpost-identity", "/k/" + token.removePrefix("tok-"))
                    val code = http.responseCode
                    val body = (if (code < 400) http.inputStream else http.errorStream)?.bufferedReader()?.readText().orEmpty()
                    if (code >= 400) throw ApiException(code, runCatching { WireJson.parseToJsonElement(body).jsonObject["error"]?.jsonPrimitive?.content }.getOrNull() ?: "http_$code")
                    body
                }
                // The harness's own ids, mapped onto this fixture's group and mailbox.
                return text.replace("\"$group\"", "\"$gid\"").replace("\"/k/" + token.removePrefix("tok-") + "\"", "\"" + world.me + "\"")
            }
            when {
                method == "GET" && gid != null && path == "/v1/groups/$gid/streams" ->
                    return buildJsonObject { putJsonArray("streams") { if (c.live) add(stream(gid)) } }.toString()
                method == "GET" && path.matches(Regex("/v1/groups/grp_[a-z2-7]+/streams")) -> return """{"streams":[]}"""
                method == "GET" && path == "/v1/streams/${c.STREAM}" -> {
                    if (!c.live || gid == null) throw ApiException(410, "stream_ended")
                    return buildJsonObject { put("stream", stream(gid)); put("role", c.stage ?: "viewer") }.toString()
                }
                path == "/v1/meetings" && method == "GET" -> return buildJsonObject { putJsonArray("meetings") {
                    listOf(row(LiveFixtureControl.READY, "ready", now - 3600), row(LiveFixtureControl.FAILED, "failed", now - 7200, "no_speech"),
                        row(LiveFixtureControl.ELSEWHERE, "ready", now - 9000)).filter { it["id"]!!.jsonPrimitive.content !in c.deleted }.forEach { add(it) }
                } }.toString()
                path.startsWith("/v1/meetings/keys/") && method == "PUT" -> {
                    val id = path.substringAfterLast('/')
                    val key = body!!.jsonObject["public_key"]!!.jsonPrimitive.content
                    require(MeetingNotesCrypto.unb64(key, 65)[0] == 4.toByte())
                    if (c.registered[id]?.let { it != key } == true) throw ApiException(409, "key_exists")
                    c.registered[id] = key
                    return """{"key_id":"$id","registered":true}"""
                }
                path.startsWith("/v1/meetings/mtg_") && method == "DELETE" -> { c.deleted += path.substringAfterLast('/'); return """{"removed":true}""" }
                path.startsWith("/v1/meetings/mtg_") && method == "GET" -> {
                    val id = path.substringAfterLast('/')
                    if (id in c.deleted) throw ApiException(404, "not_found")
                    val detail = row(id, if (id == LiveFixtureControl.FAILED) "failed" else "ready", now - 3600, if (id == LiveFixtureControl.FAILED) "no_speech" else null)
                    if (id == LiveFixtureControl.FAILED) return detail.toString()
                    // ELSEWHERE is wrapped only for another device's key; READY for every key registered here.
                    val recipients = if (id == LiveFixtureControl.ELSEWHERE) listOf(dev.pigeonpost.core.meetings.SoftwareNotesKey().let { it.keyId to MeetingNotesCrypto.unb64(it.publicKey) })
                        else c.registered.map { (k, v) -> k to MeetingNotesCrypto.unb64(v) }
                    val sealed = MeetingNotesCrypto.seal(id, brief.toString().toByteArray(), recipients)
                    return JsonObject(detail + mapOf("alg" to JsonPrimitive("A256GCM"), "iv" to JsonPrimitive(sealed.iv), "ciphertext" to JsonPrimitive(sealed.ciphertext),
                        "keys" to JsonArray(sealed.wraps.map { w -> buildJsonObject { put("key_id", w.keyId); put("epk", w.epk); put("iv", w.iv); put("wrapped", w.wrapped) } }))).toString()
                }
                method == "PATCH" && gid != null && path == "/v1/groups/$gid" && body?.jsonObject?.containsKey("stream_policy") == true -> {
                    if (!c.admin) throw ApiException(403, "not_admin")
                    c.policy = body.jsonObject["stream_policy"]!!.jsonPrimitive.content
                    return """{"ok":true}"""
                }
            }
            val response = world.fake.transport.groupJson(mailbox, method, path, body, device)
            // The group's own views carry its stream policy.
            if (method == "GET" && gid != null && (path.startsWith("/v1/groups?") || path == "/v1/groups/$gid")) {
                val parsed = WireJson.parseToJsonElement(response).jsonObject
                fun withPolicy(g: JsonElement): JsonElement {
                    if (g.jsonObject["id"]?.jsonPrimitive?.content != gid) return g
                    var o = JsonObject(g.jsonObject + ("stream_policy" to JsonPrimitive(c.policy)))
                    val my = o["my"]?.jsonObject
                    if (c.admin && my != null) o = JsonObject(o + ("my" to JsonObject(my + ("role" to JsonPrimitive("admin")))))
                    return o
                }
                return when {
                    parsed["groups"] != null -> JsonObject(parsed + ("groups" to JsonArray(parsed["groups"]!!.jsonArray.map(::withPolicy))))
                    parsed["id"] != null -> withPolicy(parsed)
                    else -> parsed
                }.toString()
            }
            return response
        }
    }
}
