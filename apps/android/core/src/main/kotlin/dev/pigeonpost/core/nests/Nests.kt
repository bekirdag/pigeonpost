package dev.pigeonpost.core.nests

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import java.io.OutputStream

/** Nests Phase B (docs/nests/phase-b.md): wire models of the postbox's `nests` routes. */

@Serializable
data class NestPackage(
    val code: String,
    val name: String,
    val rank: Int = 0,
    val slots: Int = 0,
    @SerialName("storage_bytes") val storageBytes: Long = 0,
    @SerialName("active_hours") val activeHours: Int = 0,
    @SerialName("input_tokens") val inputTokens: Long = 0,
    @SerialName("output_tokens") val outputTokens: Long = 0,
    val model: String = "managed",
    val sellable: Boolean = false,
    @SerialName("google_product_id") val googleProductId: String = "",
    @SerialName("google_base_plan_id") val googleBasePlanId: String = "monthly",
)

@Serializable
data class NestRails(val apple: Boolean = false, val google: Boolean = false, val web: Boolean = false)

@Serializable
data class NestCatalog(
    @SerialName("catalog_version") val catalogVersion: String = "",
    val packages: List<NestPackage> = emptyList(),
    val rails: NestRails = NestRails(),
    @SerialName("google_account_id") val googleAccountId: String = "",
)

@Serializable
data class NestFunding(
    @SerialName("entitlement_id") val entitlementId: String,
    val source: String,
    val `package`: String,
    val status: String = "active",
    @SerialName("paid_through") val paidThrough: Long? = null,
)

@Serializable
data class NestAllowance(
    val funding: NestFunding? = null,
    val slots: Int = 0,
    val used: Int = 0,
    val duplicates: List<NestFunding> = emptyList(),
)

@Serializable
data class NestView(
    @SerialName("nest_id") val nestId: String,
    val address: String? = null,
    val label: String = "",
    val state: String = "",
    val funding: String? = null,
    val status: String = "unavailable",
    @SerialName("status_reason") val statusReason: String? = null,
    @SerialName("paused_reason") val pausedReason: String? = null,
    @SerialName("delete_after") val deleteAfter: Long? = null,
    val role: String? = null,
    @SerialName("org_id") val orgId: String? = null,
)

@Serializable
data class NestSetup(
    @SerialName("setup_id") val setupId: String,
    val label: String = "",
    val status: String = "pending",
    val attempts: Int = 0,
    @SerialName("last_error") val lastError: String? = null,
)

@Serializable
data class NestSnapshot(
    val allowance: NestAllowance = NestAllowance(),
    val `package`: NestPackage? = null,
    val nests: List<NestView> = emptyList(),
    val setups: List<NestSetup> = emptyList(),
)

@Serializable
data class NestIntent(
    @SerialName("intent_id") val intentId: String,
    @SerialName("google_product_id") val googleProductId: String,
    @SerialName("google_account_id") val googleAccountId: String,
)

@Serializable
data class NestCoverage(@SerialName("units_total") val total: Int? = null, @SerialName("units_text") val text: Int? = null)

@Serializable
data class NestFile(
    @SerialName("file_id") val fileId: String,
    val name: String,
    val bytes: Long = 0,
    val kind: String? = null,
    val status: String = "uploading",
    val reason: String? = null,
    val coverage: NestCoverage = NestCoverage(),
    val warnings: List<String> = emptyList(),
)

@Serializable
data class NestFiles(
    val files: List<NestFile> = emptyList(),
    @SerialName("used_bytes") val usedBytes: Long = 0,
    @SerialName("quota_bytes") val quotaBytes: Long = 0,
)

