package dev.pigeonpost.inbox

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import dev.pigeonpost.core.AvatarInfo
import dev.pigeonpost.core.AvatarStore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.security.MessageDigest

/**
 * Turns pictures from [AvatarStore] into bitmaps for Compose, and keeps the latest one per
 * address in the app's cache directory so a notification can show the sender's face (`Person`
 * icon) without a network request. Pictures are never decoded larger than 256 px.
 */
class AvatarPictures(context: Context, val store: AvatarStore) {
    private val dir = File(context.cacheDir, "avatars")
    private val memory = object : LruCache<String, ImageBitmap>(8 * 1024 * 1024) {
        override fun sizeOf(key: String, value: ImageBitmap) = value.width * value.height * 4
    }

    fun cached(picture: AvatarInfo): ImageBitmap? = memory.get(picture.cacheKey)

    suspend fun bitmap(key: String, picture: AvatarInfo): ImageBitmap? {
        memory.get(picture.cacheKey)?.let { return it }
        val bytes = store.bytes(picture) ?: return null
        return withContext(Dispatchers.Default) {
            val decoded = decode(bytes, 256) ?: return@withContext null
            withContext(Dispatchers.IO) { runCatching { dir.mkdirs(); file(dir, key).writeBytes(bytes) } }
            decoded.asImageBitmap().also { memory.put(picture.cacheKey, it) }
        }
    }

    fun clear() { memory.evictAll(); dir.deleteRecursively() }

    companion object {
        fun file(dir: File, key: String): File = File(dir, MessageDigest.getInstance("SHA-256").digest(key.toByteArray()).joinToString("") { "%02x".format(it) }.take(32))

        /** The face last seen for `key`, for a notification. */
        fun forNotification(context: Context, key: String): Bitmap? {
            val f = file(File(context.cacheDir, "avatars"), key)
            if (!f.isFile || f.length() > 256 * 1024) return null
            return runCatching { decode(f.readBytes(), 128) }.getOrNull()
        }

        fun decode(bytes: ByteArray, longest: Int): Bitmap? {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
            if (bounds.outWidth <= 0 || bounds.outHeight <= 0 || bounds.outWidth.toLong() * bounds.outHeight > 4096L * 4096) return null
            var sample = 1
            while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= longest) sample *= 2
            return BitmapFactory.decodeByteArray(bytes, 0, bytes.size, BitmapFactory.Options().apply { inSampleSize = sample })
        }
    }
}
