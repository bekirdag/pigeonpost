package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pigeonpost.core.PeerFace
import dev.pigeonpost.inbox.AvatarPictures
import androidx.compose.ui.semantics.testTag
import kotlinx.coroutines.delay
import java.text.DateFormat
import java.util.Date

@Suppress("DEPRECATION")
@Composable
fun CopyAddressButton(address: String, modifier: Modifier = Modifier) {
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    var copied by remember(address) { mutableStateOf(false) }
    var revision by remember(address) { mutableIntStateOf(0) }
    LaunchedEffect(address, revision) {
        if (copied) { delay(2000); copied = false }
    }
    IconButton(onClick = {
        try {
            clipboard.setText(AnnotatedString(address))
            copied = true
            revision++
        } catch (_: SecurityException) {
            android.widget.Toast.makeText(context, "Couldn’t copy the address. Select it and copy manually.", android.widget.Toast.LENGTH_LONG).show()
        }
    }, enabled = address.isNotEmpty(), modifier = modifier.size(48.dp).semantics { stateDescription = if (copied) "Copied" else "" }) {
        Icon(if (copied) Icons.Outlined.Check else Icons.Outlined.ContentCopy, "Copy address $address", Modifier.size(18.dp), tint = Pp.colors.secondary)
    }
}

@Composable
fun PostAddressRow(address: String, modifier: Modifier = Modifier) {
    Row(modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        SelectionContainer(Modifier.weight(1f)) {
            Text(address, style = geistMono(13.sp), color = Pp.colors.secondary, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
        CopyAddressButton(address)
    }
}

/** Profile pictures for every face on screen; null where pictures are off (fixtures without them, previews). */
val LocalAvatars = staticCompositionLocalOf<AvatarPictures?> { null }

/**
 * A tinted disc with the initials in the same hue; the tone is hashed from the address, as on the
 * web. When the person has a picture this account may see, it is laid over the initials, which stay
 * underneath as the fallback.
 */
@Composable
fun Avatar(peer: String?, name: String, size: Dp = 38.dp) {
    val index = PeerFace.toneIndex(peer) - 1
    val pictures = LocalAvatars.current
    val known by (pictures?.store?.pictures ?: remember { kotlinx.coroutines.flow.MutableStateFlow(emptyMap()) }).collectAsState()
    val picture = peer?.let { known[it] }
    if (pictures != null && peer != null && peer !in known) LaunchedEffect(peer) { pictures.store.want(peer) }
    val image by produceState(picture?.let { pictures?.cached(it) }, picture?.cacheKey) {
        if (picture != null && pictures != null && value == null) value = pictures.bitmap(peer, picture)
        else if (picture == null) value = null
    }
    Box(Modifier.size(size).clip(CircleShape).background(Pp.colors.tones[index], CircleShape).semantics { testTag = "avatar:" + (peer ?: "") + if (image != null) ":photo" else "" },
        contentAlignment = Alignment.Center) {
        Text(PeerFace.initials(name), color = Pp.colors.toneInk[index], style = geist((size.value * 0.32f).sp, FontWeight.Bold), maxLines = 1)
        image?.let { androidx.compose.foundation.Image(it, null, Modifier.fillMaxSize(), contentScale = androidx.compose.ui.layout.ContentScale.Crop) }
    }
}

/** A group's face: its initials on a tone hashed from its id (groups have no pictures yet). */
@Composable
fun GroupAvatar(id: String, name: String, size: Dp = 38.dp) {
    val index = PeerFace.toneIndex(id) - 1
    Box(Modifier.size(size).background(Pp.colors.tones[index], RoundedCornerShape(size * 0.3f)), contentAlignment = Alignment.Center) {
        val words = name.split(' ').filter { w -> w.any(Char::isLetterOrDigit) }
        val letters = if (words.size >= 2) "${words[0].first(Char::isLetterOrDigit)}${words[1].first(Char::isLetterOrDigit)}".uppercase() else PeerFace.initials(name)
        Text(letters, color = Pp.colors.toneInk[index], style = geist((size.value * 0.32f).sp, FontWeight.Bold), maxLines = 1)
    }
}

@Composable
fun ActionIcon(label: String, icon: ImageVector, enabled: Boolean = true, tint: Color = Pp.colors.secondary, action: () -> Unit) {
    IconButton(action, enabled = enabled) { Icon(icon, label, Modifier.size(22.dp), tint = if (enabled) tint else tint.copy(alpha = .38f)) }
}

/** The server's decisions as small capitalised chips: held (amber), auto (green), blocked (rose). */
enum class PillKind { HELD, AUTO, BLOCKED, NEUTRAL }

@Composable
fun Pill(text: String, kind: PillKind, dot: Boolean = kind == PillKind.HELD) {
    val p = Pp.colors
    val (ink, fill, stroke) = when (kind) {
        PillKind.HELD -> Triple(p.heldText, p.heldPillFill, p.heldPillStroke)
        PillKind.AUTO -> Triple(p.autoText, p.autoFill, p.autoStroke)
        PillKind.BLOCKED -> Triple(p.blockedText, p.blockedFill, p.blockedStroke)
        PillKind.NEUTRAL -> Triple(p.secondary, p.tile, p.stroke)
    }
    Row(Modifier.clip(RoundedCornerShape(6.dp)).background(fill).border(1.dp, stroke, RoundedCornerShape(6.dp)).padding(horizontal = 7.dp, vertical = 3.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
        if (dot) Box(Modifier.size(5.dp).background(ink, CircleShape))
        Text(text, color = ink, style = geist(11.sp, FontWeight.SemiBold), maxLines = 1)
    }
}

/** Kept for the existing call sites: a chip in the server-decision colours. */
@Composable
fun BadgeText(text: String, color: Color) {
    val p = Pp.colors
    val kind = when (color) { p.heldText, p.amber -> PillKind.HELD; p.autoText, p.green -> PillKind.AUTO; p.blockedText -> PillKind.BLOCKED; else -> PillKind.NEUTRAL }
    Pill(text, kind, dot = kind == PillKind.HELD)
}

@Composable
fun UnreadBadge(count: Int, inverted: Boolean = false) {
    val p = Pp.colors
    Box(Modifier.defaultMinSize(20.dp, 20.dp).background(if (inverted) p.ink else p.accent, CircleShape).padding(horizontal = 5.dp), contentAlignment = Alignment.Center) {
        Text(if (count > 99) "99+" else count.toString(), color = if (inverted) p.app else p.onAccent, style = geist(11.sp, FontWeight.SemiBold))
    }
}

@Composable
fun EmptyPane(title: String, detail: String, modifier: Modifier = Modifier) {
    Box(modifier.fillMaxSize().padding(28.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Box(Modifier.size(52.dp).background(Pp.colors.tile, RoundedCornerShape(14.dp)), contentAlignment = Alignment.Center) {
                Icon(Icons.Outlined.Forum, null, Modifier.size(26.dp), tint = Pp.colors.secondary)
            }
            Text(title, style = geist(16.sp, FontWeight.SemiBold), color = Pp.colors.ink, textAlign = TextAlign.Center)
            Text(detail, color = Pp.colors.muted, style = geist(13.5.sp), textAlign = TextAlign.Center)
        }
    }
}

@Composable
fun Loading(label: String) {
    Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
        CircularProgressIndicator(color = Pp.colors.accent); Text(label, Modifier.padding(20.dp), color = Pp.colors.secondary, style = geist(14.sp))
    }
}

/** A hairline seam, between columns or under a bar. */
@Composable fun Seam(modifier: Modifier = Modifier, color: Color = Pp.colors.rule) = Box(modifier.background(color))

fun shortTime(epoch: Long): String = if (epoch <= 0) "" else {
    val today = java.time.Instant.now().atZone(java.time.ZoneId.systemDefault()).toLocalDate()
    val date = java.time.Instant.ofEpochSecond(epoch).atZone(java.time.ZoneId.systemDefault()).toLocalDate()
    (if (today == date) DateFormat.getTimeInstance(DateFormat.SHORT) else DateFormat.getDateInstance(DateFormat.SHORT)).format(Date(epoch * 1000))
}
fun bytes(value: Long): String = when { value >= 1024 * 1024 -> "%.1f MB".format(value / (1024.0 * 1024)); value >= 1024 -> "%.0f KB".format(value / 1024.0); else -> "$value B" }