@Serializable
data class NestApproval(
    @SerialName("approval_id") val approvalId: String,
    val tool: String,
    val call: JsonObject? = null,
    @SerialName("args_hash") val argsHash: String,
    val status: String,
    @SerialName("expires_at") val expiresAt: Long = 0,
    val result: String? = null,
) {
    private val args get() = call?.get("args") as? JsonObject
    /** `send_message` recipient and body, exactly as the postbox will send them. */
    val to get() = (args?.get("to") as? JsonPrimitive)?.contentOrNull
    val body get() = (args?.get("body") as? JsonPrimitive)?.contentOrNull
    /** Only a call this app can show in full may be approved; anything else can only be rejected. */
    val reviewable get() = tool == "send_message" && !to.isNullOrEmpty() && !body.isNullOrEmpty()
    /** The start of the hash the approval binds, for comparing what two screens show. */
    val checkCode get() = argsHash.take(8)
}

@Serializable
data class NestApprovals(val approvals: List<NestApproval> = emptyList())

@Serializable
data class NestTokenUse(val used: Long? = null, val limit: Long? = null)

@Serializable
data class NestUsage(
    val period: String? = null,
    @SerialName("input_tokens") val inputTokens: NestTokenUse = NestTokenUse(),
    @SerialName("output_tokens") val outputTokens: NestTokenUse = NestTokenUse(),
    val requests: Long? = null,
    @SerialName("running_secs") val runningSecs: Long? = null,
    @SerialName("files_bytes") val filesBytes: Long = 0,
    val `package`: NestUsagePackage? = null,
)

@Serializable
data class NestUsagePackage(
    val code: String = "",
    @SerialName("active_hours_pooled") val activeHoursPooled: Int? = null,
    @SerialName("storage_bytes_per_nest") val storageBytesPerNest: Long? = null,
)

@Serializable
data class OrgSummary(val id: String, val name: String = "", val roles: List<String> = emptyList()) {
    /** Roles the postbox lets add and manage this organization's Nests (`nest_access.rs`). */
    val managesNests get() = roles.any { it == "owner" || it == "admin" || it == "agent_admin" }
}

@Serializable
data class OrgNest(
    @SerialName("nest_id") val nestId: String,
    val address: String? = null,
    val label: String = "",
    val state: String = "",
    val role: String = "operator",
)

@Serializable
data class OrgNests(val nests: List<OrgNest> = emptyList(), val allowance: NestAllowance = NestAllowance())

@Serializable
data class NestCreated(@SerialName("nest_id") val nestId: String, val address: String? = null, val state: String = "")

interface NestApi {
    suspend fun nestPackages(): NestCatalog
    suspend fun nests(): NestSnapshot
    suspend fun nest(id: String): NestView
    suspend fun nestIntent(packageCode: String, rail: String): NestIntent
    suspend fun claimNestGoogle(purchaseToken: String): NestSnapshot
    suspend fun retryNestSetup(setupId: String): NestSnapshot
    suspend fun createNest(label: String): NestCreated
    suspend fun createOrgNest(orgId: String, handle: String, label: String): NestCreated
    suspend fun nestFiles(id: String): NestFiles
    suspend fun uploadNestFile(id: String, name: String, bytes: ByteArray): NestFile
    suspend fun deleteNestFile(id: String, fileId: String)
    suspend fun nestApprovals(id: String): NestApprovals
    suspend fun decideNestApproval(id: String, approvalId: String, approve: Boolean, argsHash: String)
    suspend fun nestUsage(id: String): NestUsage
    suspend fun nestAction(id: String, action: String)
    suspend fun renameNest(id: String, label: String)
    suspend fun deleteNest(id: String, confirm: String)
    /** Streams the export zip into `out`, so a large Nest is never held in memory. */
    suspend fun exportNest(id: String, out: OutputStream)
    suspend fun nestOrganizations(): List<OrgSummary>
    suspend fun orgNests(orgId: String): OrgNests
}

object NestRules {
    const val FEATURE = "nests"
    const val MAX_UPLOAD_BYTES = 25L * 1024 * 1024
    private val NEST_ID = Regex("nst_[0-9a-f]{1,60}")

    fun validNestId(id: String) = NEST_ID.matches(id)

    fun createdNotice(label: String, state: String) =
        if (state == "running" || state == "ready") "$label is ready." else "$label is being set up."

