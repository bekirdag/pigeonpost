package dev.pigeonpost.core.groups

/**
 * The group E2EE library as the client loop needs it (crates/pigeonpost-groupcrypto, docs/group-e2ee.md §4).
 * The app adapts the UniFFI Kotlin binding (dev.pigeonpost.groupcrypto) to this; tests use a fake.
 * Nothing here ever sees a key the server holds: the device's signature key and every group secret
 * stay inside the library and its encrypted state blob.
 */
interface GroupCryptoLibrary {
    fun newDevice(mailbox: String, deviceId: String): GroupDeviceHandle
    fun importState(blob: ByteArray, key: ByteArray): GroupDeviceHandle
    fun generateStateKey(): ByteArray
    fun sealAttachment(plaintext: ByteArray): SealedFile
    fun openAttachment(sealed: ByteArray, reference: FileKey): ByteArray
    fun fingerprintDigits(fingerprint: ByteArray): String
    val encScheme: String
}

interface GroupDeviceHandle {
    fun signaturePublicKey(): ByteArray
    fun fingerprint(): ByteArray
    fun installAttestation(mailboxPublicKey: ByteArray, signature: ByteArray)
    fun groups(): List<String>
    fun generateKeyPackages(count: Int): List<ByteArray>
    fun generateLastResortKeyPackage(): ByteArray
    fun createGroup(groupId: String): List<GroupDeviceInfo>
    fun joinFromWelcome(welcome: ByteArray): String
    fun forgetGroup(groupId: String)
    fun epoch(groupId: String): Long
    fun hasPendingCommit(groupId: String): Boolean
    fun members(groupId: String): List<GroupDeviceInfo>
    fun safetyNumber(groupId: String): String
    fun commit(groupId: String, keyPackages: List<ByteArray>, remove: List<DeviceKey>): CommitPlan
    fun selfUpdate(groupId: String): CommitPlan
    fun confirmCommit(groupId: String): CommitResult
    fun discardCommit(groupId: String)
    fun encryptMessage(groupId: String, clientMessageId: String, body: ByteArray, attachments: List<FileKey>): Ciphertext
    fun processIncoming(groupId: String, message: ByteArray): Processed
    fun exportState(key: ByteArray): ByteArray
}

/** A stable library error code (`stale_epoch`, `future_epoch`, `decryption_failed`, …). */
class GroupCryptoError(val code: String, detail: String? = null) : Exception(detail ?: code)

data class DeviceKey(val mailbox: String, val deviceId: String)
data class GroupDeviceInfo(val mailbox: String, val deviceId: String, val fingerprint: ByteArray) {
    override fun equals(other: Any?) = other is GroupDeviceInfo && other.mailbox == mailbox && other.deviceId == deviceId && other.fingerprint.contentEquals(fingerprint)
    override fun hashCode() = 31 * (31 * mailbox.hashCode() + deviceId.hashCode()) + fingerprint.contentHashCode()
}
/** An attachment's key, the SHA-256 of its sealed blob and its plaintext length. Secret. */
class FileKey(val key: ByteArray, val sealedSha256: ByteArray, val plaintextLen: Long)
class SealedFile(val sealed: ByteArray, val reference: FileKey)
class CommitPlan(val epoch: Long, val commitId: String, val commit: ByteArray, val welcome: ByteArray?, val envelopeSignature: ByteArray)
class CommitResult(val epoch: Long, val committer: GroupDeviceInfo, val added: List<GroupDeviceInfo>, val removed: List<GroupDeviceInfo>,
    val updated: List<GroupDeviceInfo>, val selfRemoved: Boolean)
class Ciphertext(val epoch: Long, val ciphertext: ByteArray, val envelopeSignature: ByteArray)

sealed class Processed {
    class Message(val sender: GroupDeviceInfo, val body: ByteArray, val attachments: List<FileKey>) : Processed()
    class Commit(val result: CommitResult) : Processed()
    object Other : Processed()
}
