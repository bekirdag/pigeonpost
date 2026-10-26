package dev.pigeonpost.core.groups

import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.validAddress
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement

/**
 * The postbox's group routes (docs/groups.md). Every call names the acting mailbox
 * (`x-pigeonpost-identity`) and, for device-scoped routes, this device (`x-pigeonpost-device`).
 * Failures are `ApiException` with the server's `{error, …}` body in `problem`.
 */
interface GroupTransport {
    /** `path` starts with `/v1/` and may carry a query string. Returns the JSON text. */
    suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement? = null, device: String? = null): String
    suspend fun groupUpload(mailbox: String, path: String, bytes: ByteArray, device: String): String
    suspend fun groupDownload(mailbox: String, path: String, device: String?, maximumBytes: Long): ByteArray
    /** The calls service's "is a call running in this group" (`/calls/v1/groups/{id}/call`). */
    suspend fun groupCall(groupId: String, from: String): GroupCallState? = null
}

/** One canonical acting mailbox and one immutable grant for metadata, messages and files alike. */
internal class AuthorizedGroupTransport(
    private val base: GroupTransport,
    private val mailbox: String,
    private val authorization: AuthorizationLease,
) : GroupTransport {
    init { require(validAddress(mailbox)) }

    private suspend fun <T> request(actor: String, work: suspend () -> T): T {
        authorization.requireActive()
        if (actor != mailbox) throw AuthorizationRetired()
        currentCoroutineContext()[AuthorizationContext]?.lease?.let { caller ->
            caller.requireActive()
            if (!caller.sharesAuthorization(authorization)) throw AuthorizationRetired()
        }
        return withContext(AuthorizationContext(authorization)) {
            authorization.requireActive()
            val result = work()
            authorization.requireActive()
            result
        }
    }

    override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?) =
        request(mailbox) { base.groupJson(mailbox, method, path, body, device) }
    override suspend fun groupUpload(mailbox: String, path: String, bytes: ByteArray, device: String) =
        request(mailbox) { base.groupUpload(mailbox, path, bytes, device) }
    override suspend fun groupDownload(mailbox: String, path: String, device: String?, maximumBytes: Long) =
        request(mailbox) { base.groupDownload(mailbox, path, device, maximumBytes) }
    override suspend fun groupCall(groupId: String, from: String) = request(from) { base.groupCall(groupId, from) }
}
