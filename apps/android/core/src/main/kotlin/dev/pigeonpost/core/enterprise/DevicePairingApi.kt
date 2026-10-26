package dev.pigeonpost.core.enterprise

import kotlinx.serialization.Serializable

interface DevicePairingApi {
    suspend fun resolveDevicePairing(code: DevicePairingCode): DevicePairingPreview
    suspend fun approveDevicePairing(preview: DevicePairingPreview, code: DevicePairingCode)
    /** An acknowledgment does not prove final denial; resolve the code again afterwards. */
    suspend fun denyDevicePairing(preview: DevicePairingPreview, code: DevicePairingCode)
}

@Serializable internal data class PairingApproval(val approved: Boolean = false, val address: String? = null, val handle: String? = null)
@Serializable internal data class PairingDenial(val denied: Boolean = false)
