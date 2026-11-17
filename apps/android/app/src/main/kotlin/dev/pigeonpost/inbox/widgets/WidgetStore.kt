package dev.pigeonpost.inbox.widgets

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import dev.pigeonpost.core.widgets.WidgetSnapshot
import dev.pigeonpost.core.widgets.WidgetSnapshots
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The one place widget data lives (docs/widgets-plan-android.md §3): a single snapshot file in
 * app-private no-backup storage, sealed with an AES-256-GCM key that never leaves Android Keystore.
 * The manifest already keeps every app file out of cloud backup and device transfer; no-backup
 * storage and the Keystore key keep this one out even if that ever changes, and a copied file is
 * unreadable on any other device.
 *
 * The app is the only writer. Widgets read it in the app's own process and get changes through
 * [snapshots]. Sign-out deletes the file and the key.
 */
object WidgetStore {
    private const val ALIAS = "pigeonpost.widgets.v1"
    private const val FORMAT: Byte = 1
    private val AAD = "pigeonpost-widget-snapshot".toByteArray()
    private val lock = Mutex()
    private val state = MutableStateFlow<WidgetSnapshot?>(null)
    @Volatile private var loaded = false

    private fun file(context: Context) = AtomicFile(File(File(context.noBackupFilesDir, "widgets").apply { mkdirs() }, "snapshot.enc"))

    private fun key(create: Boolean): SecretKey? {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        if (!create) return null
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256).setRandomizedEncryptionRequired(true).build())
        }.generateKey()
    }

    /** The current snapshot, read from disk once per process. */
    suspend fun snapshots(context: Context): StateFlow<WidgetSnapshot?> {
        if (!loaded) lock.withLock { if (!loaded) { state.value = readLocked(context.applicationContext); loaded = true } }
        return state.asStateFlow()
    }

    suspend fun read(context: Context): WidgetSnapshot? = snapshots(context).value

    /** Replace the snapshot with what [change] makes of the current one; null clears it. */
    suspend fun update(context: Context, change: (WidgetSnapshot?) -> WidgetSnapshot?): Boolean {
        val app = context.applicationContext
        return lock.withLock {
            if (!loaded) { state.value = readLocked(app); loaded = true }
            val before = state.value
            val after = change(before)
            if (after == before) return@withLock false
            if (after == null) clearLocked(app) else writeLocked(app, after)
            state.value = after
            true
        }
    }

    suspend fun write(context: Context, snapshot: WidgetSnapshot) = update(context) { snapshot }

    /** Sign-out: the file, its key and the in-memory copy are gone. */
    suspend fun clear(context: Context) = update(context) { null }.also {
        // Even if nothing was cached in memory, a file or key from an earlier process must go.
        lock.withLock { clearLocked(context.applicationContext) }
    }

    private suspend fun readLocked(context: Context): WidgetSnapshot? = withContext(Dispatchers.IO) {
        val file = file(context)
        if (!file.baseFile.exists()) return@withContext null
        try {
            val bytes = file.openRead().use { it.readBytes() }
            require(bytes.size > 29 && bytes[0] == FORMAT)
            val key = key(create = false) ?: return@withContext null
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, bytes.copyOfRange(1, 13)))
            cipher.updateAAD(AAD)
            WidgetSnapshots.decode(cipher.doFinal(bytes.copyOfRange(13, bytes.size)).toString(Charsets.UTF_8))
        } catch (_: Exception) {
            null // a snapshot this key cannot open, or of another version, shows as signed out
        }
    }

    private suspend fun writeLocked(context: Context, snapshot: WidgetSnapshot) = withContext(Dispatchers.IO) {
        val plain = WidgetSnapshots.encode(snapshot).toByteArray(Charsets.UTF_8)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key(create = true))
        require(cipher.iv.size == 12)
        cipher.updateAAD(AAD)
        val sealed = byteArrayOf(FORMAT) + cipher.iv + cipher.doFinal(plain)
        val file = file(context)
        val stream = file.startWrite()
        try { stream.write(sealed); file.finishWrite(stream) }
        catch (failure: Throwable) { file.failWrite(stream); throw failure }
    }

    private suspend fun clearLocked(context: Context) = withContext(Dispatchers.IO) {
        file(context).delete()
        runCatching { KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.deleteEntry(ALIAS) }
        Unit
    }

    // ---- the one widget setting ------------------------------------------------------------------

    private const val PREFERENCES = "widgets"
    private const val SHOW_PREVIEWS = "show_previews"
    private val previews = MutableStateFlow<Boolean?>(null)

    /** "Show message previews in widgets": off unless the person turns it on, and off again after sign-out. */
    fun previewsEnabled(context: Context): StateFlow<Boolean?> {
        if (previews.value == null) previews.value = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getBoolean(SHOW_PREVIEWS, false)
        return previews.asStateFlow()
    }

    fun setPreviews(context: Context, show: Boolean) {
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putBoolean(SHOW_PREVIEWS, show).apply()
        previews.value = show
    }

    fun resetSettings(context: Context) {
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().clear().apply()
        previews.value = false
    }
}
