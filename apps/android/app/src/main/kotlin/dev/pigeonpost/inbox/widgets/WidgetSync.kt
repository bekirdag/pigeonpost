package dev.pigeonpost.inbox.widgets

import android.content.Context
import androidx.glance.appwidget.updateAll
import dev.pigeonpost.core.InboxState
import dev.pigeonpost.core.calendar.CalendarState
import dev.pigeonpost.core.contacts.PeopleState
import dev.pigeonpost.core.groups.GroupsState
import dev.pigeonpost.core.nests.NestState
import dev.pigeonpost.core.widgets.WidgetInput
import dev.pigeonpost.core.widgets.WidgetNest
import dev.pigeonpost.core.widgets.WidgetSnapshot
import dev.pigeonpost.core.widgets.WidgetSnapshots
import dev.pigeonpost.core.widgets.WidgetSourceAgenda
import dev.pigeonpost.core.widgets.WidgetSourceConversation
import dev.pigeonpost.core.widgets.WidgetSourceEvent
import dev.pigeonpost.core.widgets.WidgetSourceNests
import dev.pigeonpost.inbox.auth.SessionState
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.time.YearMonth

/** Everything the widgets are built from, as the app holds it right now. */
internal data class WidgetSources(
    val session: SessionState, val inbox: InboxState, val groups: GroupsState?, val nests: NestState?, val calendar: CalendarState?,
    val people: PeopleState?, val previews: Boolean,
)

/**
 * Keeps the widget snapshot in step with the app (docs/widgets-plan-android.md §4). It runs inside
 * the app's ViewModel: it reads state the app already loaded, writes the minimal snapshot, and asks
 * the widgets to redraw. Sign-out (or a session that is gone) clears it. Nothing here fetches.
 */
internal class WidgetSync(private val context: Context, private val scope: CoroutineScope) {
    /** Calendar refreshes the app made for an Agenda widget, by calendar context. */
    private var refreshedCalendar: String? = null

    fun start(sources: kotlinx.coroutines.flow.Flow<WidgetSources>, refreshCalendar: () -> Unit) {
        // Off the main thread: building, encrypting and writing the snapshot and redrawing widgets
        // ran on the ViewModel's Main dispatcher after every state change, which kept the UI thread
        // busy (and made Compose UI tests time out at random on slow emulators).
        scope.launch(Dispatchers.Default) {
            sources.collectLatest { now ->
                // Coalesce bursts (a refresh changes several stores in a row) into one write.
                delay(400)
                try { apply(now, refreshCalendar) }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (_: Exception) { /* A widget that cannot be written keeps its last snapshot. */ }
            }
        }
    }

    private suspend fun apply(sources: WidgetSources, refreshCalendar: () -> Unit) {
        val session = sources.session
        if (session.loading) return
        if (!session.signedIn || !session.termsAccepted) { clear(context); return }
        val inbox = sources.inbox
        val mailbox = inbox.acting?.address ?: return
        // Until the inbox has loaded, keep showing the last snapshot rather than an empty one.
        if (!inbox.loaded) return
        val calendar = sources.calendar?.takeIf { it.enabled && it.mailbox == mailbox }
        calendar?.let { state ->
            if (refreshedCalendar != state.contextId && agendaPlaced()) { refreshedCalendar = state.contextId; withContext(Dispatchers.Main) { refreshCalendar() } }
        }
        val input = input(sources, mailbox, calendar)
        val changed = WidgetStore.update(context) { previous ->
            WidgetSnapshots.build(input, previous, previous?.generatedAt?.takeIf { same(previous, input, it) } ?: nowSeconds())
        }
        if (changed) updateWidgets(context)
    }

    /** A rebuild that changes nothing keeps the old timestamp, so the file is not rewritten. */
    private fun same(previous: WidgetSnapshot, input: WidgetInput, at: Long) = WidgetSnapshots.build(input, previous, at) == previous

