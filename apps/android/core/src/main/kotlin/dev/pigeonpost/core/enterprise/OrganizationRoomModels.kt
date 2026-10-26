package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.validAddress
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

enum class RoomJoinPolicy(val wire: String) { MEMBERS("public"), INVITED("private") }
enum class RoomPostPolicy(val wire: String) { EVERYONE("everyone"), ADMINS("admins") }
enum class RoomAction { OPEN, ACCEPT_INVITATION, JOIN, UNAVAILABLE }

object OrganizationRoomInput {
    fun identifier(value: String): Boolean = value.length in 1..128 && value.all { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' || it == '_' || it == '-' }
    fun mailbox(value: String) = value.startsWith("/k/") && validAddress(value)
    fun text(value: String, maximum: Int, multiline: Boolean = false): Boolean = value.codePointCount(0, value.length) <= maximum &&
        value.codePoints().noneMatch { cp -> cp in 0xD800..0xDFFF || cp in 0x202A..0x202E || cp in 0x2066..0x2069 ||
            Character.isISOControl(cp) && !(multiline && (cp == 10 || cp == 9)) }
}

@Serializable
data class RoomMailbox(val address: String, val handle: String? = null) {
    val valid get() = OrganizationRoomInput.mailbox(address) && (handle == null || OrganizationRoomInput.text(handle, 256))
}
@Serializable
data class RoomOrganizationSettings(@SerialName("rooms_creation") val roomsCreation: String? = null)
@Serializable
data class RoomOrganization(
    val id: String, val name: String, val status: String,
    @SerialName("my_member_id") val memberId: String,
    val namespace: String? = null, val settings: RoomOrganizationSettings? = null,
    @SerialName("my_roles") val roles: List<String> = emptyList(),
    @SerialName("my_mailbox") val mailbox: RoomMailbox? = null,
) {
    val readable get() = status in setOf("active", "past_due", "read_only")
    val writable get() = status in setOf("active", "past_due")
    val valid get() = OrganizationRoomInput.identifier(id) && OrganizationRoomInput.identifier(memberId) && name.isNotBlank() &&
        OrganizationRoomInput.text(name, 256) && OrganizationRoomInput.identifier(status) && (mailbox?.valid != false) &&
        (namespace == null || OrganizationRoomInput.text(namespace, 128)) && roles.size <= 16 && roles.distinct().size == roles.size && roles.all(OrganizationRoomInput::identifier)
    /** A display hint; the server rechecks the current membership and policy on creation. */
    fun mayCreate(acting: String): Boolean = writable && mailbox?.address == acting && when (settings?.roomsCreation) {
        "everyone" -> true
        "admins" -> roles.any { it in setOf("owner", "admin", "people_admin", "agent_admin", "billing_admin") }
        else -> false
    }
}
@Serializable
data class RoomOperatedMailbox(val address: String, @SerialName("org_id") val organizationId: String,
    val handle: String? = null, val capabilities: List<String> = emptyList()) {
    val valid get() = OrganizationRoomInput.mailbox(address) && OrganizationRoomInput.identifier(organizationId) &&
        (handle == null || OrganizationRoomInput.text(handle, 256)) && capabilities.size <= 16 && capabilities.distinct().size == capabilities.size && capabilities.all(OrganizationRoomInput::identifier)
}
@Serializable
data class RoomOrganizations(val orgs: List<RoomOrganization>, val operated: List<RoomOperatedMailbox> = emptyList()) {
    val valid get() = orgs.size <= 1000 && orgs.all { it.valid } && orgs.map { it.id }.distinct().size == orgs.size &&
        operated.size <= 10_000 && operated.all { it.valid && orgs.any { org -> org.id == it.organizationId } }
    /** Canonical membership and explicit read_inbox delegation, never handle matching. */
    fun forMailbox(mailbox: String): List<RoomOrganization> {
        if (!valid || !OrganizationRoomInput.mailbox(mailbox)) return emptyList()
        val delegated = operated.filter { it.address == mailbox && "read_inbox" in it.capabilities }.map { it.organizationId }.toSet()
        return orgs.filter { it.readable && (it.mailbox?.address == mailbox || it.id in delegated) }
    }
}

@Serializable
data class OrganizationRoom(val id: String, val name: String, val description: String? = null,
    @SerialName("member_count") val memberCount: Int,
    @SerialName("join_policy") val joinPolicy: String,
    @SerialName("my_state") val membership: String? = null) {
    val valid get() = OrganizationRoomInput.identifier(id) && name.isNotBlank() && OrganizationRoomInput.text(name, 256) &&
        (description == null || OrganizationRoomInput.text(description, 2000, multiline = true)) && memberCount >= 0 &&
        OrganizationRoomInput.identifier(joinPolicy) && (membership == null || OrganizationRoomInput.identifier(membership))
    val action get() = when (membership) {
        "active", "keying" -> RoomAction.OPEN
        "invited" -> RoomAction.ACCEPT_INVITATION
        null, "left", "removed" -> if (joinPolicy == "public") RoomAction.JOIN else RoomAction.UNAVAILABLE
        else -> RoomAction.UNAVAILABLE
    }
}
@Serializable
internal data class OrganizationRoomDirectory(val rooms: List<OrganizationRoom>)

/** Exact request identity and bytes remain unchanged through an explicit uncertain-outcome retry. */
class OrganizationRoomCreate(val organizationId: String, name: String, description: String,
    val joinPolicy: RoomJoinPolicy, val postPolicy: RoomPostPolicy, val listed: Boolean, val requestKey: String) {
    val name = name.trim()
    val description = description.trim()
    init {
        require(OrganizationRoomInput.identifier(organizationId)) { "Choose a valid organization." }
        require(requestKey.length >= 8 && OrganizationRoomInput.identifier(requestKey)) { "This creation attempt is invalid." }
        require(this.name.isNotEmpty() && OrganizationRoomInput.text(this.name, 80) && OrganizationRoomInput.text(this.description, 500, multiline = true)) {
            "Use a name of 1–80 characters and a description of at most 500 characters."
        }
    }
    val body: JsonObject = buildJsonObject {
        put("request_key", requestKey); put("org_id", organizationId); put("name", this@OrganizationRoomCreate.name)
        if (this@OrganizationRoomCreate.description.isNotEmpty()) put("description", this@OrganizationRoomCreate.description)
        put("join_policy", joinPolicy.wire); put("post_policy", postPolicy.wire); put("visibility", if (listed) "listed" else "unlisted")
        put("history_visible_to_new_members", false)
    }
    val encoded = body.toString()
    override fun toString() = "OrganizationRoomCreate(redacted)"
}
