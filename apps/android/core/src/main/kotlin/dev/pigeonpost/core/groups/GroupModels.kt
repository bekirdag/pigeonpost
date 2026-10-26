package dev.pigeonpost.core.groups

import dev.pigeonpost.core.AuthorizationLease

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject

/** The acting mailbox's own relationship with a group (docs/groups.md §2). */
@Serializable
data class GroupMine(
    val role: String = "member",
    val state: String = "active",
    @SerialName("read_cursor") val readCursor: Long = 0,
    val unread: Int = 0,
    val notify: String = "all",
    @SerialName("muted_until") val mutedUntil: Long? = null,
    @SerialName("invited_by") val invitedBy: String? = null,
    @SerialName("joined_at") val joinedAt: Long? = null,
    val filtered: Boolean = false,
    val revision: Long = 0,
)

@Serializable
data class Group(
    val id: String,
    val name: String = "Group",
    val description: String? = null,
    @SerialName("join_policy") val joinPolicy: String = "private",
    @SerialName("post_policy") val postPolicy: String = "everyone",
    val visibility: String = "unlisted",
    @SerialName("members_can_invite") val membersCanInvite: Boolean = false,
    @SerialName("members_can_change_timer") val membersCanChangeTimer: Boolean = false,
    @SerialName("ephemeral_secs") val ephemeralSecs: Long? = null,
    val revision: Long = 0,
    val epoch: Long = 0,
    @SerialName("activation_state") val activationState: String = "active",
    @SerialName("member_count") val memberCount: Int = 1,
    @SerialName("created_by") val createdBy: String? = null,
    @SerialName("last_at") val lastAt: Long? = null,
    @SerialName("last_seq") val lastSeq: Long = 0,
    @SerialName("deleted_at") val deletedAt: Long? = null,
    @SerialName("has_link") val hasLink: Boolean = false,
    val my: GroupMine? = null,
) {
    val state get() = my?.state
    val active get() = my?.state == "active" && deletedAt == null
    val invited get() = my?.state == "invited" && deletedAt == null
    val admin get() = my?.role == "owner" || my?.role == "admin"
    val owner get() = my?.role == "owner"
    /** Announcement mode and the restricted role both take the composer away. */
    val canPost get() = active && my?.role != "restricted" && (postPolicy != "admins" || admin)
    val mayChangeTimer get() = admin || (membersCanChangeTimer && active)
    val mayInvite get() = admin || (membersCanInvite && active)
    fun muted(now: Long) = (my?.mutedUntil ?: 0) > now
}

@Serializable
data class GroupResponse(val group: Group? = null)
@Serializable
data class GroupsResponse(val groups: List<Group>? = null)
@Serializable
data class CreatedGroup(val group: Group, val invitations: List<InviteResult>? = null)
@Serializable
data class InviteResult(val address: String, val status: String)
@Serializable
data class InviteResults(val results: List<InviteResult>? = null)

@Serializable
data class GroupMember(
    val address: String,
    val handle: String? = null,
    val role: String = "member",
    val state: String = "active",
    @SerialName("invited_by") val invitedBy: String? = null,
    @SerialName("joined_at") val joinedAt: Long? = null,
    val revision: Long = 0,
)
@Serializable
data class MembersResponse(val members: List<GroupMember>? = null)

@Serializable
data class DeviceRow(
    @SerialName("device_id") val deviceId: String,
    val fingerprint: String? = null,
    @SerialName("fingerprint_digits") val fingerprintDigits: String? = null,
    @SerialName("created_at") val createdAt: Long? = null,
)
@Serializable
data class DevicesResponse(val mailbox: String? = null, val devices: List<DeviceRow>? = null)

@Serializable
data class RegisteredDevice(
    @SerialName("device_id") val deviceId: String,
    val fingerprint: String? = null,
    @SerialName("fingerprint_digits") val fingerprintDigits: String? = null,
    val attestation: String,
    @SerialName("mailbox_public_key") val mailboxPublicKey: String,
)

@Serializable
data class KeyPackageCount(val unclaimed: Int = 0, @SerialName("last_resort") val lastResort: Boolean = false)

@Serializable
data class Welcome(@SerialName("welcome_id") val id: String, @SerialName("group_id") val groupId: String, val epoch: Long = 0, val seq: Long = 0, val welcome: String)
@Serializable
data class WelcomesResponse(val welcomes: List<Welcome>? = null)

@Serializable
data class DeviceAddress(val mailbox: String, @SerialName("device_id") val deviceId: String)

