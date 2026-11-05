package dev.pigeonpost.core.nests

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject

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
)

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
)

@Serializable
data class OrgSummary(val id: String, val name: String = "")

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

interface NestApi {
    suspend fun nestPackages(): NestCatalog
    suspend fun nests(): NestSnapshot
    suspend fun nest(id: String): NestView
    suspend fun nestIntent(packageCode: String, rail: String): NestIntent
    suspend fun claimNestGoogle(purchaseToken: String): NestSnapshot
    suspend fun retryNestSetup(setupId: String): NestSnapshot
    suspend fun nestFiles(id: String): NestFiles
    suspend fun uploadNestFile(id: String, name: String, bytes: ByteArray): NestFile
    suspend fun deleteNestFile(id: String, fileId: String)
    suspend fun nestApprovals(id: String): NestApprovals
    suspend fun decideNestApproval(id: String, approvalId: String, approve: Boolean, argsHash: String)
    suspend fun nestUsage(id: String): NestUsage
    suspend fun nestAction(id: String, action: String)
    suspend fun renameNest(id: String, label: String)
    suspend fun deleteNest(id: String, confirm: String)
    suspend fun exportNest(id: String): ByteArray
    suspend fun nestOrganizations(): List<OrgSummary>
    suspend fun orgNests(orgId: String): OrgNests
}

object NestRules {
    const val FEATURE = "nests"
    const val MAX_UPLOAD_BYTES = 25L * 1024 * 1024
    private val NEST_ID = Regex("nst_[0-9a-f]{1,60}")

    fun validNestId(id: String) = NEST_ID.matches(id)

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
        "confirm_required" -> "Type the Nest's name exactly to delete it."
        else -> fallback ?: "Something went wrong. Try again."
    }
}
