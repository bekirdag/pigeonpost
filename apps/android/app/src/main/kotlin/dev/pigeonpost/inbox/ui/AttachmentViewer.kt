package dev.pigeonpost.inbox.ui

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.graphics.pdf.PdfRenderer
import android.os.Build
import android.os.ParcelFileDescriptor
import android.widget.MediaController
import android.widget.VideoView
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import dev.pigeonpost.core.Attachment
import dev.pigeonpost.core.AttachmentKinds
import dev.pigeonpost.core.ViewerKind
import dev.pigeonpost.inbox.PlatformFiles
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.File
import kotlin.math.max
import kotlin.math.min

private sealed interface Loaded {
    data object Waiting : Loaded
    data class Ready(val file: File) : Loaded
    data class Failed(val reason: String) : Loaded
}

/**
 * Images, video, audio, PDF and plain text open inside the app, with a download button. HTML, SVG and
 * other markup never reach this viewer (AttachmentKinds.viewer), and text is shown as text.
 */
@Composable
fun AttachmentViewer(identity: String, attachment: Attachment, load: suspend () -> File, download: () -> Unit, dismiss: () -> Unit) {
    val kind = AttachmentKinds.viewer(attachment.filename, attachment.mediaType)
    if (kind == null) { LaunchedEffect(Unit) { dismiss() }; return }
    val p = Pp.colors
    val loaded by produceState<Loaded>(Loaded.Waiting, identity, attachment.id) {
        val file = try { load() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { value = Loaded.Failed(failure.message ?: "Could not open the attachment."); return@produceState }
        value = Loaded.Ready(file)
        awaitDispose { file.parentFile?.deleteRecursively() }
    }
    val media = kind == ViewerKind.IMAGE || kind == ViewerKind.VIDEO
    Dialog(dismiss, DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Column(Modifier.fillMaxSize().background(if (media) Color(0xFF0B0D10) else p.wash).windowInsetsPadding(WindowInsets.safeDrawing).testTag("attachmentViewer")) {
            Row(Modifier.fillMaxWidth().height(56.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                val ink = if (media) Color.White else p.ink
                IconButton(dismiss) { Icon(Icons.Outlined.Close, "Close viewer", tint = ink) }
                Column(Modifier.weight(1f).padding(horizontal = 4.dp)) {
                    Text(attachment.filename, style = geist(15.sp, FontWeight.SemiBold), color = ink, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                    Text(bytes(attachment.bytes), style = geist(12.sp), color = if (media) Color(0xFF9AA3AE) else p.muted)
                }
                IconButton(download) { Icon(Icons.Outlined.Download, "Download ${attachment.filename}", tint = ink) }
            }
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                when (val state = loaded) {
                    Loaded.Waiting -> CircularProgressIndicator(color = p.accent)
                    is Loaded.Failed -> Text(state.reason, Modifier.padding(24.dp), color = if (media) Color.White else p.secondary)
                    is Loaded.Ready -> when (kind) {
                        ViewerKind.IMAGE -> ImageView(state.file, attachment.filename)
                        ViewerKind.VIDEO, ViewerKind.AUDIO -> MediaView(state.file, kind == ViewerKind.AUDIO)
                        ViewerKind.PDF -> PdfView(state.file)
                        ViewerKind.TEXT -> TextView(state.file)
                    }
                }
            }
        }
    }
}

@Composable
private fun ImageView(file: File, name: String) {
    val bitmap by produceState<Bitmap?>(null, file) { value = withContext(Dispatchers.IO) { runCatching { decode(file, 4096) }.getOrNull() } }
    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(androidx.compose.ui.geometry.Offset.Zero) }
    val image = bitmap
    if (image == null) { CircularProgressIndicator(color = Pp.colors.accent); return }
    Image(image.asImageBitmap(), name, Modifier.fillMaxSize()
        .pointerInput(Unit) { detectTransformGestures { _, pan, zoom, _ -> scale = (scale * zoom).coerceIn(1f, 6f); offset = if (scale == 1f) androidx.compose.ui.geometry.Offset.Zero else offset + pan } }
        .graphicsLayer { scaleX = scale; scaleY = scale; translationX = offset.x; translationY = offset.y }, contentScale = ContentScale.Fit)
}

private fun decode(file: File, longest: Int): Bitmap? = if (Build.VERSION.SDK_INT >= 28) ImageDecoder.decodeBitmap(ImageDecoder.createSource(file)) { decoder, info, _ ->
    require(info.size.width.toLong() * info.size.height <= 100L * 1024 * 1024)
    val scale = min(1.0, longest.toDouble() / max(info.size.width, info.size.height))
    decoder.setTargetSize(max(1, (info.size.width * scale).toInt()), max(1, (info.size.height * scale).toInt()))
} else {
    val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(file.path, options)
    options.inSampleSize = 1
    while (max(options.outWidth, options.outHeight) / options.inSampleSize > longest) options.inSampleSize *= 2
    options.inJustDecodeBounds = false
    BitmapFactory.decodeFile(file.path, options)
}

@Composable
private fun MediaView(file: File, audio: Boolean) {
    AndroidView({ context ->
        VideoView(context).apply {
            val controls = MediaController(context)
            controls.setAnchorView(this)
            setMediaController(controls)
            setVideoPath(file.path)
            setOnPreparedListener { start(); controls.show(0) }
            contentDescription = if (audio) "Audio player" else "Video player"
        }
    }, Modifier.then(if (audio) Modifier.fillMaxWidth().height(120.dp) else Modifier.fillMaxSize()), onRelease = { it.stopPlayback() })
}

@Composable
private fun PdfView(file: File) {
    val p = Pp.colors
    val opened = remember(file) {
        runCatching { ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY).let { it to PdfRenderer(it) } }.getOrNull()
    }
    DisposableEffect(opened) { onDispose { opened?.second?.close(); opened?.first?.close() } }
    if (opened == null) { Text("This PDF could not be opened.", color = p.secondary); return }
    val renderer = opened.second
    val lock = remember(renderer) { Mutex() }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val widthPx = with(LocalDensity.current) { (maxWidth - 24.dp).toPx().toInt().coerceIn(200, 2000) }
        LazyColumn(Modifier.fillMaxSize().testTag("pdfPages"), contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            items(renderer.pageCount) { index ->
                // Every page is drawn lazily, only when it scrolls into view.
                val page by produceState<Bitmap?>(null, index, widthPx) {
                    value = withContext(Dispatchers.IO) {
                        lock.withLock {
                            runCatching {
                                renderer.openPage(index).use { source ->
                                    val height = (widthPx.toLong() * source.height / source.width).toInt().coerceIn(1, 4000)
                                    Bitmap.createBitmap(widthPx, height, Bitmap.Config.ARGB_8888).also {
                                        it.eraseColor(android.graphics.Color.WHITE)
                                        source.render(it, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
                                    }
                                }
                            }.getOrNull()
                        }
                    }
                }
                val bitmap = page
                if (bitmap == null) Box(Modifier.fillMaxWidth().aspectRatio(0.707f).background(p.ground))
                else Image(bitmap.asImageBitmap(), "Page ${index + 1}", Modifier.fillMaxWidth(), contentScale = ContentScale.FillWidth)
            }
        }
    }
}

@Composable
private fun TextView(file: File) {
    val p = Pp.colors
    val text by produceState<String?>(null, file) {
        value = withContext(Dispatchers.IO) {
            val limit = 1024 * 1024
            val bytes = file.inputStream().use { it.readNBytesCompat(limit + 1) }
            String(bytes, 0, min(bytes.size, limit), Charsets.UTF_8) + if (bytes.size > limit) "\n\n… (shortened; download the file to read all of it)" else ""
        }
    }
    val content = text ?: run { CircularProgressIndicator(color = p.accent); return }
    SelectionContainer(Modifier.fillMaxSize()) {
        Text(content, Modifier.fillMaxSize().verticalScroll(rememberScrollState()).horizontalScroll(rememberScrollState()).padding(16.dp),
            style = geistMono(13.sp, color = p.body).copy(lineHeight = 19.sp))
    }
}

private fun java.io.InputStream.readNBytesCompat(limit: Int): ByteArray {
    val out = java.io.ByteArrayOutputStream()
    val buffer = ByteArray(64 * 1024)
    while (out.size() < limit) {
        val count = read(buffer, 0, min(buffer.size, limit - out.size()))
        if (count < 0) break
        out.write(buffer, 0, count)
    }
    return out.toByteArray()
}
