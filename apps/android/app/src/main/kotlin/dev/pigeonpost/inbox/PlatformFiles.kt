package dev.pigeonpost.inbox

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import dev.pigeonpost.core.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import java.io.File
import java.util.UUID

/** Private staging plus narrowly scoped FileProvider URIs; no storage permission required. */
class PlatformFiles(private val context: Context, private val api: PostboxApi) {
    private val staging = File(context.cacheDir, "staged")
    private val received = File(context.cacheDir, "received")
    suspend fun stage(uris: List<Uri>): List<StagedAttachment> = withContext(Dispatchers.IO) {
        require(uris.size <= MAX_ATTACHMENTS) { "Choose at most $MAX_ATTACHMENTS attachments." }
        staging.mkdirs()
        val result = mutableListOf<StagedAttachment>()
        try {
            for (uri in uris) {
                require(uri.scheme == "content") { "Choose a file using the Android file picker." }
                var name = "Attachment"
                context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { cursor ->
                    if (cursor.moveToFirst()) {
                        val size = cursor.getColumnIndex(OpenableColumns.SIZE)
                        if (size >= 0 && !cursor.isNull(size)) require(cursor.getLong(size) <= MAX_ATTACHMENT_BYTES) { "Attachments must be 20 MB or smaller." }
                        val column = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                        if (column >= 0) name = safeFilename(cursor.getString(column) ?: name)
                    }
                }
                val file = File(staging, UUID.randomUUID().toString())
                val entry = StagedAttachment(file = file, name = name, mediaType = context.contentResolver.getType(uri) ?: "application/octet-stream")
                result += entry
                val input = context.contentResolver.openInputStream(uri) ?: error("Could not open $name.")
                input.use { source -> file.outputStream().use { sink ->
                    val buffer = ByteArray(64 * 1024); var total = 0L
                    while (true) {
                        currentCoroutineContext().ensureActive()
                        val count = source.read(buffer); if (count < 0) break
                        total += count; require(total <= MAX_ATTACHMENT_BYTES) { "Attachments must be 20 MB or smaller." }
                        sink.write(buffer, 0, count)
                    }
                } }
            }
            result
        } catch (failure: Exception) { result.forEach { it.file.delete() }; throw failure }
    }
    suspend fun download(identity: String, attachment: Attachment): File = withContext(Dispatchers.IO) {
        require(attachment.bytes <= MAX_ATTACHMENT_BYTES) { "Open attachments up to 20 MB on this device." }
        val directory = File(received, UUID.randomUUID().toString()).apply { mkdirs() }
        val file = File(directory, safeFilename(attachment.filename))
        try { file.outputStream().use { api.download(identity, attachment.id, it) }; file }
        catch (failure: Exception) { directory.deleteRecursively(); throw failure }
    }
    suspend fun save(file: File, uri: Uri) = withContext(Dispatchers.IO) {
        require(uri.scheme == "content")
        val output = context.contentResolver.openOutputStream(uri, "wt") ?: error("Could not save the file.")
        output.use { sink -> file.inputStream().use { source -> source.copyTo(sink) } }
    }
    fun clear() { staging.deleteRecursively(); received.deleteRecursively() }
}

fun safeFilename(name: String): String = name.substringAfterLast('/').substringAfterLast('\\')
    .map { if (it.isISOControl() || it in ":*?\"<>|") '_' else it }.joinToString("")
    .trim().trim('.').take(120).ifBlank { "Attachment" }