    /** A decided card, for the short history under the open ones. */
    fun approvalOutcome(status: String, result: String?): String = when (status) {
        "approved" -> "Approved"
        "rejected" -> "Rejected"
        "superseded" -> "Replaced by a newer request"
        "expired" -> "Expired"
        "failed" -> if (result == "recipient_not_allowed") "Not sent · this recipient is not allowed" else "Not sent"
        else -> status
    }

    /** The display name sent in `x-file-name`: UTF-8, every byte outside the unreserved set
     * percent-encoded, control characters dropped, at most 200 characters. Never a path. */
    fun encodeFileName(name: String): String {
        val clean = name.filterNot { it.isISOControl() }.trim().take(200).ifEmpty { "file" }
        val out = StringBuilder()
        for (b in clean.toByteArray(Charsets.UTF_8)) {
            val c = b.toInt() and 0xff
            if ((c in 'A'.code..'Z'.code) || (c in 'a'.code..'z'.code) || (c in '0'.code..'9'.code) || c == '-'.code || c == '_'.code || c == '.'.code || c == '~'.code) out.append(c.toChar())
            else out.append('%').append("0123456789ABCDEF"[c shr 4]).append("0123456789ABCDEF"[c and 15])
        }
        return out.toString()
    }

    /** What the person sees for a server refusal. */
    fun explain(code: String?, fallback: String?): String = when (code) {
        "nest_package_active" -> "A Nest package bought on another platform already pays for your Nests. Change it where you bought it."
        "nest_package_owned" -> "You already have this package. Use Restore purchases."
        "feature_disabled" -> "Nests are not available for this account yet."
        "billing_unavailable" -> "Google Play purchases are not available right now. Try again later."
        "purchase_already_used" -> "This Google Play subscription belongs to another Pigeonpost account."
        "purchase_account_mismatch" -> "This purchase was made for a different Pigeonpost account. Sign in to that account to restore it."
        "purchase_inactive" -> "Google Play has not confirmed an active payment yet."
        "nest_slots_full" -> "Every Nest in your package is in use. Upgrade or delete a Nest first."
        "no_nest_package" -> "Buy a Nest package first."
        "needs_payment" -> "This Nest is paused until its package is paid for."
        "over_capacity" -> "Your package has fewer Nests than you have. Delete one or upgrade."
        "approval_changed" -> "This action changed since you saw it. Review it again."
        "approval_closed" -> "This approval was already decided, replaced or expired."
        "recipient_not_allowed" -> "A Nest may only message you or people you have allowed."
        "storage_full" -> "This Nest's storage is full."
        "too_large" -> "Files can be at most 25 MB."
        "rate_limited" -> "Too many attempts. Try again later."
        "too_many_files" -> "This Nest holds its maximum number of files. Delete some first."
        "upload_failed" -> "The file was not stored. Try again."
        "bad_name" -> "This file has no usable name."
        "bad_label", "invalid_request" -> "Use a name of 1 to 64 characters."
        "invalid_handle" -> "Use letters, numbers and hyphens for the handle."
        "handle_taken" -> "That handle is taken."
        "agent_limit" -> "The organization has used all its agent addresses."
        "no_nest_slot" -> "The organization has no free Nest slot."
        "nest_limit" -> "This account has reached its Nest limit."
        "org_read_only" -> "This organization is read-only until its payment is settled."
        "org_inactive" -> "This organization is not active."
        "no_nest" -> "This Nest is gone, or you no longer have access to it."
        "no_file" -> "This file is already gone."
        "no_approval" -> "This approval is gone."
        "tool_failed" -> "The action could not be carried out."
        "nests_unavailable", "nestd_failed" -> "The Nests service is not answering. Try again in a moment."
        "export_too_large" -> "This Nest is too large for one export. Contact support."
        "confirm_required" -> "Type the Nest's name exactly to delete it."
        else -> fallback ?: "Something went wrong. Try again."
    }
}
