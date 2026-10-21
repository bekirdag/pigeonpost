package dev.pigeonpost.core.groups

import dev.pigeonpost.groupcrypto.AttachmentRef
import dev.pigeonpost.groupcrypto.CommitBundle
import dev.pigeonpost.groupcrypto.CommitOutcome
import dev.pigeonpost.groupcrypto.DeviceRef
import dev.pigeonpost.groupcrypto.GroupCryptoException
import dev.pigeonpost.groupcrypto.GroupDevice
import dev.pigeonpost.groupcrypto.Incoming
import dev.pigeonpost.groupcrypto.MemberInfo

/**
 * The UniFFI Kotlin binding of crates/pigeonpost-groupcrypto behind [GroupCryptoLibrary].
 *
 * This one file is compiled twice: into the Android app against the AAR (arm64-v8a, armeabi-v7a,
 * x86_64), and into core's JVM tests against the generated binding source and a host build of the
 * same library, so the client loop is tested with real MLS on both.
 */
class UniffiGroupCrypto : GroupCryptoLibrary {
    override fun newDevice(mailbox: String, deviceId: String): GroupDeviceHandle = wrap { Handle(GroupDevice(mailbox, deviceId)) }
    override fun importState(blob: ByteArray, key: ByteArray): GroupDeviceHandle = wrap { Handle(GroupDevice.importState(blob, key)) }
    override fun generateStateKey(): ByteArray = wrap { dev.pigeonpost.groupcrypto.generateStateKey() }
    override fun sealAttachment(plaintext: ByteArray): SealedFile = wrap {
        val s = dev.pigeonpost.groupcrypto.sealAttachment(plaintext)
        SealedFile(s.sealed, FileKey(s.reference.key, s.reference.sealedSha256, s.reference.plaintextLen.toLong()))
    }
    override fun openAttachment(sealed: ByteArray, reference: FileKey): ByteArray = wrap {
        dev.pigeonpost.groupcrypto.openAttachment(sealed, AttachmentRef(reference.key, reference.sealedSha256, reference.plaintextLen.toULong()))
    }
    override fun fingerprintDigits(fingerprint: ByteArray): String = wrap { dev.pigeonpost.groupcrypto.fingerprintDigits(fingerprint) }
    override val encScheme: String get() = dev.pigeonpost.groupcrypto.encScheme()

    private class Handle(private val d: GroupDevice) : GroupDeviceHandle {
        override fun signaturePublicKey() = wrap { d.signaturePublicKey() }
        override fun fingerprint() = wrap { d.fingerprint() }
        override fun installAttestation(mailboxPublicKey: ByteArray, signature: ByteArray) = wrap { d.installAttestation(mailboxPublicKey, signature) }
        override fun groups() = wrap { d.groups() }
        override fun generateKeyPackages(count: Int) = wrap { d.generateKeyPackages(count.toUInt()) }
        override fun generateLastResortKeyPackage() = wrap { d.generateLastResortKeyPackage() }
        override fun createGroup(groupId: String) = wrap { d.createGroup(groupId).members.map(::info) }
        override fun joinFromWelcome(welcome: ByteArray) = wrap { d.joinFromWelcome(welcome).groupId }
        override fun forgetGroup(groupId: String) = wrap { d.forgetGroup(groupId) }
        override fun epoch(groupId: String) = wrap { d.epoch(groupId).toLong() }
        override fun hasPendingCommit(groupId: String) = wrap { d.hasPendingCommit(groupId) }
        override fun members(groupId: String) = wrap { d.members(groupId).map(::info) }
        override fun safetyNumber(groupId: String) = wrap { d.safetyNumber(groupId) }
        override fun commit(groupId: String, keyPackages: List<ByteArray>, remove: List<DeviceKey>) =
            wrap { plan(d.commit(groupId, keyPackages, remove.map { DeviceRef(it.mailbox, it.deviceId) })) }
        override fun selfUpdate(groupId: String) = wrap { plan(d.selfUpdate(groupId)) }
        override fun confirmCommit(groupId: String) = wrap { result(d.confirmCommit(groupId)) }
        override fun discardCommit(groupId: String) = wrap { d.discardCommit(groupId) }
        override fun encryptMessage(groupId: String, clientMessageId: String, body: ByteArray, attachments: List<FileKey>) = wrap {
            val out = d.encryptMessage(groupId, clientMessageId, body, attachments.map { AttachmentRef(it.key, it.sealedSha256, it.plaintextLen.toULong()) })
            Ciphertext(out.epoch.toLong(), out.ciphertext, out.envelopeSignature)
        }
        override fun processIncoming(groupId: String, message: ByteArray): Processed = wrap {
            when (val r = d.processIncoming(groupId, message)) {
                is Incoming.Message -> Processed.Message(info(r.message.sender), r.message.body,
                    r.message.attachments.map { FileKey(it.key, it.sealedSha256, it.plaintextLen.toLong()) })
                is Incoming.Commit -> Processed.Commit(result(r.outcome))
                is Incoming.OwnCommitConfirmed -> Processed.Commit(result(r.outcome))
                else -> Processed.Other // history offers are for a later own-device transfer; own echoes need nothing
            }
        }
        override fun exportState(key: ByteArray) = wrap { d.exportState(key) }
    }

    companion object {
        private fun info(m: MemberInfo) = GroupDeviceInfo(m.mailbox, m.deviceId, m.fingerprint)
        private fun plan(b: CommitBundle) = CommitPlan(b.epoch.toLong(), b.commitId, b.commit, b.welcome, b.envelopeSignature)
        private fun result(o: CommitOutcome) = CommitResult(o.epoch.toLong(), info(o.committer), o.added.map(::info), o.removed.map(::info), o.updated.map(::info), o.selfRemoved)
        private inline fun <T> wrap(work: () -> T): T = try { work() } catch (e: GroupCryptoException.Failure) { throw GroupCryptoError(e.code, e.detail) }
    }
}
