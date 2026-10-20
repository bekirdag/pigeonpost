package dev.pigeonpost.core.groups

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
