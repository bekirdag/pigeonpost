package dev.pigeonpost.inbox.meetings

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import androidx.annotation.RequiresApi
import dev.pigeonpost.core.meetings.MeetingNotesCrypto
import dev.pigeonpost.core.meetings.NotesKey
import java.io.File
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.PKCS8EncodedKeySpec
import javax.crypto.Cipher
import javax.crypto.KeyAgreement
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * This device's meeting-notes key (docs/meeting-summaries.md §3), kept by the Android Keystore.
 *
 * - Android 12 and later: a P-256 key made inside the Keystore for key agreement only
 *   (`PURPOSE_AGREE_KEY`). Its private half never exists outside the Keystore.
 * - Android 8–11, whose Keystore cannot do ECDH: the P-256 key is made in the app, and its PKCS#8 bytes
 *   are kept only as AES-256-GCM ciphertext under a Keystore key, in no-backup storage, with the key id
 *   as additional data. They are decrypted for a single agreement and wiped.
 *
 * The key id and the public half are not secret. Signing out destroys the key; a new one (with a new
 * id, never reusing an old one) is made the next time notes are opened.
 */
class KeystoreNotesKeys(context: Context, name: String = "meeting-notes",
    /** Tests turn this off to exercise the Android 8–11 path on a newer device. */
    private val agreeInKeystore: Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
    private val prefs = context.getSharedPreferences(name, Context.MODE_PRIVATE)
    private val wrapped = AtomicFile(File(context.noBackupFilesDir, "$name-key.enc"))
    private val agreeAlias = "pigeonpost.$name.agree.v1"
    private val wrapAlias = "pigeonpost.$name.wrap.v1"
    private val lock = Any()
    @Volatile private var cached: NotesKey? = null

    /** The key, made on first use. Blocking: call it off the main thread. */
    fun current(): NotesKey = synchronized(lock) {
        cached?.let { return it }
        val id = prefs.getString(KEY_ID, null)?.takeIf(MeetingNotesCrypto::validKeyId)
        val public = prefs.getString(PUBLIC, null)
        val loaded = if (id != null && public != null) load(id, public) else null
        (loaded ?: make()).also { cached = it }
    }

    /** Forget the key on this device (sign-out). Notes already wrapped for it can no longer be opened here. */
    fun destroy() = synchronized(lock) {
        cached = null
        runCatching { keystore().deleteEntry(agreeAlias) }
        runCatching { keystore().deleteEntry(wrapAlias) }
        wrapped.delete()
        prefs.edit().clear().commit()
    }

    private fun keystore() = KeyStore.getInstance(STORE).apply { load(null) }

    private fun load(id: String, public: String): NotesKey? = try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && prefs.getString(KIND, null) == KIND_KEYSTORE) {
            val entry = keystore().getEntry(agreeAlias, null) as? KeyStore.PrivateKeyEntry
            entry?.takeIf { MeetingNotesCrypto.b64(MeetingNotesCrypto.rawPublic(it.certificate.publicKey as ECPublicKey)) == public }
                ?.let { KeystoreAgreementKey(id, public, it.privateKey) }
        } else if (prefs.getString(KIND, null) == KIND_WRAPPED && wrapped.baseFile.exists()) WrappedKey(id, public, wrapped, ::wrappingKey)
        else null
    } catch (_: Exception) { null }

    private fun make(): NotesKey {
        destroy()
        val id = MeetingNotesCrypto.newKeyId()
        if (agreeInKeystore && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            try { return makeInKeystore(id) } catch (_: Exception) { runCatching { keystore().deleteEntry(agreeAlias) } }
        }
        return makeWrapped(id)
    }

    @RequiresApi(Build.VERSION_CODES.S)
    private fun makeInKeystore(id: String): NotesKey {
        val pair = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, STORE).apply {
            initialize(KeyGenParameterSpec.Builder(agreeAlias, KeyProperties.PURPOSE_AGREE_KEY)
                .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1")).build())
        }.generateKeyPair()
        val public = MeetingNotesCrypto.b64(MeetingNotesCrypto.rawPublic(pair.public as ECPublicKey))
        prefs.edit().putString(KEY_ID, id).putString(PUBLIC, public).putString(KIND, KIND_KEYSTORE).commit()
        return KeystoreAgreementKey(id, public, pair.private)
    }

    private fun makeWrapped(id: String): NotesKey {
        val pair = KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }.generateKeyPair()
        val pkcs8 = pair.private.encoded
        try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, wrappingKey())
            cipher.updateAAD(id.toByteArray())
            val bytes = byteArrayOf(1) + cipher.iv + cipher.doFinal(pkcs8)
            val stream = wrapped.startWrite()
            try { stream.write(bytes); wrapped.finishWrite(stream) } catch (failure: Throwable) { wrapped.failWrite(stream); throw failure }
        } finally { pkcs8.fill(0) }
        val public = MeetingNotesCrypto.b64(MeetingNotesCrypto.rawPublic(pair.public as ECPublicKey))
        prefs.edit().putString(KEY_ID, id).putString(PUBLIC, public).putString(KIND, KIND_WRAPPED).commit()
        return WrappedKey(id, public, wrapped, ::wrappingKey)
    }

    private fun wrappingKey(): SecretKey {
        (keystore().getKey(wrapAlias, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, STORE).apply {
            init(KeyGenParameterSpec.Builder(wrapAlias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256).setRandomizedEncryptionRequired(true).build())
        }.generateKey()
    }

    /** What the store reports about where the private half lives (shown in the Meeting notes screen). */
    val hardwareBacked: Boolean get() = prefs.getString(KIND, null) == KIND_KEYSTORE

    private class KeystoreAgreementKey(override val keyId: String, override val publicKey: String, private val private: PrivateKey) : NotesKey {
        override fun agree(peer: ECPublicKey): ByteArray = KeyAgreement.getInstance("ECDH", STORE).run { init(private); doPhase(peer, true); generateSecret() }
    }

    private class WrappedKey(override val keyId: String, override val publicKey: String, private val file: AtomicFile, private val wrapping: () -> SecretKey) : NotesKey {
        override fun agree(peer: ECPublicKey): ByteArray {
            val bytes = file.openRead().use { it.readBytes() }
            require(bytes.size in 30..4096 && bytes[0] == 1.toByte()) { "Unrecognized notes key." }
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, wrapping(), GCMParameterSpec(128, bytes.copyOfRange(1, 13)))
            cipher.updateAAD(keyId.toByteArray())
            val pkcs8 = cipher.doFinal(bytes.copyOfRange(13, bytes.size))
            try {
                val private = KeyFactory.getInstance("EC").generatePrivate(PKCS8EncodedKeySpec(pkcs8))
                return KeyAgreement.getInstance("ECDH").run { init(private); doPhase(peer, true); generateSecret() }
            } finally { pkcs8.fill(0) }
        }
    }

    companion object {
        private const val STORE = "AndroidKeyStore"
        private const val KEY_ID = "key_id"
        private const val PUBLIC = "public_key"
        private const val KIND = "kind"
        private const val KIND_KEYSTORE = "keystore"
        private const val KIND_WRAPPED = "wrapped"
    }
}
