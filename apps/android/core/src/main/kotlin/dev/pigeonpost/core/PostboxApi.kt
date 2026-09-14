package dev.pigeonpost.core

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException
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
    /** A rejected token is renewed only if it is still the current token. */
    suspend fun token(rejected: String? = null): String
    suspend fun invalidate(rejected: String) {}
}

class SessionExpired : IOException("Your session expired. Sign in again.")
class PostboxConnectionInterrupted(cause: IOException) : IOException("The connection was interrupted. Please try again.", cause)
class ApiException(val status: Int, val code: String?, detail: String? = null) : IOException(detail ?: when (code) {
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
    allowLoopbackForTests: Boolean = false,
) : PostboxApi, PaidHandleApi, AccountHandleApi, DevicePushApi {
    init {
        require(base.isHttps || allowLoopbackForTests && base.host in setOf("127.0.0.1", "localhost", "::1")) { "The postbox must use HTTPS." }
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
    override suspend fun inbox(identity: String, wait: Int?): InboxResponse {
        require(wait == null || wait in 0..25)
        val query = buildMap { put("include_sent", "true"); put("include_read", "true"); wait?.let { put("wait", it.toString()) } }
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
        require(validAddress(to)) { "Enter an address such as /name/agent." }
        require(attachments.size <= MAX_ATTACHMENTS)
        return decode(request("send", "POST", json = buildJsonObject {
            put("from", identity); put("to", to); put("body", body)
            threadId?.takeIf { it.isNotEmpty() }?.let { put("thread_id", it) }
            if (attachments.isNotEmpty()) putJsonArray("attachments") { attachments.forEach { add(JsonPrimitive(it)) } }
        }))
    }
    override suspend fun ack(identity: String, messageId: String) { request("ack", "POST", json = messageBody(identity, messageId)) }
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
        authenticated(request).use { decode(readJson(it)) }
    }
    override suspend fun download(identity: String, id: String, output: OutputStream, maximumBytes: Long): Unit = withContext(Dispatchers.IO) {
        require(maximumBytes in 1..MAX_ATTACHMENT_BYTES)
        val request = Request.Builder().url(url("attachments", id = safeId(id))).header("x-pigeonpost-identity", identity).build()
        authenticated(request).use { response ->
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
                    output.write(buffer, 0, count)
                }
            }
        }
    }

    private fun messageBody(identity: String, id: String) = buildJsonObject { put("identity", identity); put("message_id", id) }
    private fun url(path: String, identity: String? = null, query: Map<String, String> = emptyMap(), id: String? = null) = base.newBuilder().addPathSegment("v1").addPathSegments(path).apply {
        id?.let { addPathSegment(it) }; identity?.let { addQueryParameter("identity", it) }
        query.forEach { (key, value) -> addQueryParameter(key, value) }
    }.build()
    private suspend fun request(path: String, method: String = "GET", identity: String? = null, query: Map<String, String> = emptyMap(), json: JsonObject? = null, id: String? = null, retryable: Boolean = method == "GET"): String = withContext(Dispatchers.IO) {
        val body: RequestBody? = json?.toString()?.toRequestBody("application/json".toMediaType())
        val request = Request.Builder().url(url(path, identity, query, id)).method(method, body).build()
        try {
            authenticated(request).use { readJson(it) }
        } catch (failure: IOException) {
            currentCoroutineContext().ensureActive()
            if (!retryable || failure is ApiException || failure is SessionExpired || failure is SSLException) throw failure
            delay(200)
            try {
                authenticated(request, recoveryHttp).use { readJson(it) }
            } catch (retryFailure: IOException) {
                currentCoroutineContext().ensureActive()
                if (retryFailure is ApiException || retryFailure is SessionExpired || retryFailure is SSLException) throw retryFailure
                throw PostboxConnectionInterrupted(retryFailure)
            }
        }
    }
    private suspend fun authenticated(request: Request, transport: OkHttpClient = http): Response {
        val first = tokens.token()
        var response = transport.newCall(request.newBuilder().header("Authorization", "Bearer $first").header("Accept", "application/json").build()).await()
        if (response.code == 401) {
            response.close()
            val renewed = tokens.token(rejected = first)
            response = transport.newCall(request.newBuilder().header("Authorization", "Bearer $renewed").header("Accept", "application/json").build()).await()
            if (response.code == 401) {
                response.close(); tokens.invalidate(renewed); throw SessionExpired()
            }
        }
        if (!response.isSuccessful) response.use {
            val problem = runCatching { WireJson.parseToJsonElement(readJson(it)) as? JsonObject }.getOrNull()
            throw ApiException(it.code, (problem?.get("error") as? JsonPrimitive)?.contentOrNull, (problem?.get("detail") as? JsonPrimitive)?.contentOrNull?.take(500))
        }
        return response
    }
    private fun readJson(response: Response): String {
        val body = response.body ?: return ""
        val max = 16L * 1024 * 1024
        if (body.contentLength() > max) throw ApiException(response.code, "response_too_large", "The postbox response is too large.")
        val source = body.source()
        if (source.request(max + 1) && source.buffer.size > max) throw ApiException(response.code, "response_too_large", "The postbox response is too large.")
        return source.readUtf8()
    }
    private suspend inline fun <reified T> decode(text: String): T = withContext(Dispatchers.Default) {
        try { WireJson.decodeFromString(text) }
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
