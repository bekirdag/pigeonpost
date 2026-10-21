@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
package dev.pigeonpost.inbox.ui

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.graphics.Matrix
import android.net.Uri
import android.os.Build
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.ClipOp
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.exifinterface.media.ExifInterface
import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.GroupStore
import dev.pigeonpost.core.groups.GroupsState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/** Settings → Privacy & filtering: the six controls, each shown only when its feature is on. */
@Composable
fun PrivacySettings(state: InboxState, store: InboxStore, blocked: () -> Unit) {
    val p = Pp.colors
    LaunchedEffect(state.acting?.address) { store.loadPolicy() }
    val policy = state.policy
    Text("These settings belong to ${state.acting?.name ?: "this inbox"} and apply on every device.", style = geist(13.sp), color = p.secondary)
    state.policyError?.let { Text(it, color = p.blockedText, style = geist(13.sp)) }
    POLICY_ROWS.filter { state.has(it.feature) }.forEach { row ->
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(row.title, style = geist(14.5.sp, FontWeight.Medium), color = p.ink)
            SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().testTag("policy:" + row.field)) {
                row.options.forEachIndexed { i, (value, label) ->
                    SegmentedButton(policy?.value(row.field) == value, { if (policy != null && policy.value(row.field) != value) store.setPolicy(row.field, value) },
                        SegmentedButtonDefaults.itemShape(i, row.options.size), enabled = policy != null,
                        colors = SegmentedButtonDefaults.colors(activeContainerColor = p.chip, activeContentColor = p.ink, inactiveContainerColor = p.wash,
                            inactiveContentColor = p.secondary, activeBorderColor = p.strokeStrong, inactiveBorderColor = p.strokeStrong), icon = {}) {
                        Text(label, style = geist(12.5.sp, FontWeight.Medium), maxLines = 1)
                    }
                }
            }
            row.note?.let { Text(it, style = geist(12.sp), color = p.muted) }
        }
    }
    HorizontalDivider()
    SettingsLink("Blocked", "People and namespaces you blocked", blocked)
}

@Composable
private fun SettingsLink(title: String, detail: String, open: () -> Unit) {
    val p = Pp.colors
    Surface(shape = MaterialTheme.shapes.medium, color = p.wash, border = androidx.compose.foundation.BorderStroke(1.dp, p.stroke)) {
        ListItem(headlineContent = { Text(title, style = geist(15.sp, FontWeight.Medium), color = p.ink) }, supportingContent = { Text(detail, style = geist(13.sp), color = p.secondary) },
            colors = ListItemDefaults.colors(containerColor = Color.Transparent), modifier = Modifier.fillMaxWidth().selectable(false, onClick = open))
    }
}

