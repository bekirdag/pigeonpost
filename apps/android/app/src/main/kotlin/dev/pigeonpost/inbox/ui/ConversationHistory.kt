package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ArrowDownward
import androidx.compose.material3.Icon
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsPropertyKey
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.pigeonpost.core.HistoryWindow
import dev.pigeonpost.core.ThreadMessage

internal val LoadedMessageCount = SemanticsPropertyKey<Int>("LoadedMessageCount")

private class HistoryViewport(window: HistoryWindow, val list: LazyListState = LazyListState()) {
    val windowState = mutableStateOf(window)

    companion object {
        // Save the window and its viewport together, using four values even for very long histories.
        fun saver(currentIds: () -> List<String>) = listSaver<HistoryViewport, Any>(
            save = {
                val ids = it.windowState.value.ids
                listOf(ids.firstOrNull().orEmpty(), ids.lastOrNull().orEmpty(),
                    ids.getOrNull(it.list.firstVisibleItemIndex).orEmpty(), it.list.firstVisibleItemScrollOffset)
            },
            restore = {
                val ids = currentIds()
                val newest = ids.indexOf(it[0])
                val oldest = ids.indexOf(it[1])
                val window = if (oldest >= 0 && newest >= oldest)
                    HistoryWindow(ids.subList(oldest, newest + 1).asReversed()) else HistoryWindow().latest(ids)
                val anchor = window.ids.indexOf(it[2])
                if (anchor >= 0) HistoryViewport(window, LazyListState(anchor, it[3] as Int))
                else HistoryViewport(HistoryWindow().latest(ids))
            },
        )
    }
}

private data class HistoryScrollRequest(val id: String, val sequence: Int)

/** Item zero is the bottom origin, including when its content is taller than the viewport. */
@Composable
internal fun ConversationHistory(
    messages: List<ThreadMessage>,
    modifier: Modifier = Modifier,
    searchTarget: String? = null,
    latestRequest: Int = 0,
    messageContent: @Composable (ThreadMessage) -> Unit,
) {
    val ids = remember(messages) { messages.map { it.id } }
    val byId = remember(messages) { messages.associateBy { it.id } }
    val currentIds by rememberUpdatedState(ids)
    val viewport = rememberSaveable(saver = HistoryViewport.saver { currentIds }) { HistoryViewport(HistoryWindow().latest(ids)) }
    var window by viewport.windowState
    val list = viewport.list
    var scrollRequest by remember { mutableStateOf<HistoryScrollRequest?>(null) }
    fun requestPosition(id: String?) {
        if (id != null) scrollRequest = HistoryScrollRequest(id, (scrollRequest?.sequence ?: 0) + 1)
    }
    val atBottom by remember { derivedStateOf { list.firstVisibleItemIndex == 0 && list.firstVisibleItemScrollOffset == 0 } }

    LaunchedEffect(ids) {
        // Keep arrivals outside the reading window. Stable item keys preserve the visible anchor.
        val follow = atBottom && !list.isScrollInProgress && searchTarget == null
        window = window.update(ids, follow)
        if (follow) requestPosition(window.ids.firstOrNull())
    }
    LaunchedEffect(list, ids) {
        snapshotFlow {
            list.isScrollInProgress && window.ids.isNotEmpty() &&
                (list.layoutInfo.visibleItemsInfo.maxOfOrNull { it.index } ?: -1) >= window.ids.size - 3
        }.collect { nearOlderEdge -> if (nearOlderEdge) window = window.older(ids) }
    }
    LaunchedEffect(searchTarget) {
        if (searchTarget != null) {
            window = window.reveal(ids, searchTarget)
            requestPosition(searchTarget)
        }
    }
    LaunchedEffect(latestRequest) {
        if (latestRequest > 0) {
            window = window.latest(ids)
            requestPosition(window.ids.firstOrNull())
        }
    }
    val visible = window.ids.mapNotNull(byId::get)
    LaunchedEffect(scrollRequest) {
        val index = visible.indexOfFirst { it.id == scrollRequest?.id }
        // Apply after the new item provider is composed. An immediate request can be consumed by
        // the old provider, after which stable keys incorrectly restore the previous message.
        if (index >= 0) list.scrollToItem(index)
    }
    Box(modifier) {
        LazyColumn(
            state = list,
            reverseLayout = true,
            modifier = Modifier.fillMaxSize().testTag("conversation-history").semantics { this[LoadedMessageCount] = visible.size },
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp, Alignment.Bottom),
        ) {
            items(visible, key = { it.id }) { message ->
                Box(Modifier.fillMaxWidth().testTag("message:" + message.id)) { messageContent(message) }
            }
        }
        if (visible.isNotEmpty() && (!atBottom || window.ids.firstOrNull() != ids.lastOrNull())) {
            SmallFloatingActionButton({
                window = window.latest(ids)
                requestPosition(window.ids.firstOrNull())
            }, Modifier.align(Alignment.BottomEnd).padding(12.dp)) {
                Icon(Icons.Outlined.ArrowDownward, "Latest messages")
            }
        }
    }
}
