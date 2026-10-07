package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import dev.pigeonpost.inbox.AttachmentThumbnail
import dev.pigeonpost.inbox.AttachmentThumbnails
import kotlinx.coroutines.CancellationException

@Composable
fun AttachmentPreview(key: String, name: String, mediaType: String, compact: Boolean = false,
    load: suspend () -> AttachmentThumbnail?) {
    if (!AttachmentThumbnails.supports(name, mediaType)) return
    val currentLoad by rememberUpdatedState(load)
    val preview by produceState<AttachmentThumbnail?>(null, key) {
        value = try { currentLoad() } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null }
    }
    Box(Modifier.size(if (compact) 72.dp else 220.dp, if (compact) 54.dp else 150.dp)
        .clip(RoundedCornerShape(8.dp)).background(MaterialTheme.colorScheme.surface), contentAlignment = Alignment.Center) {
        preview?.bitmap?.let { Image(it.asImageBitmap(), "Preview of $name", Modifier.fillMaxSize(), contentScale = ContentScale.Fit) }
            ?: Text(preview?.text ?: "Preview unavailable", Modifier.padding(6.dp), maxLines = if (compact) 2 else 8,
                style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