/** Settings → Privacy → Blocked: exact and wildcard blocks, each with Unblock (a person's decision only). */
@Composable
fun BlockedSettings(state: InboxState, store: InboxStore) {
    val p = Pp.colors
    var address by rememberSaveable { mutableStateOf("") }
    var confirm by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(state.acting?.address) { store.loadBlocked() }
    Text("Blocked people can’t message or call you, and aren’t told.", style = geist(13.sp), color = p.secondary)
    state.blockedError?.let { Text(it, color = p.blockedText, style = geist(13.sp)) }
    val rows = state.blocked
    when {
        rows == null -> Text("Loading…", color = p.muted)
        rows.isEmpty() -> Text("Nobody is blocked.", color = p.muted, modifier = Modifier.testTag("blockedEmpty"))
        else -> rows.sortedBy { it.peer }.forEach { c ->
            Row(Modifier.fillMaxWidth().testTag("blocked:" + c.peer), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(c.peer, style = geistMono(13.5.sp, FontWeight.Medium), color = p.ink)
                    Text(if (c.wildcard) "Everyone at ${c.peer.removeSuffix("/*")}" else c.alias ?: "Blocked", style = geist(12.sp), color = p.muted)
                }
                TextButton({ confirm = c.peer }, enabled = !state.actionBusy, modifier = Modifier.semantics { contentDescription = "Unblock ${c.peer}" }) { Text("Unblock") }
            }
        }
    }
    HorizontalDivider()
    OutlinedTextField(address, { address = conversationAddressInput(it).take(300) }, Modifier.fillMaxWidth(), singleLine = true,
        label = { Text("Block an address") }, placeholder = { Text("/spamco/agent or /spamco/*") },
        isError = address.length > 1 && !validAddress(address, wildcard = true))
    val self = address == state.acting?.address || address == state.acting?.handle
    if (self) Text("You cannot block this mailbox itself.", color = p.blockedText, style = geist(12.sp))
    OutlinedButton({ store.blockSender(address) { address = "" }; store.loadBlocked() }, Modifier.fillMaxWidth(), enabled = validAddress(address, wildcard = true) && !self && !state.actionBusy) { Text("Block") }
    confirm?.let { peer -> Confirm("Unblock $peer?", "They will be able to message and call you again. Messages they sent while blocked were refused and are not delivered.", "Unblock",
        { store.unblock(peer); confirm = null }, { confirm = null }) }
}

/** Settings → Profile picture (SDS §16.1), per mailbox. */
@Composable
fun ProfileSettings(state: InboxState, profile: ProfileStore, choosePhoto: () -> Unit, takePhoto: () -> Unit, all: Boolean, setAll: (Boolean) -> Unit) {
    val p = Pp.colors
    val mine by profile.state.collectAsState()
    LaunchedEffect(state.acting?.address) { state.acting?.address?.let(profile::load) }
    val acting = state.acting ?: return
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Avatar(acting.key, acting.name, 88.dp)
        Column { Text(acting.name, style = geistMono(15.sp, FontWeight.SemiBold), color = p.ink); mine.note?.let { Text(it, style = geist(12.5.sp), color = p.secondary) } }
    }
    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        Button(choosePhoto, enabled = !mine.saving, modifier = Modifier.testTag("choosePicture")) { Text("Choose photo") }
        OutlinedButton(takePhoto, enabled = !mine.saving) { Text("Take photo") }
    }
    Row(Modifier.fillMaxWidth().toggleable(all, role = Role.Checkbox, onValueChange = setAll), verticalAlignment = Alignment.CenterVertically) {
        Checkbox(all, null); Text("Use for all my mailboxes", Modifier.padding(start = 6.dp))
    }
    Text("Who can see it", style = geist(14.5.sp, FontWeight.Medium), color = p.ink)
    listOf("everyone" to "Everyone", "known" to "People I know", "nobody" to "Nobody").forEach { (value, label) ->
        Row(Modifier.fillMaxWidth().selectable(mine.visibility == value, role = Role.RadioButton) { profile.setVisibility(value) }.testTag("visibility:$value"),
            verticalAlignment = Alignment.CenterVertically) {
            RadioButton(mine.visibility == value, null); Text(label, Modifier.padding(start = 6.dp))
        }
    }
    Text("People I know are your contacts and the members of groups you share. Everyone means anyone who can look up this address.", style = geist(12.sp), color = p.muted)
    mine.error?.let { Text(it, color = p.blockedText, style = geist(13.sp)) }
    if (mine.avatar != null) TextButton(profile::remove, enabled = !mine.saving) { Text("Remove picture", color = p.blockedText) }
}

/** A photo, the right way up and no larger than 4096 px, ready for cropping. HEIC decodes on Android 9+. */
suspend fun decodeForCrop(context: Context, uri: Uri): Bitmap = withContext(Dispatchers.IO) {
    if (Build.VERSION.SDK_INT >= 28) {
        ImageDecoder.decodeBitmap(ImageDecoder.createSource(context.contentResolver, uri)) { decoder, info, _ ->
            require(info.size.width.toLong() * info.size.height <= 100L * 1024 * 1024) { "That picture is too large." }
            val scale = min(1.0, 4096.0 / max(info.size.width, info.size.height))
            decoder.setTargetSize(max(1, (info.size.width * scale).toInt()), max(1, (info.size.height * scale).toInt()))
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
        }
    } else {
        val resolver = context.contentResolver
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        resolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
        var sample = 1
        while (max(bounds.outWidth, bounds.outHeight) / sample > 4096) sample *= 2
        val raw = resolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, BitmapFactory.Options().apply { inSampleSize = sample }) }
            ?: error("That picture could not be opened.")
        val rotation = resolver.openInputStream(uri)?.use { ExifInterface(it).rotationDegrees } ?: 0
        if (rotation == 0) raw else Bitmap.createBitmap(raw, 0, 0, raw.width, raw.height, Matrix().apply { postRotate(rotation.toFloat()) }, true)
    }
}

/** The square chosen in the crop sheet, re-encoded here: JPEG, at most 1024 px, nothing of the original file. */
fun encodeCrop(source: Bitmap, left: Int, top: Int, side: Int): Pair<ByteArray, Int> {
    val s = side.coerceAtMost(min(source.width - left, source.height - top)).coerceAtLeast(1)
    val square = Bitmap.createBitmap(source, left.coerceAtLeast(0), top.coerceAtLeast(0), s, s)
    val out = if (s > 1024) Bitmap.createScaledBitmap(square, 1024, 1024, true) else square
    val bytes = ByteArrayOutputStream().also { out.compress(Bitmap.CompressFormat.JPEG, 90, it) }.toByteArray()
    return bytes to out.width
}

/** Square crop with zoom: drag to move, pinch or slide to zoom. The round mask is what people will see. */
@Composable
fun CropDialog(bitmap: Bitmap, saving: Boolean, error: String?, save: (left: Int, top: Int, side: Int) -> Unit, dismiss: () -> Unit) {
    val p = Pp.colors
    var view by remember { mutableStateOf(IntSize.Zero) }
    var zoom by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf<Offset?>(null) } // image top-left in view pixels
    val image = remember(bitmap) { bitmap.asImageBitmap() }
    fun base() = if (view.width == 0) 1f else view.width.toFloat() / min(bitmap.width, bitmap.height)
    fun clamp(o: Offset, z: Float): Offset {
        val s = base() * z
        return Offset(o.x.coerceIn(view.width - bitmap.width * s, 0f), o.y.coerceIn(view.height - bitmap.height * s, 0f))
    }
    fun centred(z: Float): Offset { val s = base() * z; return Offset((view.width - bitmap.width * s) / 2, (view.height - bitmap.height * s) / 2) }
    fun rezoom(to: Float) {
        val from = zoom; val next = to.coerceIn(1f, 4f)
        val o = offset ?: centred(from)
        // Keep the centre of the view on the same point of the photo.
        val cx = view.width / 2f; val cy = view.height / 2f
        val k = next / from
        offset = clamp(Offset(cx - (cx - o.x) * k, cy - (cy - o.y) * k), next); zoom = next
    }
    Dialog(dismiss, DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.padding(16.dp).widthIn(max = 480.dp).fillMaxWidth(), shape = MaterialTheme.shapes.extraLarge, color = p.ground) {
            Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                Text("Crop your picture", style = geist(19.sp, FontWeight.SemiBold), color = p.ink)
                Box(Modifier.fillMaxWidth().aspectRatio(1f).clip(RoundedCornerShape(8.dp)).background(Color.Black).onSizeChanged { view = it }
                    .pointerInput(bitmap) { detectTransformGestures { _, pan, gesture, _ ->
                        val o = offset ?: centred(zoom)
                        if (gesture != 1f) rezoom(zoom * gesture)
                        offset = clamp((offset ?: o) + pan, zoom)
                    } }.testTag("cropArea").semantics { contentDescription = "Picture to crop. Drag to move, pinch to zoom." }) {
                    if (view.width > 0) {
                        val o = offset ?: centred(zoom)
                        val s = base() * zoom
                        Canvas(Modifier.fillMaxSize()) {
                            drawImage(image, dstOffset = androidx.compose.ui.unit.IntOffset(o.x.roundToInt(), o.y.roundToInt()),
                                dstSize = IntSize((bitmap.width * s).roundToInt(), (bitmap.height * s).roundToInt()))
                            val circle = Path().apply { addOval(androidx.compose.ui.geometry.Rect(Offset.Zero, Size(size.width, size.height))) }
                            clipPath(circle, ClipOp.Difference) { drawRect(Color.Black.copy(alpha = .45f)) }
                        }
                    }
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Zoom", style = geist(13.sp), color = p.secondary)
                    Slider(zoom, { rezoom(it) }, Modifier.weight(1f).padding(start = 12.dp).testTag("cropZoom"), valueRange = 1f..4f)
                }
                error?.let { Text(it, color = p.blockedText, style = geist(13.sp)) }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(dismiss) { Text("Cancel") }
                    Button({
                        val o = offset ?: centred(zoom)
                        val s = base() * zoom
                        save((-o.x / s).roundToInt(), (-o.y / s).roundToInt(), (view.width / s).roundToInt())
                    }, enabled = !saving && view.width > 0, modifier = Modifier.testTag("savePicture")) { Text(if (saving) "Saving…" else "Save") }
                }
            }
        }
    }
}

