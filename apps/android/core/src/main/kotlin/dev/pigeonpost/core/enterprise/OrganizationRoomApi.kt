package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.Group
import dev.pigeonpost.core.groups.GroupPreview
import dev.pigeonpost.core.groups.GroupTransport
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.serialization.json.*
import java.net.URLEncoder

interface OrganizationRoomApi {
    suspend fun organizations(mailbox: String): RoomOrganizations
    suspend fun rooms(mailbox: String, organizationId: String, search: String): List<OrganizationRoom>
    suspend fun preview(mailbox: String, groupId: String): GroupPreview
    suspend fun group(mailbox: String, groupId: String): Group
    suspend fun create(mailbox: String, payload: OrganizationRoomCreate): Group
    suspend fun join(mailbox: String, groupId: String)
    suspend fun acceptInvitation(mailbox: String, groupId: String)
}

/** Reuses the existing accepted-login transport and MLS routes, without weaker encryption. */
class OrganizationRoomClient(private val transport: GroupTransport) : OrganizationRoomApi {
    private fun bad(): Nothing = throw ApiException(200, "bad_response")
    private fun checked(value: String): String { require(OrganizationRoomInput.identifier(value)) { "Invalid room or organization." }; return value }
    private fun path(group: String, suffix: String = "") = "/v1/groups/" + checked(group) + suffix
    private suspend fun <T> accepted(work: suspend () -> T): T {
        val owner = currentCoroutineContext()[AuthorizationContext]?.lease ?: throw AuthorizationRetired()
        owner.requireActive()
        return work().also { owner.requireActive() }
    }
    private suspend fun json(mailbox: String, method: String, path: String, body: JsonObject? = null): JsonObject {
        require(OrganizationRoomInput.mailbox(mailbox)) { "Choose a valid inbox." }
        val owner = currentCoroutineContext()[AuthorizationContext]?.lease ?: throw AuthorizationRetired()
        owner.requireActive()
        val text = transport.groupJson(mailbox, method, path, body)
        owner.requireActive()
        if (text.length > 1024 * 1024 || text.toByteArray(Charsets.UTF_8).size > 1024 * 1024) bad()
        return try { WireJson.parseToJsonElement(text) as? JsonObject ?: bad() } catch (_: IllegalArgumentException) { bad() }
    }
    private inline fun <reified T> decode(value: JsonElement): T = try { WireJson.decodeFromJsonElement(value) } catch (_: IllegalArgumentException) { bad() }
    override suspend fun organizations(mailbox: String): RoomOrganizations = accepted {
        decode<RoomOrganizations>(json(mailbox, "GET", "/v1/orgs")).also { if (!it.valid) bad() }
    }

    override suspend fun rooms(mailbox: String, organizationId: String, search: String): List<OrganizationRoom> {
        val org = checked(organizationId) // Omitting this query would expose the personal public directory instead.
        require(OrganizationRoomInput.text(search, 80)) { "Use at most 80 characters to search." }
        return accepted {
            val result = decode<OrganizationRoomDirectory>(json(mailbox, "GET", "/v1/rooms?org_id=$org&q=" + URLEncoder.encode(search, "UTF-8"))).rooms
            if (result.size > 50 || result.any { !it.valid } || result.map { it.id }.distinct().size != result.size) bad()
            result
        }
    }
    override suspend fun preview(mailbox: String, groupId: String): GroupPreview {
        val id = checked(groupId)
        return accepted { decode<GroupPreview>(json(mailbox, "POST", "/v1/groups/resolve", buildJsonObject { put("group_id", id) })).also {
            if (it.id != id || !OrganizationRoomInput.text(it.name, 256) || it.memberCount < 0 ||
                it.description?.let { text -> !OrganizationRoomInput.text(text, 2000, multiline = true) } == true) bad()
        } }
    }
    private fun confirmed(raw: JsonObject, mailbox: String, id: String? = null, organization: String? = null, created: Boolean = false): Group {
        val my = raw["my"] as? JsonObject ?: bad()
        fun string(objectValue: JsonObject, key: String) = (objectValue[key] as? JsonPrimitive)?.takeIf { it.isString }?.content
        val state = string(my, "state"); val role = string(my, "role")
        if (state !in setOf("active", "keying") || role !in setOf("owner", "admin", "member", "restricted")) bad()
        if (string(raw, "id") == null || string(raw, "org_id") == null) bad()
        val result = decode<Group>(raw)
        if (!OrganizationRoomInput.identifier(result.id) || id?.let { result.id != it } == true || result.orgId?.let(OrganizationRoomInput::identifier) != true ||
            organization?.let { result.orgId != it } == true || result.deletedAt != null ||
            created && (result.createdBy != mailbox || role != "owner")) bad()
        return result
    }
    override suspend fun group(mailbox: String, groupId: String): Group = accepted {
        val value = json(mailbox, "GET", path(groupId))
        // The deployed single-group route returns the object itself; older clients also accept an envelope.
        val raw = if ("group" in value) value["group"] as? JsonObject ?: bad() else value
        confirmed(raw, mailbox, id = groupId)
    }
    override suspend fun create(mailbox: String, payload: OrganizationRoomCreate): Group = accepted {
        val raw = json(mailbox, "POST", "/v1/groups", payload.body)["group"] as? JsonObject ?: bad()
        confirmed(raw, mailbox, organization = payload.organizationId, created = true)
    }
    override suspend fun join(mailbox: String, groupId: String) { accepted { json(mailbox, "POST", path(groupId, "/join"), buildJsonObject {}) } }
    override suspend fun acceptInvitation(mailbox: String, groupId: String) { accepted { json(mailbox, "POST", path(groupId, "/invitation"), buildJsonObject { put("accept", true) }) } }
}