/** One item of a group's single ordered sequence: an application message, a commit or a notice. */
@Serializable
data class GroupItem(
    val seq: Long,
    val type: String,
    val epoch: Long? = null,
    val sender: String? = null,
    @SerialName("sender_device") val senderDevice: String? = null,
    @SerialName("client_message_id") val clientMessageId: String? = null,
    val ciphertext: String? = null,
    val commit: String? = null,
    val adds: List<DeviceAddress>? = null,
    val removes: List<DeviceAddress>? = null,
    val kind: String? = null,
    val actor: String? = null,
    val metadata: JsonObject? = null,
    @SerialName("reply_to") val replyTo: Long? = null,
    @SerialName("edit_of") val editOf: Long? = null,
    @SerialName("replaced_by") val replacedBy: Long? = null,
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("expires_at") val expiresAt: Long? = null,
    @SerialName("deleted_at") val deletedAt: Long? = null,
)
@Serializable
data class ItemsResponse(val items: List<GroupItem>? = null, @SerialName("last_seq") val lastSeq: Long = 0, val more: Boolean = false)

@Serializable
data class PendingChange(val kind: String, val mailbox: String, @SerialName("device_id") val deviceId: String)
@Serializable
data class CryptoStatus(
    val epoch: Long = 0,
    val revision: Long = 0,
    @SerialName("activation_state") val activationState: String = "active",
    @SerialName("in_group") val inGroup: Boolean = false,
    val pending: List<PendingChange>? = null,
)

@Serializable
data class Claim(val mailbox: String, @SerialName("device_id") val deviceId: String, @SerialName("key_package") val keyPackage: String, @SerialName("key_package_ref") val keyPackageRef: String)
@Serializable
data class ClaimsResponse(val claims: List<Claim>? = null)
@Serializable
data class Posted(val seq: Long, val epoch: Long? = null, @SerialName("expires_at") val expiresAt: Long? = null)
@Serializable
data class Uploaded(val id: String, val bytes: Long = 0)

@Serializable
data class GroupPreview(
    val id: String,
    val name: String = "Group",
    val description: String? = null,
    @SerialName("member_count") val memberCount: Int = 1,
    @SerialName("join_policy") val joinPolicy: String = "private",
    @SerialName("my_state") val myState: String? = null,
    @SerialName("can_join") val canJoin: Boolean = false,
    val needs: String? = null,
)
@Serializable
data class LinkResponse(@SerialName("link_token") val linkToken: String? = null, val url: String? = null)

/** "Call in progress — Join" (calls service, `/calls/v1/groups/{id}/call`). */
@Serializable
data class GroupCallState(val active: Boolean = false, val kind: String? = null, val participants: List<String>? = null)

@Serializable
data class GroupMemberReceipt(
    val address: String,
    val handle: String? = null,
    @SerialName("delivered_seq") val deliveredSeq: Long = 0,
    @SerialName("seen_seq") val seenSeq: Long? = null,
)
@Serializable
data class GroupReceipts(val members: List<GroupMemberReceipt> = emptyList(), @SerialName("read_receipts") val readReceipts: Boolean = false)

data class GroupDraftKey(val mailbox: String, val groupId: String)
enum class GroupComposerKind { REPLY, EDIT }
data class GroupComposerAction(
    val kind: GroupComposerKind,
    val seq: Long,
    val sender: String?,
    val excerpt: String,
    val ordinaryDraft: String? = null,
    val id: String = java.util.UUID.randomUUID().toString(),
)

// ---- what this device keeps -------------------------------------------------------------------

/** A file inside a group message: shown by name, opened with the key that travelled encrypted. */
@Serializable
data class GroupFile(val id: String, val name: String, val mime: String, val size: Long, val key: String? = null, val sha: String? = null, val len: Long? = null)

@Serializable
data class DeviceSummary(val mailbox: String, @SerialName("device_id") val deviceId: String, val fingerprint: String)

/**
 * One line of a group's history as this device decrypted it. A device cannot decrypt a message
 * twice, so this (kept encrypted at rest) is the only readable copy it will ever have.
 */
