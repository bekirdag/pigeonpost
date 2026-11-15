package dev.pigeonpost.inbox.widgets

import android.content.Intent
import androidx.glance.action.Action
import androidx.glance.action.ActionParameters
import androidx.glance.action.actionParametersOf
import androidx.glance.appwidget.action.actionStartActivity
import dev.pigeonpost.core.widgets.WidgetEvent
import dev.pigeonpost.inbox.BuildConfig
import dev.pigeonpost.inbox.MainActivity

/**
 * Where a widget tap goes. A tap only asks the app to navigate: the app checks the mailbox belongs
 * to the signed-in account and rereads everything, exactly as for a notification tap
 * (InboxViewModel.openNotification / openCalendarNotice). Conversations and events reuse the
 * notification extras, so there is one validated path for each.
 */
object WidgetTargets {
    const val EXTRA = "widget_target"
    const val INBOX = "inbox"
    const val COMPOSE = "compose"
    const val NESTS = "nests"
    const val NEST = "nest"
    const val EXTRA_NEST = "widget_nest"

    private val target = ActionParameters.Key<String>(EXTRA)
    private val nest = ActionParameters.Key<String>(EXTRA_NEST)
    private val identity = ActionParameters.Key<String>("push_identity")
    private val peer = ActionParameters.Key<String>("push_peer")
    private val type = ActionParameters.Key<String>("push_type")
    private val event = ActionParameters.Key<String>("calendar_event_id")
    private val occurrence = ActionParameters.Key<String>("calendar_occurrence_id")

    /** One activity instance: a tap brings the open app forward instead of stacking a second one. */
    private fun open(parameters: ActionParameters): Action = actionStartActivity(Intent().apply {
        setClassName(BuildConfig.APPLICATION_ID, MainActivity::class.java.name)
        addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    }, parameters)

    fun app(): Action = open(actionParametersOf())
    fun inbox(): Action = open(actionParametersOf(target to INBOX))
    fun compose(): Action = open(actionParametersOf(target to COMPOSE))
    fun conversation(mailbox: String, peer: String): Action = open(actionParametersOf(identity to mailbox, this.peer to peer))
    fun calendar(mailbox: String): Action = open(actionParametersOf(type to "calendar", identity to mailbox))
    fun event(mailbox: String, value: WidgetEvent): Action =
        open(actionParametersOf(type to "calendar", identity to mailbox, event to value.eventId, occurrence to value.occurrenceId))
    fun nests(): Action = open(actionParametersOf(target to NESTS))
    fun nest(id: String): Action = open(actionParametersOf(target to NEST, nest to id))
}
