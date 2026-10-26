package dev.pigeonpost.inbox.calendar

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.WireJson
import dev.pigeonpost.core.calendar.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import java.io.File
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Durable exact retry intent, not a calendar cache. Never discard an unreadable unknown write. */
class KeystoreCalendarAttempts(context: Context) : CalendarAttemptStorage {
    private val directory = File(context.noBackupFilesDir, "calendar-attempts")
    private val lock = Mutex()
    companion object {
        private const val ALIAS = "pigeonpost.calendar.attempts.v1"
        private val keyLock = Any()
    }
    private fun key(): SecretKey = synchronized(keyLock) {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey) ?: KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256).setRandomizedEncryptionRequired(true).build())
        }.generateKey()
    }
    private fun file(mailbox: String): AtomicFile {
        val digest = MessageDigest.getInstance("SHA-256").digest(mailbox.toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it) }
        return AtomicFile(File(directory, "$digest.enc"))
    }
    private fun aad(mailbox: String) = "pigeonpost.calendar.attempts.v1\n$mailbox".toByteArray(Charsets.UTF_8)
    private fun present(file: AtomicFile) = file.baseFile.exists() || File(file.baseFile.path + ".bak").exists() || File(file.baseFile.path + ".new").exists()
    private fun read(file: AtomicFile): ByteArray = file.openRead().use { input ->
        val out = ByteArrayOutputStream(); val buffer = ByteArray(8192)
        while (true) {
            val count = input.read(buffer); if (count < 0) break
            require(out.size() + count <= CalendarAttempts.MAX_FILE_BYTES)
            out.write(buffer, 0, count)
        }
        out.toByteArray()
    }
    private suspend fun <T> local(authorization: AuthorizationLease, work: () -> T): T = withContext(Dispatchers.IO) {
        authorization.requireActive()
        lock.withLock {
            authorization.withActive {
                try { work() }
                catch (cancelled: CancellationException) { throw cancelled }
                // Parser diagnostics may contain plaintext. Do not attach them to a public failure.
                catch (_: Exception) { throw CalendarAttemptStorageException() }
            }
        }
    }
    override suspend fun load(mailbox: String, authorization: AuthorizationLease): List<CalendarPendingAttempt> = local(authorization) {
        CalendarAttempts.validate(mailbox, emptyList())
        val source = file(mailbox)
        if (!present(source)) return@local emptyList()
        val sealed = read(source)
        require(sealed.size >= 29 && sealed[0] == 1.toByte())
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, sealed.copyOfRange(1, 13)))
        cipher.updateAAD(aad(mailbox))
        val envelope = WireJson.decodeFromString<CalendarAttemptEnvelope>(cipher.doFinal(sealed.copyOfRange(13, sealed.size)).toString(Charsets.UTF_8))
        require(envelope.version == 1)
        CalendarAttempts.validate(mailbox, envelope.attempts)
        envelope.attempts
    }
    override suspend fun save(mailbox: String, attempts: List<CalendarPendingAttempt>, authorization: AuthorizationLease) {
        val frozen = attempts.toList()
        local(authorization) {
            CalendarAttempts.validate(mailbox, frozen)
            val destination = file(mailbox)
            if (frozen.isEmpty()) {
                destination.delete()
                check(!present(destination))
            } else {
                check(directory.isDirectory || directory.mkdirs())
                val plain = WireJson.encodeToString(CalendarAttemptEnvelope.serializer(), CalendarAttemptEnvelope(attempts = frozen)).toByteArray(Charsets.UTF_8)
                val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                cipher.init(Cipher.ENCRYPT_MODE, key()); require(cipher.iv.size == 12)
                cipher.updateAAD(aad(mailbox))
                val sealed = byteArrayOf(1) + cipher.iv + cipher.doFinal(plain)
                require(sealed.size <= CalendarAttempts.MAX_FILE_BYTES)
                val stream = destination.startWrite()
                try { stream.write(sealed); destination.finishWrite(stream) }
                catch (failure: Throwable) { destination.failWrite(stream); throw failure }
                // AtomicFile may log a failed rename without throwing. Never send before verification.
                check(destination.baseFile.isFile && read(destination).contentEquals(sealed))
            }
        }
    }
}
