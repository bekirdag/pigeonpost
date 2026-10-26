package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.SessionExpired
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.util.UUID

data class CalendarMeetingLookupState(
    val contextId: String = UUID.randomUUID().toString(), val mailbox: String? = null, val code: String? = null,
    val information: CalendarMeetingStatus? = null, val loading: Boolean = false, val error: String? = null,
)

/** One canonical member and accepted login. Lookup never joins, responds or changes a contact. */
class CalendarMeetingLookup(private val api: CalendarMeetingApi, private val scope: CoroutineScope, private val onSessionExpired: () -> Unit = {}) {
    private val mutable = MutableStateFlow(CalendarMeetingLookupState())
    val state = mutable.asStateFlow()
    private var actor: AuthorizationLease? = null
    private var readLease: AuthorizationLease? = null
    private var job: Job? = null
    private var request = UUID.randomUUID()
    private fun current(lease: AuthorizationLease) = actor === lease && lease.isValid && state.value.contextId == lease.id
    fun reset() {
        actor?.retire(); actor = null; readLease?.retire(); readLease = null; job?.cancel(); job = null; request = UUID.randomUUID()
        mutable.value = CalendarMeetingLookupState()
    }
    fun onMailbox(mailbox: String?) {
        val parent = scope.coroutineContext[AuthorizationContext]?.lease
        val old = actor
        if (mailbox != null && old != null && parent != null && current(old) && old.sharesAuthorization(parent) && state.value.mailbox == mailbox) return
        reset()
        if (mailbox == null || !Regex("/k/[A-Za-z0-9_]{1,157}").matches(mailbox) || parent?.isValid != true) return
        val lease = try { parent.child() } catch (_: AuthorizationRetired) { return }
        actor = lease; mutable.value = CalendarMeetingLookupState(contextId = lease.id, mailbox = mailbox)
    }
    /** Closing the picker retires its reads; an explicitly handed-off join keeps the actor lease. */
    fun close() {
        request = UUID.randomUUID(); readLease?.retire(); readLease = null; job?.cancel(); job = null
        mutable.update { it.copy(code = null, information = null, loading = false, error = null) }
    }
    fun load(input: String, expectedContext: String): Job? {
        val lease = actor ?: return null
        val mailbox = state.value.mailbox ?: return null
        if (!current(lease) || expectedContext != state.value.contextId) return null
        val code = CalendarMeetingCode.parse(input) ?: return null
        readLease?.retire(); job?.cancel(); val visit = UUID.randomUUID(); request = visit
        val read = try { lease.child() } catch (_: AuthorizationRetired) { return null }
        readLease = read
        mutable.update { it.copy(code = code, information = null, loading = true, error = null) }
        return scope.launch(AuthorizationContext(read)) {
            try {
                read.requireActive()
                val result = api.scheduledMeeting(mailbox, code)
                if (current(lease) && visit == request) read.withActive { mutable.update { it.copy(information = result) } }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) {
                if (current(lease) && visit == request) {
                    if (failure is SessionExpired) { reset(); onSessionExpired() }
                    else mutable.update { it.copy(error = "The meeting could not be loaded. Try again.") }
                }
            } finally { read.retire(); if (current(lease) && visit == request) mutable.update { it.copy(loading = false) } }
        }.also { job = it }
    }
    fun prepareJoin(occurrence: String, video: Boolean, expectedContext: String, expectedCode: String, done: (CalendarScheduledJoin) -> Unit): Boolean {
        val lease = actor ?: return false
        val snapshot = state.value
        if (!current(lease) || snapshot.contextId != expectedContext || snapshot.code != expectedCode ||
            snapshot.information?.choices?.any { it.occurrenceId == occurrence } != true) return false
        return try {
            lease.withActive {
                val join = CalendarScheduledJoin(snapshot.mailbox!!, expectedCode, occurrence, "Meeting " + expectedCode.chunked(5).joinToString("-"), video, lease)
                done(join); true
            }
        } catch (_: AuthorizationRetired) { false }
    }
}