    private fun input(sources: WidgetSources, mailbox: String, calendar: CalendarState?): WidgetInput {
        val inbox = sources.inbox
        val people = sources.people
        val direct = inbox.conversations.filter { it.peer !in inbox.archived && !it.blocked && it.messages.isNotEmpty() }.map { row ->
            val fallback = row.name
            val name = people?.label(row.peer, row.identity?.name ?: row.contact?.alias, fallback) ?: fallback
            val last = row.messages.lastOrNull { !it.system }
            val text = last?.display?.text?.takeIf { it.isNotBlank() } ?: if (last?.attachments?.isNotEmpty() == true) "Attachment" else null
            WidgetSourceConversation(row.peer, name, row.last, row.unread, text)
        }
        val groups = sources.groups?.takeIf { it.enabled && !it.unavailable && it.mailbox == mailbox }?.groups.orEmpty()
            .filter { it.active }.map { g -> WidgetSourceConversation(g.id, g.name, g.lastAt ?: 0, g.my?.unread ?: 0, group = true) }
        val nestState = sources.nests
        val nestsEnabled = inbox.has(dev.pigeonpost.core.nests.NestRules.FEATURE) && nestState != null
        val nests = nestState?.snapshot?.let { snapshot ->
            val usage = nestState.detail?.let { it.view.nestId to it.usage?.runningSecs }
            val made = snapshot.nests.map { n -> WidgetNest(n.nestId, n.label, n.status, n.statusReason, usage?.takeIf { it.first == n.nestId }?.second) }
            val setting = snapshot.setups.filter { s -> made.none { it.label == s.label } }.map { s ->
                WidgetNest("setup:" + s.setupId, s.label, "unavailable", if (s.status == "failed") "failed" else "setting_up")
            }
            WidgetSourceNests(made + setting, snapshot.`package`?.activeHours)
        }
        // Loaded means a refresh finished (it always brings the inbox's calendars). The calendar holds
        // one month window; only the month that contains today feeds the agenda.
        val agenda = calendar?.takeIf { !it.loading && it.folders.isNotEmpty() && YearMonth.from(it.selectedDay.date) == YearMonth.now(it.zone) }?.let { state ->
            WidgetSourceAgenda(state.zone.id, state.occurrences.map { o ->
                WidgetSourceEvent(o.eventId, o.occurrenceId, o.title, o.startsAt, o.endsAt, o.allDay == true, o.startDate, o.endDate, o.location,
                    o.status, o.myRsvp, o.meeting != null)
            })
        }
        return WidgetInput(
            mailbox = mailbox, conversations = direct + groups, showPreviews = sources.previews,
            // Android Pigeonpost has no app lock today; when one is added, its state goes here.
            locked = false,
            nests = nests, nestsEnabled = nestsEnabled, agenda = agenda, calendarEnabled = calendar != null,
        )
    }

    private fun agendaPlaced() = WidgetPlacement.agenda(context)

    companion object {
        suspend fun clear(context: Context) {
            val had = WidgetStore.read(context) != null
            WidgetStore.clear(context)
            WidgetStore.resetSettings(context)
            if (had) updateWidgets(context)
        }

        /** Redraw placed widgets. With none on a home screen there is nothing to redraw (see [WidgetPlacement]). */
        suspend fun updateWidgets(context: Context) {
            if (!WidgetPlacement.any(context)) return
            for (widget in listOf(InboxWidget(), QuickActionsWidget(), NestsWidget(), AgendaWidget())) runCatching { widget.updateAll(context) }
        }
    }
}

/** The flows [WidgetSync] reads, combined. Stores a build does not have are passed as null. */
internal fun widgetSources(
    session: StateFlow<SessionState>, inbox: StateFlow<InboxState>, groups: StateFlow<GroupsState>?, nests: StateFlow<NestState>?,
    calendar: StateFlow<CalendarState>?, people: StateFlow<PeopleState>?, previews: StateFlow<Boolean?>,
): kotlinx.coroutines.flow.Flow<WidgetSources> {
    val none = MutableStateFlow<Any?>(null)
    @Suppress("UNCHECKED_CAST")
    val parts = listOf(session, inbox, groups ?: none, nests ?: none, calendar ?: none, people ?: none, previews) as List<kotlinx.coroutines.flow.Flow<Any?>>
    return combine(parts) { values ->
        WidgetSources(values[0] as SessionState, values[1] as InboxState, values[2] as GroupsState?, values[3] as NestState?,
            values[4] as CalendarState?, values[5] as PeopleState?, values[6] as Boolean? ?: false)
    }
}
