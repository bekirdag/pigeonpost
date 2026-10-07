@file:OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class, androidx.compose.material3.ExperimentalMaterial3Api::class)
package dev.pigeonpost.inbox.ui

import android.net.Uri
import androidx.compose.foundation.content.ReceiveContentListener
import androidx.compose.foundation.content.consume
import androidx.compose.foundation.content.contentReceiver
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.distinctUntilChanged

@Composable
fun MessageEditor(text: String, changed: (String) -> Unit, enabled: Boolean, modifier: Modifier,
    attach: (List<Uri>, Any) -> Unit) {
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
    OutlinedTextField(state = field, modifier = modifier.contentReceiver(receiver), placeholder = { Text("Message") },
        enabled = enabled, shape = RoundedCornerShape(20.dp), lineLimits = TextFieldLineLimits.MultiLine(maxHeightInLines = 5))
}
