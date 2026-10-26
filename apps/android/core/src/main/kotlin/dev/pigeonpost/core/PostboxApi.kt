package dev.pigeonpost.core

import kotlinx.coroutines.CancellationException
import dev.pigeonpost.core.calendar.CalendarTransport
import dev.pigeonpost.core.calendar.CalendarMeetingApi
import dev.pigeonpost.core.calendar.CalendarMeetingCode
import dev.pigeonpost.core.calendar.CalendarMeetingStatus

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException
import dev.pigeonpost.core.groups.GroupCallState
import dev.pigeonpost.core.groups.GroupTransport
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import okhttp3.Call
import okhttp3.Callback
import okhttp3.ConnectionPool
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.asRequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.io.File
import java.io.IOException
import java.io.OutputStream
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLException
import kotlin.coroutines.resumeWithException

interface TokenProvider {
    val authorizationLease: AuthorizationLease? get() = null
    /** A rejected token is renewed only if it is still the current token. */
    suspend fun token(rejected: String? = null): String
    suspend fun invalidate(rejected: String) {}
    /** Session implementations recheck the expected grant under their own state mutex. */
    suspend fun token(rejected: String?, expectedAuthorization: AuthorizationLease?): String {
        if (authorizationLease !== expectedAuthorization) throw AuthorizationRetired()
        return token(rejected)
    }
    suspend fun invalidate(rejected: String, expectedAuthorization: AuthorizationLease?) {
        if (authorizationLease !== expectedAuthorization) throw AuthorizationRetired()
        invalidate(rejected)
    }
}

class SessionExpired : IOException("Your session expired. Sign in again.")
class PostboxConnectionInterrupted(cause: IOException) : IOException("The connection was interrupted. Please try again.", cause)
class ApiException(val status: Int, val code: String?, detail: String? = null, val problem: JsonObject? = null) : IOException(detail ?: when (code) {
    "not_admitted" -> "They are not accepting messages from this inbox."
    "recipient_unresolved" -> "No inbox at that address."
    "recipient_inbox_full" -> "Their inbox is full."
    "unauthorized" -> "Your session expired. Sign in again."
    else -> code ?: "The postbox answered $status."
})

interface PostboxApi {
    suspend fun identities(): List<IdentityRow>
    suspend fun whoami(identity: String): WhoAmI
    suspend fun createIdentity(handle: String? = null): String
    suspend fun inbox(identity: String, wait: Int? = null): InboxResponse
    suspend fun threads(identity: String): List<ServerThread>
    suspend fun contacts(identity: String): ContactsResponse
    suspend fun archive(identity: String): Set<String>
    suspend fun quota(identity: String): Quota
    suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String> = emptyList()): SendResponse
    suspend fun ack(identity: String, messageId: String)
    suspend fun inboxWithActions(identity: String, wait: Int? = null): InboxResponse = throw ApiException(404, "feature_disabled")
    suspend fun filteredInboxWithActions(identity: String): List<Message> = throw ApiException(404, "feature_disabled")
    suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String> = emptyList()): SendResponse = throw ApiException(404, "feature_disabled")
    suspend fun ackWithEdits(identity: String, messageId: String) = ack(identity, messageId)
    suspend fun messageInfo(identity: String, messageId: String): MessageInfo = throw ApiException(404, "feature_disabled")
    suspend fun openThread(identity: String, peer: String, title: String): String
    suspend fun deleteThread(identity: String, id: String)
    suspend fun deleteMessage(identity: String, id: String)
    suspend fun reportSpam(identity: String, id: String)
    suspend fun setArchived(identity: String, peer: String, archived: Boolean)
    suspend fun saveContact(identity: String, contact: Contact)
    suspend fun removeContact(identity: String, peer: String)
    suspend fun upload(identity: String, file: File, filename: String, mediaType: String): Attachment
    suspend fun download(identity: String, id: String, output: OutputStream, maximumBytes: Long = MAX_ATTACHMENT_BYTES)
    suspend fun handleOffer(): HandleOffer
    suspend fun checkHandle(name: String): HandleAvailability
    suspend fun claimHandle(name: String): HandleOffer

    // Phase 1b and 3a. An older postbox answers 404 for each; the defaults say "off".
    suspend fun capabilities(): Capabilities = Capabilities()
    suspend fun policy(identity: String): FilterPolicy = throw ApiException(404, "feature_disabled")
    suspend fun setPolicy(identity: String, field: String, value: String): FilterPolicy = throw ApiException(404, "feature_disabled")
    suspend fun filteredInbox(identity: String): List<Message> = emptyList()
    suspend fun acceptFiltered(identity: String, peer: String) { throw ApiException(404, "feature_disabled") }
    suspend fun blockedContacts(identity: String): List<Contact> = contacts(identity).contacts.orEmpty().filter { it.admission == "block" }
    suspend fun unblock(identity: String, peer: String) = removeContact(identity, peer)
    suspend fun callLog(identity: String, filter: String = "all", before: Long? = null, limit: Int = 50): CallLogPage = throw ApiException(404, "feature_disabled")
    suspend fun markCallsSeen(identity: String, upTo: Long): Int = 0
    suspend fun deleteCall(identity: String, id: String) { throw ApiException(404, "feature_disabled") }
    suspend fun clearCalls(identity: String) { throw ApiException(404, "feature_disabled") }
    suspend fun reportCall(identity: String, callId: String) { throw ApiException(404, "feature_disabled") }
    suspend fun avatars(keys: List<String>): Map<String, AvatarInfo?> = emptyMap()
    suspend fun avatarBytes(path: String): ByteArray = throw ApiException(404, "not_found")
    suspend fun ownAvatar(identity: String): AvatarInfo? = null
    suspend fun setAvatar(identity: String, image: ByteArray, mediaType: String, crop: CropRect, visibility: String, all: Boolean): AvatarInfo? = throw ApiException(404, "feature_disabled")
    suspend fun setAvatarVisibility(identity: String, visibility: String): AvatarInfo? = throw ApiException(404, "feature_disabled")
    suspend fun removeAvatar(identity: String) { throw ApiException(404, "feature_disabled") }
    suspend fun reportAvatar(identity: String, address: String) { throw ApiException(404, "feature_disabled") }
    suspend fun setThreadTimer(identity: String, threadId: String, seconds: Long?) { throw ApiException(404, "feature_disabled") }
}

