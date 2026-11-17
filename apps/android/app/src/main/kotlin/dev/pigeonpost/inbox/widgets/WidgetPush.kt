package dev.pigeonpost.inbox.widgets

import android.content.Context
import dev.pigeonpost.core.widgets.WidgetSnapshots
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull

/**
 * A message push while the app is closed: the inbox widget counts it and moves that conversation
 * up, from the push's own locators only (mailbox, peer, message id, a group's name). Pushes carry no
 * message text, so nothing new can be shown; the app rebuilds the snapshot when it next runs.
 */
object WidgetPush {
    fun message(context: Context, identity: String, peer: String, groupTitle: String?, messageId: String) {
        runCatching {
            runBlocking(Dispatchers.IO) {
                withTimeoutOrNull(5_000) {
                    val now = System.currentTimeMillis() / 1000
                    val changed = WidgetStore.update(context) { current ->
                        current?.let { WidgetSnapshots.bump(it, identity, peer, groupTitle, messageId, now) ?: it }
                    }
                    if (changed) WidgetSync.updateWidgets(context)
                }
            }
        }
    }
}
