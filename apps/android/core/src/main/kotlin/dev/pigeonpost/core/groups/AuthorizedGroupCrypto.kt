package dev.pigeonpost.core.groups

import dev.pigeonpost.core.AuthorizationLease

/** Revocation is checked while the synchronous library operation actually runs. */
internal class AuthorizedGroupCrypto(private val base: GroupCryptoLibrary, private val lease: AuthorizationLease) : GroupCryptoLibrary {
    override fun newDevice(mailbox: String, deviceId: String): GroupDeviceHandle = lease.withActive { AuthorizedGroupDevice(base.newDevice(mailbox, deviceId), lease) }
    override fun importState(blob: ByteArray, key: ByteArray): GroupDeviceHandle = lease.withActive { AuthorizedGroupDevice(base.importState(blob, key), lease) }
    override fun generateStateKey() = lease.withActive { base.generateStateKey() }
    override fun sealAttachment(plaintext: ByteArray) = lease.withActive { base.sealAttachment(plaintext) }
    override fun openAttachment(sealed: ByteArray, reference: FileKey) = lease.withActive { base.openAttachment(sealed, reference) }
    override fun fingerprintDigits(fingerprint: ByteArray) = lease.withActive { base.fingerprintDigits(fingerprint) }
    override val encScheme: String get() = lease.withActive { base.encScheme }
}

private class AuthorizedGroupDevice(private val base: GroupDeviceHandle, private val lease: AuthorizationLease) : GroupDeviceHandle {
    override fun signaturePublicKey() = lease.withActive { base.signaturePublicKey() }
    override fun fingerprint() = lease.withActive { base.fingerprint() }
    override fun installAttestation(mailboxPublicKey: ByteArray, signature: ByteArray): Unit = lease.withActive { base.installAttestation(mailboxPublicKey, signature) }
    override fun groups() = lease.withActive { base.groups() }
    override fun generateKeyPackages(count: Int) = lease.withActive { base.generateKeyPackages(count) }
    override fun generateLastResortKeyPackage() = lease.withActive { base.generateLastResortKeyPackage() }
    override fun createGroup(groupId: String) = lease.withActive { base.createGroup(groupId) }
    override fun joinFromWelcome(welcome: ByteArray) = lease.withActive { base.joinFromWelcome(welcome) }
    override fun forgetGroup(groupId: String): Unit = lease.withActive { base.forgetGroup(groupId) }
    override fun epoch(groupId: String) = lease.withActive { base.epoch(groupId) }
    override fun hasPendingCommit(groupId: String) = lease.withActive { base.hasPendingCommit(groupId) }
    override fun members(groupId: String) = lease.withActive { base.members(groupId) }
    override fun safetyNumber(groupId: String) = lease.withActive { base.safetyNumber(groupId) }
    override fun commit(groupId: String, keyPackages: List<ByteArray>, remove: List<DeviceKey>) = lease.withActive { base.commit(groupId, keyPackages, remove) }
    override fun selfUpdate(groupId: String) = lease.withActive { base.selfUpdate(groupId) }
    override fun confirmCommit(groupId: String) = lease.withActive { base.confirmCommit(groupId) }
    override fun discardCommit(groupId: String): Unit = lease.withActive { base.discardCommit(groupId) }
    override fun encryptMessage(groupId: String, clientMessageId: String, body: ByteArray, attachments: List<FileKey>) = lease.withActive { base.encryptMessage(groupId, clientMessageId, body, attachments) }
    override fun processIncoming(groupId: String, message: ByteArray) = lease.withActive { base.processIncoming(groupId, message) }
    override fun exportState(key: ByteArray) = lease.withActive { base.exportState(key) }
}