/** Settings → Group encryption: this device's fingerprint, and the way to take it out of every group. */
@Composable
fun GroupDeviceSettings(groups: GroupsState, store: GroupStore) {
    val p = Pp.colors
    var confirm by remember { mutableStateOf(false) }
    Text("Group messages are end-to-end encrypted. This phone is one device of ${groups.mailbox?.let(::displayName) ?: "this mailbox"}, with its own keys.",
        style = geist(13.5.sp), color = p.secondary)
    Text("This device", style = geist(14.5.sp, FontWeight.Medium), color = p.ink)
    Text(groups.deviceId ?: "Not set up yet", style = geistMono(13.sp), color = p.body)
    Text("Fingerprint", style = geist(14.5.sp, FontWeight.Medium), color = p.ink)
    Text(groups.fingerprint?.chunked(5)?.joinToString(" ") ?: "—", style = geistMono(15.sp, FontWeight.Medium), color = p.ink, modifier = Modifier.testTag("deviceFingerprint"))
    Text("Others in your groups see this fingerprint when this device is added. Compare it with what they see if you want to be sure no one else added a device for you.",
        style = geist(12.sp), color = p.muted)
    if (!groups.persistent) Text("This device cannot keep group keys between launches.", color = p.heldText)
    HorizontalDivider()
    OutlinedButton({ confirm = true }, Modifier.fillMaxWidth(), enabled = groups.enabled && !groups.busy) { Text("Remove this device from my groups", color = p.blockedText) }
    if (confirm) Confirm("Remove this device?", "Its group keys and decrypted group history are deleted from this phone and the postbox stops accepting it. When you open groups again it joins as a new device, without earlier messages.",
        "Remove", { confirm = false; store.removeThisDevice() }, { confirm = false })
}