@Serializable
data class HistoryItem(
    val kind: String,
    val seq: Long? = null,
    /** Orders a warning just after the item it is about. */
    val after: Int = 0,
    val notice: String? = null,
    val actor: String? = null,
    val metadata: JsonObject? = null,
    val sender: String? = null,
    @SerialName("sender_device") val senderDevice: String? = null,
    val cmid: String? = null,
    val text: String? = null,
    val files: List<GroupFile> = emptyList(),
    @SerialName("reply_to") val replyTo: String? = null,
    @SerialName("edit_of") val editOf: String? = null,
    @SerialName("replaced_by") val replacedBy: Long? = null,
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("expires_at") val expiresAt: Long? = null,
    val deleted: Boolean = false,
    val undecryptable: Boolean = false,
    val code: String? = null,
    val pending: Boolean = false,
    @SerialName("send_failed") val sendFailed: Boolean = false,
    /** Set only when encrypted action fields agree with the sequenced envelope. */
    @SerialName("actions_verified") val actionsVerified: Boolean = false,
    val local: Boolean = false,
    val warning: String? = null,
    val epoch: Long? = null,
    val committer: DeviceSummary? = null,
    val added: List<DeviceSummary> = emptyList(),
    val removed: List<DeviceSummary> = emptyList(),
    val updated: List<DeviceSummary> = emptyList(),
)

@Serializable
data class GroupCursor(val cursor: Long = 0, @SerialName("in_group") val inGroup: Boolean = false, val acked: Long = 0)

/** Stored alongside the MLS sender state in the platform's encrypted group vault. */
@Serializable
data class GroupPendingSend(
    val group: String,
    val cmid: String,
    val fingerprint: String,
    @SerialName("created_at") val createdAt: Long,
    val body: String,
    val files: List<GroupFile>,
    val mentions: List<String> = emptyList(),
    /** Exact prepared JSON. Null only before encryption or after an explicit conflict response. */
    val request: String? = null,
)

/** Everything this device stores for one mailbox, sealed by the platform before it touches disk. */
@Serializable
data class GroupRecord(
    @SerialName("device_id") val deviceId: String,
    val registered: Boolean = false,
    @SerialName("state_key") val stateKey: String,
    @SerialName("state_blob") val stateBlob: String? = null,
    val fingerprint: String? = null,
    val groups: Map<String, GroupCursor> = emptyMap(),
    val history: Map<String, List<HistoryItem>> = emptyMap(),
    @SerialName("pending_sends") val pendingSends: Map<String, GroupPendingSend> = emptyMap(),
)

/** Where the record lives. The Android implementation seals it with a Keystore AES-GCM key. */
interface GroupStorage {
    val persistent: Boolean get() = true
    suspend fun load(mailbox: String): GroupRecord?
    suspend fun save(mailbox: String, record: GroupRecord)
    suspend fun remove(mailbox: String)
    /** Check inside the implementation's serialized local IO boundary, never around a suspend. */
    suspend fun load(mailbox: String, authorization: AuthorizationLease): GroupRecord?
    suspend fun save(mailbox: String, record: GroupRecord, authorization: AuthorizationLease)
    suspend fun remove(mailbox: String, authorization: AuthorizationLease)
}

class MemoryGroupStorage : GroupStorage {
    override val persistent = false
    val rows = mutableMapOf<String, GroupRecord>()
    override suspend fun load(mailbox: String) = synchronized(rows) { rows[mailbox] }
    override suspend fun save(mailbox: String, record: GroupRecord) { synchronized(rows) { rows[mailbox] = record } }
    override suspend fun remove(mailbox: String) { synchronized(rows) { rows.remove(mailbox) } }
    override suspend fun load(mailbox: String, authorization: AuthorizationLease) = synchronized(rows) { authorization.withActive { rows[mailbox] } }
    override suspend fun save(mailbox: String, record: GroupRecord, authorization: AuthorizationLease) { synchronized(rows) { authorization.withActive { rows[mailbox] = record } } }
    override suspend fun remove(mailbox: String, authorization: AuthorizationLease) { synchronized(rows) { authorization.withActive { rows.remove(mailbox) } } }
}

/** `https://inbox.pigeonpost.dev/g/<id>#<token>` links, from a web page, a message or an App Link. */
object GroupLinks {
    private val ID = Regex("grp_[a-z2-7]{26}")
    fun isGroupId(value: String) = ID.matches(value)

    /** The group id and the link token (the fragment), or null for anything else. */
    fun parse(url: String?): Pair<String, String?>? {
        if (url == null || url.length > 512) return null
        val uri = try { java.net.URI(url) } catch (_: Exception) { return null }
        if (!uri.scheme.equals("https", true) || !uri.host.equals("inbox.pigeonpost.dev", true) || (uri.port != -1 && uri.port != 443) || uri.rawUserInfo != null) return null
        val id = uri.rawPath?.removePrefix("/g/")?.trimEnd('/') ?: return null
        if (uri.rawPath?.startsWith("/g/") != true || !isGroupId(id)) return null
        val token = uri.rawFragment?.takeIf { it.isNotEmpty() && it.length <= 200 && it.all { c -> c.isLetterOrDigit() || c in "-_" } }
        return id to token
    }
}