const val MAX_ATTACHMENT_BYTES = 20L * 1024 * 1024
const val MAX_ATTACHMENTS = 8

interface DevicePushApi {
    suspend fun registerPushDevice(identity: String, token: String)
    suspend fun unregisterPushDevice(token: String)
}

class PostboxClient(
    private val tokens: TokenProvider,
    private val base: HttpUrl = "https://postbox.pigeonpost.dev/".toHttpUrl(),
    private val allowLoopbackForTests: Boolean = false,
    /** The inbox origin, where the calls service lives (`/calls/v1`). */
    private val inbox: HttpUrl = "https://inbox.pigeonpost.dev/".toHttpUrl(),
) : PostboxApi, PaidHandleApi, AccountHandleApi, DevicePushApi, GroupTransport, CalendarTransport, CalendarMeetingApi {
    init {
        require(base.isHttps || allowLoopbackForTests && base.host in setOf("127.0.0.1", "localhost", "::1", "10.0.2.2")) { "The postbox must use HTTPS." }
        require(base.username.isEmpty() && base.password.isEmpty() && base.query == null && base.fragment == null && base.encodedPath == "/") { "Expected a postbox origin." }
    }
    private val http = OkHttpClient.Builder()
        .followRedirects(false).followSslRedirects(false).retryOnConnectionFailure(false)
        .connectTimeout(20, TimeUnit.SECONDS).readTimeout(60, TimeUnit.SECONDS).writeTimeout(120, TimeUnit.SECONDS)
        .callTimeout(180, TimeUnit.SECONDS).build()
    // A read or a receipt verification can safely recover from a dropped connection. Use a fresh
    // connection for that one retry; never evict connections belonging to other in-flight calls.
    private val recoveryHttp = http.newBuilder().retryOnConnectionFailure(true)
        .connectionPool(ConnectionPool(0, 1, TimeUnit.SECONDS)).build()

    override suspend fun identities() = decode<IdentitiesResponse>(request("identities")).identities.orEmpty()
    override suspend fun registerPushDevice(identity: String, token: String) {
        request("devices", "POST", json = buildJsonObject {
            put("identity", identity); put("token", token); put("platform", "fcm"); put("environment", "production")
        }, retryable = true)
    }
    override suspend fun unregisterPushDevice(token: String) {
        require(token.isNotBlank() && token.length <= 4096 && token.all { it.isLetterOrDigit() || it in ":_-" })
        request("devices/$token", "DELETE", retryable = true)
    }
    override suspend fun accountHandles() = decode<AccountHandlesResponse>(request("me/handles", query = mapOf("include_inactive" to "true"))).handles
    override suspend fun playCatalog() = decode<PlayCatalog>(request("claims/google"))
    override suspend fun redeemPlayPurchase(token: String, name: String?) = decode<PlayClaim>(request("claims/google", "POST", json = buildJsonObject {
        put("purchase_token", token); name?.let { put("namespace", tidyHandle(it)) }
    }, retryable = true)) // The server durably binds this same receipt to this account before acknowledging it.
    override suspend fun assignPlayHandle(productId: String, name: String) = decode<PlayClaim>(request("claims/google/assign", "POST", json = buildJsonObject {
        put("product_id", productId); put("namespace", tidyHandle(name))
    }))
    override suspend fun whoami(identity: String) = decode<WhoAmI>(request("whoami", identity = identity))
    override suspend fun createIdentity(handle: String?) = decode<CreatedIdentity>(request("identities", "POST", json = buildJsonObject { handle?.let { put("handle", it) } })).address
    override suspend fun inbox(identity: String, wait: Int?) = inboxRequest(identity, wait, actions = false)
    override suspend fun inboxWithActions(identity: String, wait: Int?) = inboxRequest(identity, wait, actions = true)
    private suspend fun inboxRequest(identity: String, wait: Int?, actions: Boolean): InboxResponse {
        require(wait == null || wait in 0..25)
        val query = buildMap {
            put("include_sent", "true"); put("include_read", "true"); wait?.let { put("wait", it.toString()) }
            if (actions) { put("actions", "1"); put("fold", "1") }
        }
        return decode(request("inbox", identity = identity, query = query))
    }
    override suspend fun threads(identity: String) = decode<ThreadsResponse>(request("threads", identity = identity)).threads.orEmpty()
    override suspend fun contacts(identity: String) = decode<ContactsResponse>(request("contacts", identity = identity))
    override suspend fun archive(identity: String) = decode<ArchiveResponse>(request("archive", identity = identity)).archived.orEmpty().toSet()
    override suspend fun quota(identity: String) = decode<Quota>(request("quota", identity = identity))
    override suspend fun handleOffer() = decode<HandleOffer>(request("claims/test"))
    override suspend fun checkHandle(name: String): HandleAvailability {
        require(validHandleName(name))
        return decode(request("handles/${tidyHandle(name)}/availability"))
    }
    override suspend fun claimHandle(name: String): HandleOffer {
        require(validHandleName(name))
        return decode(request("claims/test", method = "POST", json = buildJsonObject { put("namespace", tidyHandle(name)) }))
    }

    override suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String>): SendResponse {
        return sendRequest(identity, to, body, threadId, attachments, null)
    }
    override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
        require(action.editOf == null || attachments.isEmpty()) { "Edits are text only." }
        return sendRequest(identity, to, body, threadId, attachments, action)
    }
    private suspend fun sendRequest(identity: String, to: String, body: String, threadId: String?, attachments: List<String>, action: MessageSendAction?): SendResponse {
        require(validAddress(to)) { "Enter an address such as /name/agent." }
        require(attachments.size <= MAX_ATTACHMENTS)
        return decode(request("send", "POST", json = buildJsonObject {
            put("from", identity); put("to", to); put("body", body)
            action?.let {
                put("msg_ref", it.msgRef)
                it.replyTo?.let { ref -> put("reply_to", ref) }
                it.quote?.let { quote -> put("quote", quote) }
                it.editOf?.let { ref -> put("edit_of", ref) }
            }
            threadId?.takeIf { it.isNotEmpty() }?.let { put("thread_id", it) }
            if (attachments.isNotEmpty()) putJsonArray("attachments") { attachments.forEach { add(JsonPrimitive(it)) } }
        }))
    }
    override suspend fun ack(identity: String, messageId: String) { request("ack", "POST", json = messageBody(identity, messageId)) }
    override suspend fun ackWithEdits(identity: String, messageId: String) {
        request("ack", "POST", json = buildJsonObject { put("identity", identity); put("message_id", safeId(messageId)); put("with_edits", true) })
    }
    override suspend fun messageInfo(identity: String, messageId: String): MessageInfo = decode(request("messages/${safeId(messageId)}/info", identity = identity))
    override suspend fun deleteMessage(identity: String, id: String) { request("messages/delete", "POST", json = messageBody(identity, id)) }
    override suspend fun reportSpam(identity: String, id: String) { request("report-spam", "POST", json = messageBody(identity, id)) }
    override suspend fun openThread(identity: String, peer: String, title: String): String = decode<OpenedThread>(request("threads", "POST", json = buildJsonObject {
        put("identity", identity); put("peer", peer); put("title", title)
    })).id
    override suspend fun deleteThread(identity: String, id: String) { request("threads", "DELETE", identity = identity, id = safeId(id)) }
    override suspend fun setArchived(identity: String, peer: String, archived: Boolean) { request("archive", "PUT", json = buildJsonObject {
        put("identity", identity); put("peer", peer); put("archived", archived)
    }) }
    override suspend fun saveContact(identity: String, contact: Contact) {
        require(validAddress(contact.peer, wildcard = true)) { "Enter a sender address or /name/*." }
        require(contact.admission in setOf("allow", "block") && contact.autonomy in setOf("review", "auto"))
        request("contacts", "PUT", json = buildJsonObject {
            put("identity", identity); put("peer", contact.peer); put("admission", contact.admission); put("autonomy", contact.autonomy)
            contact.alias?.takeIf { it.isNotBlank() }?.let { put("alias", it) }
            putJsonArray("allowed_verbs") { if (contact.autonomy == "auto") contact.allowedVerbs.orEmpty().distinct().sorted().forEach { add(JsonPrimitive(it)) } }
        })
    }
    override suspend fun removeContact(identity: String, peer: String) { request("contacts", "PUT", json = buildJsonObject {
        put("identity", identity); put("peer", peer); put("remove", true)
    }) }

    override suspend fun upload(identity: String, file: File, filename: String, mediaType: String): Attachment = withContext(Dispatchers.IO) {
        require(file.isFile && file.length() in 1..MAX_ATTACHMENT_BYTES) { "Attachments must be between 1 byte and 20 MB." }
        val request = Request.Builder().url(url("attachments"))
            .header("x-pigeonpost-identity", identity)
            .header("x-pigeonpost-filename", headerSafe(filename).ifEmpty { "attachment" })
            .header("x-pigeonpost-media-type", headerSafe(mediaType))
            .post(file.asRequestBody("application/octet-stream".toMediaType())).build()
        authenticated(request).use { decode(readAuthorized(it)) }
    }
    override suspend fun download(identity: String, id: String, output: OutputStream, maximumBytes: Long): Unit = withContext(Dispatchers.IO) {
        require(maximumBytes in 1..MAX_ATTACHMENT_BYTES)
        val request = Request.Builder().url(url("attachments", id = safeId(id))).header("x-pigeonpost-identity", identity).build()
        authenticated(request).use { response ->
            requireCurrent(response)
            val authorization = response.request.tag(AuthorizationLease::class.java)
            val body = response.body ?: throw IOException("The attachment is empty.")
            if (body.contentLength() > maximumBytes) throw IOException("This attachment is too large to download here.")
            body.byteStream().use { input ->
                val buffer = ByteArray(32 * 1024)
                var total = 0L
                while (true) {
                    currentCoroutineContext().ensureActive()
                    val count = input.read(buffer)
                    if (count < 0) break
                    total += count
                    if (total > maximumBytes) throw IOException("This attachment is too large to download here.")
                    if (authorization == null) output.write(buffer, 0, count)
                    else authorization.withActive { output.write(buffer, 0, count) }
                }
                requireCurrent(response)
            }
        }
    }

    override suspend fun capabilities(): Capabilities = try { decode(request("capabilities")) }
        catch (failure: ApiException) { if (failure.status in setOf(400, 404, 501)) Capabilities() else throw failure }
    override suspend fun policy(identity: String) = decode<FilterPolicy>(request("policy", identity = identity))
    override suspend fun setPolicy(identity: String, field: String, value: String): FilterPolicy {
        require(POLICY_ROWS.any { row -> row.field == field && row.options.any { it.first == value } }) { "Unknown setting." }
        return decode(request("policy", "PUT", json = buildJsonObject {
            put("identity", identity)
            when (field) {
                "call_history_days" -> put(field, value.toInt())
                "read_receipts" -> put(field, value.toBooleanStrict())
                else -> put(field, value)
            }
        }))
    }
    override suspend fun filteredInbox(identity: String) = decode<InboxResponse>(request("inbox", identity = identity,
        query = mapOf("filtered" to "1", "include_read" to "true"))).messages.orEmpty().filter { !it.outgoing }
    override suspend fun filteredInboxWithActions(identity: String) = decode<InboxResponse>(request("inbox", identity = identity,
        query = mapOf("filtered" to "1", "include_read" to "true", "actions" to "1", "fold" to "1"))).messages.orEmpty().filter { !it.outgoing }
    override suspend fun acceptFiltered(identity: String, peer: String) {
        require(validAddress(peer))
        request("filtered/accept", "POST", json = buildJsonObject { put("identity", identity); put("peer", peer) })
    }
    override suspend fun blockedContacts(identity: String) = decode<ContactsResponse>(request("contacts", identity = identity,
        query = mapOf("admission" to "block"))).contacts.orEmpty().filter { it.admission == "block" }
    override suspend fun unblock(identity: String, peer: String) {
        require(validAddress(peer, wildcard = true))
        request("contacts", "DELETE", identity = identity, query = mapOf("peer" to peer))
    }
    override suspend fun callLog(identity: String, filter: String, before: Long?, limit: Int): CallLogPage {
        require(filter in setOf("all", "missed", "filtered") && limit in 1..200)
        return decode(request("calls/log", identity = identity, query = buildMap {
            put("filter", filter); put("limit", limit.toString()); before?.let { put("before", it.toString()) }
        }))
    }
    override suspend fun markCallsSeen(identity: String, upTo: Long) = decode<CallsSeen>(request("calls/log/seen", "POST", json = buildJsonObject {
        put("identity", identity); put("up_to", upTo)
    })).unseenMissed
    override suspend fun deleteCall(identity: String, id: String) { request("calls/log", "DELETE", identity = identity, id = safeId(id)) }
    override suspend fun clearCalls(identity: String) { request("calls/log", "DELETE", identity = identity) }
    override suspend fun reportCall(identity: String, callId: String) {
        request("report-spam", "POST", json = buildJsonObject { put("identity", identity); put("call_id", safeId(callId)) })
    }
    override suspend fun avatars(keys: List<String>): Map<String, AvatarInfo?> {
        val wanted = keys.filter { it.startsWith('/') && !it.endsWith("/*") && it.length <= 300 && !it.contains(',') }.distinct().take(100)
        if (wanted.isEmpty()) return emptyMap()
        return decode<AvatarsResponse>(request("avatars", query = mapOf("addresses" to wanted.joinToString(",")))).avatars.orEmpty()
    }
    override suspend fun avatarBytes(path: String): ByteArray = withContext(Dispatchers.IO) {
        require(path.startsWith("/v1/avatar-blobs/") && !path.contains("..") && !path.contains('?') && path.length < 300) { "Unexpected picture address." }
        val url = base.newBuilder().addPathSegments(path.removePrefix("/")).build()
        authenticated(Request.Builder().url(url).build()).use { response -> limitedBytes(response, 2L * 1024 * 1024) }
    }
    override suspend fun ownAvatar(identity: String) = decode<OwnAvatarResponse>(request("profile/avatar", identity = identity)).avatar
    override suspend fun setAvatar(identity: String, image: ByteArray, mediaType: String, crop: CropRect, visibility: String, all: Boolean): AvatarInfo? = withContext(Dispatchers.IO) {
        require(image.isNotEmpty() && image.size <= 10 * 1024 * 1024) { "That picture is too large. Choose one under 10 MB." }
        require(mediaType in setOf("image/jpeg", "image/png", "image/webp") && visibility in setOf("everyone", "known", "nobody"))
        val url = url("profile/avatar", identity, buildMap {
            put("crop", "${crop.x},${crop.y},${crop.size},${crop.size}"); put("visibility", visibility); if (all) put("all", "1")
        })
        val request = Request.Builder().url(url).put(image.toRequestBody(mediaType.toMediaType())).build()
        val reply = decode<OwnAvatarResponse>(authenticated(request).use { readJson(it) })
        reply.avatar ?: reply.avatars?.firstOrNull { it.address == null || it.address == identity }?.avatar ?: reply.avatars?.firstOrNull()?.avatar
    }
    override suspend fun setAvatarVisibility(identity: String, visibility: String): AvatarInfo? {
        require(visibility in setOf("everyone", "known", "nobody"))
        return decode<OwnAvatarResponse>(request("profile/avatar", "PATCH", json = buildJsonObject { put("identity", identity); put("visibility", visibility) })).avatar
    }
    override suspend fun removeAvatar(identity: String) { request("profile/avatar", "DELETE", identity = identity) }
    override suspend fun reportAvatar(identity: String, address: String) {
        require(validAddress(address))
        request("report-avatar", "POST", json = buildJsonObject { put("identity", identity); put("address", address) })
    }
    override suspend fun setThreadTimer(identity: String, threadId: String, seconds: Long?) {
        require(seconds in DISAPPEARING_TIMERS) { "Choose one of the offered timers." }
        request("threads", "PATCH", identity = identity, id = safeId(threadId), json = buildJsonObject {
            put("identity", identity); put("ephemeral_secs", seconds?.let { JsonPrimitive(it) } ?: JsonNull)
        })
    }

    // ---- calendar (docs/calendar.md) -------------------------------------------------------------

    override suspend fun calendarRequest(mailbox: String, method: String, path: String, query: Map<String, String>, body: ByteArray?, mediaType: String): ByteArray = withContext(
        Dispatchers.IO + (currentCoroutineContext()[AuthorizationContext] ?: AuthorizationContext(tokens.authorizationLease?.child()))
    ) {
        require(method in setOf("GET", "POST", "PUT", "PATCH", "DELETE")) { "Unexpected calendar method." }
        require(mediaType in setOf("application/json", "text/calendar")) { "Unexpected calendar content type." }
        require(path.length < 2048 && Regex("[A-Za-z0-9_./-]+").matches(path) &&
            (path == "/v1/calendars" || path.startsWith("/v1/calendars/") || path.startsWith("/v1/calendar/")) &&
            path.split('/').drop(1).all { it.isNotEmpty() && it != "." && it != ".." }) { "Unexpected calendar route." }
        require(!path.startsWith("/v1/calendar/guest/") && !path.startsWith("/v1/calendar/feed/")) { "Expected a member calendar route." }
        require(body == null || body.size <= 512 * 1024) { "Calendar requests must be at most 512 KB." }
        require(method != "GET" || body == null) { "Calendar reads have no request body." }
        val target = base.newBuilder().encodedPath(path).apply { query.forEach { (key, value) -> addQueryParameter(key, value) } }.build()
        require(target.encodedPath == path) { "Unexpected calendar route." }
        val bytes = body?.copyOf() ?: if (method in setOf("POST", "PUT", "PATCH")) "{}".toByteArray() else null
        val request = Request.Builder().url(target).acting(mailbox, null).header("Accept", if (path.endsWith(".ics")) "text/calendar" else "application/json")
            .method(method, bytes?.toRequestBody(mediaType.toMediaType())).build()
        try { authenticated(request).use { limitedBytes(it, 16L * 1024 * 1024) } }
        catch (failure: IOException) {
            currentCoroutineContext().ensureActive()
            if (method != "GET" || failure is ApiException || failure is SessionExpired || failure is SSLException) throw failure
            delay(200)
            authenticated(request, recoveryHttp).use { limitedBytes(it, 16L * 1024 * 1024) }
        }
    }

    override suspend fun scheduledMeeting(mailbox: String, code: String): CalendarMeetingStatus = withContext(
        Dispatchers.IO + (currentCoroutineContext()[AuthorizationContext] ?: AuthorizationContext(tokens.authorizationLease?.child()))
    ) {
        val normalized = requireNotNull(CalendarMeetingCode.parse(code)) { "This meeting code is invalid." }
        require(Regex("/k/[A-Za-z0-9_]{1,157}").matches(mailbox)) { "Choose a valid meeting inbox." }
        require(inbox.isHttps || allowLoopbackForTests && inbox.host in setOf("127.0.0.1", "localhost", "::1", "10.0.2.2")) { "The call service must use HTTPS." }
        require(inbox.username.isEmpty() && inbox.password.isEmpty() && inbox.query == null && inbox.fragment == null && inbox.encodedPath == "/") { "Expected a call-service origin." }
        val url = inbox.newBuilder().addPathSegments("calls/v1/meetings/scheduled").addPathSegment(normalized).addQueryParameter("as", mailbox).build()
        val request = Request.Builder().url(url).build()
        suspend fun read(transport: OkHttpClient): CalendarMeetingStatus {
            val response = authenticated(request, transport).use { reply ->
                AuthorizedJson(limitedBytes(reply, 1024L * 1024).toString(Charsets.UTF_8), reply.request.tag(AuthorizationLease::class.java))
            }
            return decode(response)
        }
        try { read(http) }
        catch (failure: IOException) {
            currentCoroutineContext().ensureActive()
            if (failure is ApiException || failure is SessionExpired || failure is SSLException) throw failure
            delay(200); read(recoveryHttp)
        }
    }

    // ---- groups (docs/groups.md) ----------------------------------------------------------------

    private fun groupUrl(path: String): HttpUrl {
        require(path.startsWith("/v1/") && !path.contains("..") && !path.contains('#') && path.length < 2048) { "Unexpected group route." }
        return (base.toString().trimEnd('/') + path).toHttpUrl()
    }
    private fun Request.Builder.acting(mailbox: String, device: String?) = apply {
        require(validAddress(mailbox)) { "Unexpected mailbox." }
        header("x-pigeonpost-identity", mailbox)
        device?.let { require(Regex("[A-Za-z0-9._-]{1,64}").matches(it)); header("x-pigeonpost-device", it) }
    }
    override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String = withContext(
        Dispatchers.IO + (currentCoroutineContext()[AuthorizationContext] ?: AuthorizationContext(tokens.authorizationLease?.child()))
    ) {
        val payload = body?.toString()?.toRequestBody("application/json".toMediaType())
            ?: if (method == "POST" || method == "PUT" || method == "PATCH") "{}".toRequestBody("application/json".toMediaType()) else null
        val request = Request.Builder().url(groupUrl(path)).acting(mailbox, device).method(method, payload).build()
        try { authenticated(request).use { readJson(it) } }
        catch (failure: IOException) {
            currentCoroutineContext().ensureActive()
            // Reads are safe to repeat; every mutating group route carries an idempotency key or is naturally idempotent.
            if (method != "GET" || failure is ApiException || failure is SessionExpired || failure is SSLException) throw failure
            delay(200)
            authenticated(request, recoveryHttp).use { readJson(it) }
        }
    }
    override suspend fun groupUpload(mailbox: String, path: String, bytes: ByteArray, device: String): String = withContext(Dispatchers.IO) {
        val request = Request.Builder().url(groupUrl(path)).acting(mailbox, device)
            .post(bytes.toRequestBody("application/octet-stream".toMediaType())).build()
        authenticated(request).use { readJson(it) }
    }
    override suspend fun groupDownload(mailbox: String, path: String, device: String?, maximumBytes: Long): ByteArray = withContext(Dispatchers.IO) {
        val request = Request.Builder().url(groupUrl(path)).acting(mailbox, device).build()
        authenticated(request).use { limitedBytes(it, maximumBytes) }
    }
    override suspend fun groupCall(groupId: String, from: String): GroupCallState? = withContext(Dispatchers.IO) {
        require(Regex("grp_[a-z2-7]{26}").matches(groupId) && validAddress(from))
        val url = inbox.newBuilder().addPathSegments("calls/v1/groups").addPathSegment(groupId).addPathSegment("call").addQueryParameter("from", from).build()
        try { decode<GroupCallState>(authenticated(Request.Builder().url(url).build()).use { readAuthorized(it) }) }
        catch (cancelled: kotlinx.coroutines.CancellationException) { throw cancelled }
        catch (_: IOException) { null }
    }

    private fun limitedBytes(response: Response, max: Long): ByteArray {
        requireCurrent(response)
        val body = response.body ?: return ByteArray(0)
        if (body.contentLength() > max) throw ApiException(response.code, "response_too_large", "The response is too large.")
        val source = body.source()
        if (source.request(max + 1) && source.buffer.size > max) throw ApiException(response.code, "response_too_large", "The response is too large.")
        val bytes = source.readByteArray()
        requireCurrent(response)
        return bytes
    }

    private fun messageBody(identity: String, id: String) = buildJsonObject { put("identity", identity); put("message_id", id) }
    private fun url(path: String, identity: String? = null, query: Map<String, String> = emptyMap(), id: String? = null) = base.newBuilder().addPathSegment("v1").addPathSegments(path).apply {
        id?.let { addPathSegment(it) }; identity?.let { addQueryParameter("identity", it) }
        query.forEach { (key, value) -> addQueryParameter(key, value) }
    }.build()
    private suspend fun request(path: String, method: String = "GET", identity: String? = null, query: Map<String, String> = emptyMap(), json: JsonObject? = null, id: String? = null, retryable: Boolean = method == "GET"): AuthorizedJson = withContext(
        Dispatchers.IO + (currentCoroutineContext()[AuthorizationContext] ?: AuthorizationContext(tokens.authorizationLease?.child()))
    ) {
        val body: RequestBody? = json?.toString()?.toRequestBody("application/json".toMediaType())
        val request = Request.Builder().url(url(path, identity, query, id)).method(method, body).build()
        try {
            authenticated(request).use { readAuthorized(it) }
        } catch (failure: IOException) {
            currentCoroutineContext().ensureActive()
            if (!retryable || failure is ApiException || failure is SessionExpired || failure is SSLException) throw failure
            delay(200)
            try {
                authenticated(request, recoveryHttp).use { readAuthorized(it) }
            } catch (retryFailure: IOException) {
                currentCoroutineContext().ensureActive()
                if (retryFailure is ApiException || retryFailure is SessionExpired || retryFailure is SSLException) throw retryFailure
                throw PostboxConnectionInterrupted(retryFailure)
            }
        }
    }
    private suspend fun authenticated(request: Request, transport: OkHttpClient = http): Response {
        val context = currentCoroutineContext()[AuthorizationContext]
        val sourceGrant = tokens.authorizationLease
        if (context != null && context.lease == null && sourceGrant != null) throw AuthorizationRetired()
        val requestLease = (context?.lease ?: sourceGrant)?.child()
        val provider = if (requestLease == null) tokens else ScopedTokenProvider(tokens, requestLease)
        fun authorized(token: String) = request.newBuilder().tag(AuthorizationLease::class.java, requestLease)
            .header("Authorization", "Bearer $token").header("Accept", request.header("Accept") ?: "application/json").build()
        val first = provider.token()
        var response = transport.newCall(authorized(first)).await()
        requireCurrent(response)
        if (response.code == 401) {
            response.close()
            val renewed = provider.token(rejected = first)
            response = transport.newCall(authorized(renewed)).await()
            requireCurrent(response)
            if (response.code == 401) {
                response.close(); provider.invalidate(renewed); throw SessionExpired()
            }
        }
        if (!response.isSuccessful) response.use {
            val problem = try { WireJson.parseToJsonElement(readJson(it)) as? JsonObject }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (_: Exception) { null }
            throw ApiException(it.code, (problem?.get("error") as? JsonPrimitive)?.contentOrNull,
                (problem?.get("detail") as? JsonPrimitive)?.contentOrNull?.take(500), problem)
        }
        return response
    }
    private fun requireCurrent(response: Response) {
        try { response.request.tag(AuthorizationLease::class.java)?.requireActive() }
        catch (retired: AuthorizationRetired) { response.close(); throw retired }
    }
    private fun readJson(response: Response): String {
        requireCurrent(response)
        val body = response.body ?: return ""
        val max = 16L * 1024 * 1024
        if (body.contentLength() > max) throw ApiException(response.code, "response_too_large", "The postbox response is too large.")
        val source = body.source()
        if (source.request(max + 1) && source.buffer.size > max) throw ApiException(response.code, "response_too_large", "The postbox response is too large.")
        val text = source.readUtf8()
        requireCurrent(response)
        return text
    }
    private data class AuthorizedJson(val text: String, val authorization: AuthorizationLease?)
    private fun readAuthorized(response: Response) = AuthorizedJson(readJson(response), response.request.tag(AuthorizationLease::class.java))
    private suspend inline fun <reified T> decode(value: AuthorizedJson): T = withContext(AuthorizationContext(value.authorization)) {
        value.authorization?.requireActive()
        decode<T>(value.text)
    }
    private suspend inline fun <reified T> decode(text: String): T = withContext(Dispatchers.Default) {
        val authorization = currentCoroutineContext()[AuthorizationContext]?.lease
        authorization?.requireActive()
        try { WireJson.decodeFromString<T>(text).also { authorization?.requireActive() } }
        catch (e: SerializationException) { throw ApiException(200, "bad_response", "The postbox returned an unreadable response.") }
    }
    private fun safeId(id: String): String {
        require(Regex("[A-Za-z0-9_:-]{1,256}").matches(id)) { "Invalid item identifier." }
        return id
    }
    private fun headerSafe(value: String) = value.filter { it.code in 32..126 && it != '"' && it != '\\' }.take(120)
}

suspend fun Call.await(): Response = suspendCancellableCoroutine { continuation ->
    continuation.invokeOnCancellation { cancel() }
    enqueue(object : Callback {
        override fun onFailure(call: Call, e: IOException) { if (continuation.isActive) continuation.resumeWithException(e) }
        override fun onResponse(call: Call, response: Response) {
            continuation.resume(response) { _, resource, _ -> resource.close() }
        }
    })
}
