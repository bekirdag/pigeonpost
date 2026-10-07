package dev.pigeonpost.inbox

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.graphics.pdf.PdfRenderer
import android.media.MediaMetadataRetriever
import android.os.Build
import android.os.ParcelFileDescriptor
import android.webkit.MimeTypeMap
import java.io.File
import kotlin.math.max
import kotlin.math.min

data class AttachmentThumbnail(val bitmap: Bitmap? = null, val text: String? = null)

object AttachmentThumbnails {
    fun type(name: String, mediaType: String): String {
        val mime = mediaType.substringBefore(';').trim().lowercase()
        return if (mime.isBlank() || mime == "application/octet-stream")
            MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase()).orEmpty() else mime
    }
    fun supports(name: String, mediaType: String): Boolean {
        val mime = type(name, mediaType)
        return (mime.startsWith("image/") && mime != "image/svg+xml") || mime.startsWith("video/") || mime.startsWith("audio/") ||
            mime in setOf("application/pdf", "text/plain", "text/csv", "text/markdown", "application/json")
    }
    fun render(file: File, name: String, mediaType: String): AttachmentThumbnail? {
        val mime = type(name, mediaType)
        return when {
            mime.startsWith("image/") -> {
                val bitmap = if (Build.VERSION.SDK_INT >= 28) ImageDecoder.decodeBitmap(ImageDecoder.createSource(file)) { decoder, info, _ ->
                    require(info.size.width.toLong() * info.size.height <= 64L * 1024 * 1024)
                    val scale = min(1.0, min(440.0 / info.size.width, 300.0 / info.size.height))
                    decoder.setTargetSize(max(1, (info.size.width * scale).toInt()), max(1, (info.size.height * scale).toInt()))
                    decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
                } else {
                    val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                    BitmapFactory.decodeFile(file.path, options)
                    require(options.outWidth > 0 && options.outHeight > 0 && options.outWidth.toLong() * options.outHeight <= 64L * 1024 * 1024)
                    options.inSampleSize = 1
                    while (options.outWidth / options.inSampleSize > 880 || options.outHeight / options.inSampleSize > 600) options.inSampleSize *= 2
                    options.inJustDecodeBounds = false
                    BitmapFactory.decodeFile(file.path, options)
                }
                bitmap?.let { AttachmentThumbnail(bitmap = it) }
            }
            mime == "application/pdf" -> ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY).use { descriptor ->
                PdfRenderer(descriptor).use { renderer ->
                    if (renderer.pageCount == 0) null else renderer.openPage(0).use { page ->
                        val scale = min(440.0 / page.width, 300.0 / page.height)
                        val bitmap = Bitmap.createBitmap(max(1, (page.width * scale).toInt()), max(1, (page.height * scale).toInt()), Bitmap.Config.ARGB_8888)
                        bitmap.eraseColor(android.graphics.Color.WHITE)
                        page.render(bitmap, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
                        AttachmentThumbnail(bitmap = bitmap)
                    }
                }
            }
            mime.startsWith("video/") || mime.startsWith("audio/") -> {
                val retriever = MediaMetadataRetriever()
                try {
                    retriever.setDataSource(file.path)
                    if (mime.startsWith("video/")) {
                        retriever.getScaledFrameAtTime(1_000_000, MediaMetadataRetriever.OPTION_CLOSEST_SYNC, 440, 300)?.let { AttachmentThumbnail(bitmap = it) }
                    } else {
                        val duration = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()?.div(1000)
                        AttachmentThumbnail(text = "♫ $name" + (duration?.let { " · $it sec" } ?: ""))
                    }
                } finally { retriever.release() }
            }
            else -> file.reader(Charsets.UTF_8).use { reader ->
                val chars = CharArray(1200)
                val count = reader.read(chars)
                AttachmentThumbnail(text = if (count > 0) String(chars, 0, count) else "Empty document")
            }
        }
    }
}
