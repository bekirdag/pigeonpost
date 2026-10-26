package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.validAddress
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import java.net.URI
import java.time.Instant
import java.util.Locale

/** A short-lived terminal proof, kept in view memory and omitted from diagnostic descriptions. */
class DevicePairingCode private constructor(val rawValue: String) {
    val displayValue get() = rawValue.take(4) + "-" + rawValue.takeLast(4)
    override fun equals(other: Any?) = other is DevicePairingCode && rawValue == other.rawValue
    override fun hashCode() = rawValue.hashCode()
    override fun toString() = "DevicePairingCode(redacted)"
    companion object {
        fun parse(input: String): DevicePairingCode? {
            if (input.length > 128 || input.any { it.code > 127 }) return null
            val value = input.trim { it in " \t\r\n" }.uppercase(Locale.ROOT)
            if (!Regex("[A-Z0-9]{4}-?[A-Z0-9]{4}").matches(value)) return null
            return DevicePairingCode(value.replace("-", ""))
        }
    }
}

object DevicePairingLink {
    fun parse(input: String): DevicePairingCode? = runCatching {
        if (input.length > 4096 || input.any { it.code < 32 || it.code > 126 }) return null
        val uri = URI(input)
        if (!uri.scheme.equals("https", true) || !uri.host.equals("inbox.pigeonpost.dev", true) ||
            uri.rawUserInfo != null || uri.port != -1 || uri.rawPath != "/pair.html" || uri.rawQuery != null ||
            !uri.rawAuthority.equals("inbox.pigeonpost.dev", true)) return null
        val fragment = uri.rawFragment ?: return null
        if (fragment != fragment.trim()) return null
        DevicePairingCode.parse(fragment)
    }.getOrNull()
}

internal fun validPairingId(value: String) = Regex("[A-Za-z0-9_-]{1,128}").matches(value)
internal fun canonicalPairingAddress(value: String) = Regex("/k/[A-Za-z0-9_]{1,157}").matches(value)
private fun safeText(value: String, limit: Int) = value.isNotEmpty() && value.toByteArray(Charsets.UTF_8).size <= limit &&
    value.none { Character.isISOControl(it) || it in '\u202a'..'\u202e' || it in '\u2066'..'\u2069' || it in "\u200e\u200f\u061c" }

@Serializable
data class PairingOrganization(val id: String, val name: String)

@Serializable
data class DevicePairingPreview(
    @SerialName("pairing_id") val pairingId: String,
    val org: PairingOrganization,
    val handle: String,
    val address: String? = null,
    @SerialName("creates_agent") val createsAgent: Boolean? = null,
    val label: String? = null,
    val confirmation: String,
    val capabilities: List<String>,
    @SerialName("ip_country") val ipCountry: String? = null,
    @SerialName("requested_at") val requestedAt: Long,
    @SerialName("expires_at") val expiresAt: Long,
    @SerialName("can_approve") val canApprove: Boolean = false,
    val reason: String? = null,
    val state: String? = null,
) {
    val confirmationDigits get() = confirmation.replace(" ", "")
    val displayConfirmation get() = confirmationDigits.take(3) + " " + confirmationDigits.takeLast(3)
    val isValid: Boolean get() = validPairingId(pairingId) && validPairingId(org.id) && safeText(org.name, 320) &&
        handle.length <= 160 && handle.count { it == '/' } == 2 && !handle.startsWith("/k/") && validAddress(handle) &&
        (address?.let { canonicalPairingAddress(it) && createsAgent != true } ?: (createsAgent == true)) &&
        (label == null || safeText(label, 320)) &&
        (confirmation.length == 6 || confirmation.length == 7 && confirmation[3] == ' ') &&
        confirmationDigits.length == 6 && confirmationDigits.all { it in '0'..'9' } &&
        capabilities.isNotEmpty() && capabilities.size <= 32 && capabilities.distinct().size == capabilities.size && capabilities.all(::validPairingId) &&
        (ipCountry == null || safeText(ipCountry, 80)) && (reason == null || safeText(reason, 160)) &&
        requestedAt >= 0 && expiresAt > requestedAt && expiresAt <= 253_402_300_799L &&
        (state == null || state in setOf("pending", "approved", "denied"))
    fun allowsDecision(now: Long = Instant.now().epochSecond) = isValid && (state == null || state == "pending") &&
        expiresAt > now && requestedAt <= now + 60
    fun allowsApproval(now: Long = Instant.now().epochSecond) = allowsDecision(now) && canApprove && capabilities == listOf("mailbox")
    override fun toString() = "DevicePairingPreview(valid=$isValid)"
}
