@file:OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class, androidx.compose.material3.ExperimentalMaterial3Api::class)
package dev.pigeonpost.inbox.ui

import android.net.Uri
import androidx.compose.foundation.content.ReceiveContentListener
import androidx.compose.foundation.content.consume
import androidx.compose.foundation.content.contentReceiver
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.sp
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import kotlinx.coroutines.flow.distinctUntilChanged

@Composable
fun MessageEditor(text: String, changed: (String) -> Unit, enabled: Boolean, modifier: Modifier,
    attach: (List<Uri>, Any) -> Unit, focus: FocusRequester = remember { FocusRequester() }, placeholder: String = "Message") {
    val field = rememberTextFieldState(text)
    val currentChanged by rememberUpdatedState(changed)
    val currentAttach by rememberUpdatedState(attach)
    val currentEnabled by rememberUpdatedState(enabled)
    LaunchedEffect(text) { if (field.text.toString() != text) field.setTextAndPlaceCursorAtEnd(text) }
    LaunchedEffect(field) { snapshotFlow { field.text.toString() }.distinctUntilChanged().collect { currentChanged(it) } }
    val receiver = remember {
        ReceiveContentListener { content ->
            if (!currentEnabled) content else {
                val uris = mutableListOf<Uri>()
                val remaining = content.consume { item ->
                    val uri = item.uri
                    if (uri?.scheme == "content") { uris += uri; true } else false
                }
                if (uris.isNotEmpty()) currentAttach(uris, content)
                remaining
            }
        }
    }
    // Read-only rather than disabled while a message is sending: a disabled field drops focus and the
    // keyboard, and the next message should be typed straight after Send.
    val p = Pp.colors
    TextField(state = field, modifier = modifier.focusRequester(focus).contentReceiver(receiver),
        placeholder = { Text(placeholder, style = geist(15.sp), color = p.muted) },
        readOnly = !enabled, textStyle = geist(15.sp, color = p.ink), lineLimits = TextFieldLineLimits.MultiLine(maxHeightInLines = 6),
        colors = TextFieldDefaults.colors(focusedContainerColor = Color.Transparent, unfocusedContainerColor = Color.Transparent,
            disabledContainerColor = Color.Transparent, focusedIndicatorColor = Color.Transparent, unfocusedIndicatorColor = Color.Transparent,
            disabledIndicatorColor = Color.Transparent, cursorColor = p.accent))
}
