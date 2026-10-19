package dev.pigeonpost.core

/** What the in-app viewer may show. Active or markup content (HTML, SVG, XML) is never rendered. */
enum class ViewerKind { IMAGE, VIDEO, AUDIO, PDF, TEXT }

object AttachmentKinds {
    private val extensions = mapOf(
        "png" to "image/png", "jpg" to "image/jpeg", "jpeg" to "image/jpeg", "gif" to "image/gif", "webp" to "image/webp",
        "heic" to "image/heic", "heif" to "image/heif", "bmp" to "image/bmp",
        "mp4" to "video/mp4", "m4v" to "video/mp4", "mov" to "video/quicktime", "webm" to "video/webm", "3gp" to "video/3gpp", "mkv" to "video/x-matroska",
        "mp3" to "audio/mpeg", "m4a" to "audio/mp4", "aac" to "audio/aac", "wav" to "audio/wav", "ogg" to "audio/ogg", "oga" to "audio/ogg", "opus" to "audio/opus", "flac" to "audio/flac",
        "pdf" to "application/pdf", "txt" to "text/plain", "log" to "text/plain", "md" to "text/markdown", "csv" to "text/csv", "json" to "application/json",
        "svg" to "image/svg+xml", "html" to "text/html", "htm" to "text/html", "xml" to "application/xml",
    )
    private val markup = setOf("image/svg+xml", "text/html", "application/xhtml+xml", "application/xml", "text/xml")
    private val plainText = setOf("text/plain", "text/csv", "text/markdown", "application/json")

    /** The effective type: the declared one, or the file extension's when the declared one is generic. */
    fun type(name: String, mediaType: String): String {
        val declared = mediaType.substringBefore(';').trim().lowercase()
        val byName = extensions[name.substringAfterLast('.', "").lowercase()]
        return if (declared.isBlank() || declared == "application/octet-stream") byName ?: declared else declared
    }

    /** Null when the file opens only outside the app (save or share). */
    fun viewer(name: String, mediaType: String): ViewerKind? {
        val mime = type(name, mediaType)
        // A declared harmless type cannot launder a markup file name, nor the reverse.
        if (mime in markup || extensions[name.substringAfterLast('.', "").lowercase()] in markup) return null
        return when {
            mime.startsWith("image/") -> ViewerKind.IMAGE
            mime.startsWith("video/") -> ViewerKind.VIDEO
            mime.startsWith("audio/") -> ViewerKind.AUDIO
            mime == "application/pdf" -> ViewerKind.PDF
            mime in plainText -> ViewerKind.TEXT
            else -> null
        }
    }

    /** Photos and videos are saved to the gallery; everything else through the system's save dialog. */
    fun savesToGallery(name: String, mediaType: String) = viewer(name, mediaType).let { it == ViewerKind.IMAGE || it == ViewerKind.VIDEO }
}
