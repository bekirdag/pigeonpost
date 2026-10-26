package dev.pigeonpost.inbox.groups

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import dev.pigeonpost.core.WireJson
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.groups.GroupRecord
import dev.pigeonpost.core.groups.GroupStorage
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.File
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Group state at rest: one file per mailbox in app-private, no-backup storage, sealed with an
 * AES-256-GCM key that never leaves Android Keystore. The MLS state blob inside is already
 * encrypted by the library under its own 32-byte state key; this outer layer also covers that key
 * and the decrypted history, so copying the files off the device yields nothing readable.
 * The mailbox address is the AAD, so one mailbox's file cannot be swapped in for another's.
 * Clearing the app's data (or a Keystore reset) destroys the device for good: it is re-enrolled
 * and re-added to its groups without history.
 */
class KeystoreGroupStorage(context: Context) : GroupStorage {
    private val dir = File(context.noBackupFilesDir, "groups").apply { mkdirs() }
    private val alias = "pigeonpost.groups.v1"
    private val lock = Mutex()

    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(alias, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256).setRandomizedEncryptionRequired(true).build())
        }.generateKey()
    }

    private fun file(mailbox: String): AtomicFile {
        val name = MessageDigest.getInstance("SHA-256").digest(mailbox.toByteArray()).joinToString("") { "%02x".format(it) }.take(40)
        return AtomicFile(File(dir, "$name.enc"))
    }

    private suspend fun <T> local(authorization: AuthorizationLease?, work: () -> T): T = withContext(Dispatchers.IO) {
        authorization?.requireActive()
        lock.withLock {
            if (authorization == null) work() else authorization.withActive(work)
        }
    }

    override suspend fun load(mailbox: String) = read(mailbox, null)
    override suspend fun load(mailbox: String, authorization: AuthorizationLease) = read(mailbox, authorization)
    private suspend fun read(mailbox: String, authorization: AuthorizationLease?): GroupRecord? = local(authorization) {
            val file = file(mailbox)
            if (!file.baseFile.exists()) return@local null
            try {
                val bytes = file.openRead().use { it.readBytes() }
                require(bytes.size > 29 && bytes[0] == 1.toByte())
                val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes.copyOfRange(1, 13)))
                cipher.updateAAD(mailbox.toByteArray())
                WireJson.decodeFromString<GroupRecord>(cipher.doFinal(bytes.copyOfRange(13, bytes.size)).toString(Charsets.UTF_8))
            } catch (_: Exception) {
                null // a key that no longer opens it: this device is gone, and a new one is enrolled
            }
    }

    override suspend fun save(mailbox: String, record: GroupRecord) = write(mailbox, record, null)
    override suspend fun save(mailbox: String, record: GroupRecord, authorization: AuthorizationLease) = write(mailbox, record, authorization)
    private suspend fun write(mailbox: String, record: GroupRecord, authorization: AuthorizationLease?) = local(authorization) {
            val plain = WireJson.encodeToString(GroupRecord.serializer(), record).toByteArray(Charsets.UTF_8)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, key())
            require(cipher.iv.size == 12)
            cipher.updateAAD(mailbox.toByteArray())
            val sealed = byteArrayOf(1) + cipher.iv + cipher.doFinal(plain)
            val file = file(mailbox)
            val stream = file.startWrite()
            try { stream.write(sealed); file.finishWrite(stream) }
            catch (failure: Throwable) { file.failWrite(stream); throw failure }
    }

    override suspend fun remove(mailbox: String) = local(null) { file(mailbox).delete() }
    override suspend fun remove(mailbox: String, authorization: AuthorizationLease) = local(authorization) { file(mailbox).delete() }
}
