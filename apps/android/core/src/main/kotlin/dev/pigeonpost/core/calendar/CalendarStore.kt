package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.SessionExpired
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*
import java.time.Instant
import java.io.IOException
import java.util.UUID

data class CalendarState(
    val contextId: String = UUID.randomUUID().toString(),
    val mailbox: String? = null,
    val enabled: Boolean = false,
    val emailEnabled: Boolean = false,
    val folders: List<CalendarFolder> = emptyList(),
    val occurrences: List<EventOccurrence> = emptyList(),
    val invitations: List<CalendarInvitation> = emptyList(),
    val filteredInvitations: List<CalendarInvitation> = emptyList(),
    val settings: CalendarSettings = CalendarSettings(),
    val selectedDay: CalendarDay = CalendarDay.at(Instant.now(), CalendarTime.displayZone(null)),
    /** null = all, empty = none. Never send an empty selection as the API's implicit all. */
    val selectedFolders: Set<String>? = null,
    val details: Map<CalendarEventKey, CalendarEventResponse> = emptyMap(),
    val openEvent: CalendarEventKey? = null,
    val loading: Boolean = false,
    val busy: Set<String> = emptySet(),
    val error: String? = null,
    val revisionConflict: Boolean = false,
    val pending: List<CalendarPendingAttempt> = emptyList(),
    val attemptsLoaded: Boolean = false,
    val attemptsUnreadable: Boolean = false,
    val durableWrites: Boolean = true,
) {
    val zone get() = CalendarTime.displayZone(settings.timezone)
    val weekStart get() = settings.weekStart?.takeIf { it in 0..6 } ?: 1
    val agenda get() = occurrences.filter { it.occursOn(selectedDay, zone) }.sortedWith(compareByDescending<EventOccurrence> { it.allDay == true }.thenBy { it.startsAt ?: Long.MIN_VALUE })
    val canCreate get() = enabled && attemptsLoaded && !attemptsUnreadable && durableWrites
}
data class CalendarCreated(val kind: CalendarAttemptKind, val eventId: String? = null, val calendarId: String? = null, val imported: Int = 0)

/** UI-scope owner. Every task captures its canonical mailbox, child lease and request generation. */
class CalendarStore(
    private val transport: CalendarTransport,
    private val storage: CalendarAttemptStorage,
    private val scope: CoroutineScope,
    private val now: () -> Instant = { Instant.now() },
    private val allowVolatileStorage: Boolean = false,
    private val pollSeconds: Long = 20,
    private val onSessionExpired: () -> Unit = {},
) {
    private fun empty() = CalendarState(selectedDay = CalendarDay.at(now(), CalendarTime.displayZone(null)), durableWrites = storage.persistent || allowVolatileStorage)
    private val mutable = MutableStateFlow(empty())
    val state = mutable.asStateFlow()
    private var client: CalendarClient? = null
    private var refreshId = UUID.randomUUID()
    private var attemptReadId = UUID.randomUUID()
    private val detailReads = mutableMapOf<CalendarEventKey, UUID>()
    private val eventVersions = mutableMapOf<String, UUID>()
    private val jobs = mutableSetOf<Job>()
    private var mutation = Mutex()
    private var poll: Job? = null
    private var open = false
    private var active = false
    private var followingToday = true

    private fun current(c: CalendarClient) = client === c && c.authorization.isValid && state.value.contextId == c.authorization.id
    private fun update(c: CalendarClient, work: (CalendarState) -> CalendarState) {
        if (current(c)) c.authorization.withActive { if (current(c)) mutable.update(work) }
    }
    private fun launch(c: CalendarClient, work: suspend CoroutineScope.() -> Unit): Job = scope.launch(AuthorizationContext(c.authorization)) {
        c.authorization.requireActive(); work()
    }.also { job -> jobs += job; job.invokeOnCompletion { jobs.remove(job) } }
    fun reset() {
        client?.close(); client = null
        jobs.toList().forEach { it.cancel() }; jobs.clear(); poll?.cancel(); poll = null
        // A retired network task may ignore cancellation. It never owns the new actor's queue.
        mutation = Mutex()
        refreshId = UUID.randomUUID(); attemptReadId = UUID.randomUUID(); detailReads.clear(); eventVersions.clear(); followingToday = true
        mutable.value = empty()
    }
    fun onMailbox(mailbox: String?, features: Set<String>) {
        val parent = scope.coroutineContext[AuthorizationContext]?.lease
        val enabled = mailbox != null && "calendar" in features && parent?.isValid == true
        val old = client
        if (enabled && old != null && current(old) && old.mailbox == mailbox && old.authorization.sharesAuthorization(parent!!)) {
            update(old) { it.copy(emailEnabled = "calendar_email" in features) }; return
        }
        reset()
        if (!enabled) return
        val child = try { parent!!.child() } catch (_: AuthorizationRetired) { return }
        val c = CalendarClient(transport, mailbox!!, child)
        client = c
        mutable.value = empty().copy(contextId = c.authorization.id, mailbox = mailbox, enabled = true, emailEnabled = "calendar_email" in features)
        loadAttempts()
        if (open) refresh()
        startPolling()
    }
    fun setOpen(value: Boolean) { open = value; if (value) refresh(); startPolling() }
    fun setActive(value: Boolean) { active = value; startPolling() }
    private fun startPolling() {
        poll?.cancel(); poll = null
        val c = client ?: return
        if (!open || !active) return
        poll = launch(c) {
            while (isActive && current(c) && open && active) { delay(pollSeconds * 1000); if (current(c)) refresh() }
        }
    }
    fun clearError() { mutable.update { it.copy(error = null, revisionConflict = false) } }
    fun select(day: CalendarDay) { followingToday = false; mutable.update { it.copy(selectedDay = day) }; refresh() }
    fun today() { followingToday = true; mutable.update { it.copy(selectedDay = CalendarDay.at(now(), it.zone)) }; refresh() }
    fun selectFolders(ids: Set<String>?) { mutable.update { it.copy(selectedFolders = ids) }; refresh() }
    fun closeEvent() { mutable.update { it.copy(openEvent = null) } }
    fun openEvent(key: CalendarEventKey) { mutable.update { it.copy(openEvent = key) }; loadDetail(key) }

    private data class Metadata(val folders: List<CalendarFolder>, val settings: CalendarSettings, val invitations: List<CalendarInvitation>, val filtered: List<CalendarInvitation>)
    fun refresh(): Job? {
        val c = client ?: return null
        val request = UUID.randomUUID(); refreshId = request
        update(c) { it.copy(loading = true) }
        return launch(c) {
            try {
                val metadata = coroutineScope {
                    val folders = async { c.calendars() }; val settings = async { c.settings() }
                    val invitations = async { c.invitations(false) }; val filtered = async { c.invitations(true) }
                    Metadata(folders.await(), settings.await(), invitations.await(), filtered.await())
                }
                if (!current(c) || request != refreshId) return@launch
                val (folders, settings, invitations, filtered) = metadata
                val selected = state.value.selectedFolders?.intersect(folders.map { it.id }.toSet())
                val day = if (followingToday) CalendarDay.at(now(), CalendarTime.displayZone(settings.timezone)) else state.value.selectedDay
                val window = CalendarMonthWindow(day, settings.weekStart?.takeIf { it in 0..6 } ?: 1).interval(CalendarTime.displayZone(settings.timezone))
                val rows = if (selected?.isEmpty() == true) emptyList() else c.occurrences(window.start, window.end, selected?.toList().orEmpty())
                if (!current(c) || request != refreshId) return@launch
                update(c) { it.copy(folders = folders, settings = settings, invitations = invitations, filteredInvitations = filtered,
                    selectedFolders = selected, selectedDay = day, occurrences = rows) }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { if (current(c) && request == refreshId) failed(c, failure) }
            finally { if (request == refreshId) update(c) { it.copy(loading = false) } }
        }
    }
    fun loadDetail(key: CalendarEventKey): Job? {
        val c = client ?: return null
        val request = UUID.randomUUID(); detailReads[key] = request
        val version = eventVersions.getOrPut(key.eventId) { UUID.randomUUID() }
        return launch(c) {
            try {
                val response = c.detail(key)
                if (current(c) && detailReads[key] == request && eventVersions[key.eventId] == version)
                    update(c) { it.copy(details = it.details + (key to response)) }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: ApiException) {
                if (current(c) && detailReads[key] == request) {
                    if (failure.status in setOf(403, 404)) forget(c, key.eventId)
                    failed(c, failure)
                }
            } catch (failure: Exception) { if (current(c) && detailReads[key] == request) failed(c, failure) }
        }
    }
    private fun forget(c: CalendarClient, id: String, close: Boolean = true) {
        refreshId = UUID.randomUUID()
        eventVersions[id] = UUID.randomUUID()
        detailReads.keys.filter { it.eventId == id }.toList().forEach(detailReads::remove)
        update(c) { it.copy(loading = false, details = it.details.filterKeys { key -> key.eventId != id }, occurrences = it.occurrences.filterNot { row -> row.eventId == id },
            invitations = it.invitations.filterNot { row -> row.eventId == id }, filteredInvitations = it.filteredInvitations.filterNot { row -> row.eventId == id },
            openEvent = it.openEvent?.takeUnless { key -> close && key.eventId == id }) }
    }
    fun loadAttempts(): Job? {
        val c = client ?: return null
        val request = UUID.randomUUID(); attemptReadId = request
        val serial = mutation
        update(c) { it.copy(attemptsLoaded = false) }
        return launch(c) {
            serial.withLock {
                if (!current(c) || request != attemptReadId) return@withLock
                try {
                    val records = storage.load(c.mailbox, c.authorization)
                    CalendarAttempts.validate(c.mailbox, records)
                    if (request == attemptReadId) update(c) { it.copy(pending = records, attemptsLoaded = true, attemptsUnreadable = false) }
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (_: Exception) { if (request == attemptReadId) update(c) { it.copy(attemptsLoaded = false, attemptsUnreadable = true, error = CalendarAttemptStorageException().message) } }
            }
        }
    }
    private suspend fun saveAttempts(c: CalendarClient, records: List<CalendarPendingAttempt>) {
        withContext(NonCancellable) { storage.save(c.mailbox, records, c.authorization) }
        c.authorization.requireActive()
        update(c) { it.copy(pending = records.toList()) }
    }
    private fun mutate(operation: String, expectedContext: String, work: suspend (CalendarClient) -> Unit): Job? {
        val c = client ?: return null
        if (!current(c) || state.value.contextId != expectedContext || state.value.busy.isNotEmpty()) return null
        val serial = mutation
        update(c) { it.copy(busy = it.busy + operation, error = null, revisionConflict = false) }
        return launch(c) {
            serial.withLock {
                try { if (current(c)) work(c) }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (failure: Exception) { if (current(c)) failed(c, failure) }
                finally { update(c) { it.copy(busy = it.busy - operation) } }
            }
        }
    }
    private fun failed(c: CalendarClient, failure: Exception) {
        if (failure is SessionExpired) { reset(); onSessionExpired(); return }
        val api = failure as? ApiException
        if (api?.code == "feature_disabled") { reset(); return }
        val message = when (api?.code) {
            "revision_conflict" -> "This event changed while you were editing. Review the latest version before saving again."
            "idempotency_conflict" -> "An earlier version may already be saved. Check the calendar before creating another."
            "calendar_unavailable" -> "This calendar is no longer available."
            "human_required" -> "Sign in yourself with a verified account to invite contact-address guests."
            "external_unavailable" -> "Invitation delivery is unavailable. You can copy a personal guest link."
            "not_found", "forbidden" -> "This event is no longer available to this inbox."
            else -> if (failure is IOException && failure !is ApiException && failure !is CalendarAttemptStorageException)
                "Could not reach the calendar. Try again when connected." else failure.message ?: "The calendar could not be reached."
        }
        update(c) { it.copy(error = message, revisionConflict = api?.code == "revision_conflict") }
    }
    private fun create(kind: CalendarAttemptKind, fields: JsonObject, expectedContext: String, target: String? = null, done: (CalendarCreated) -> Unit): Job? = mutate("create:${kind.name}", expectedContext) { c ->
        check(state.value.canCreate) { "Wait for your saved changes to load before creating another item." }
        check(state.value.pending.none { it.kind == kind }) { "An earlier change is unconfirmed. Review it and retry the original before creating another." }
        val id = UUID.randomUUID().toString()
        val payload = CalendarWritePayload(JsonObject(fields.filterKeys { it != "request_key" } + if (kind == CalendarAttemptKind.IMPORT) emptyMap() else mapOf("request_key" to JsonPrimitive(id))))
        val attempt = CalendarPendingAttempt(id, c.mailbox, kind, payload.text, now().epochSecond, target)
        saveAttempts(c, state.value.pending + attempt)
        execute(c, attempt, fresh = true)?.let { if (current(c)) done(it) }
    }
    private suspend fun execute(c: CalendarClient, attempt: CalendarPendingAttempt, fresh: Boolean): CalendarCreated? {
        check(attempt.canRetry(now().epochSecond)) { "This unfinished change is too old to retry safely. Inspect the calendar before discarding it." }
        val result = try {
            when (attempt.kind) {
                CalendarAttemptKind.EVENT -> CalendarCreated(attempt.kind, eventId = c.createEvent(attempt.payload()).event.id)
                CalendarAttemptKind.CALENDAR -> CalendarCreated(attempt.kind, calendarId = c.createCalendar(attempt.payload()).id)
                CalendarAttemptKind.IMPORT -> CalendarCreated(attempt.kind, imported = c.confirmImport(attempt.target!!, attempt.payload()).created.orEmpty().size)
            }
        } catch (failure: ApiException) {
            // A failed replay may mean continuing access was lost after an earlier accepted write.
            if (fresh && failure.status in 400..499 && failure.status !in setOf(408, 409, 429))
                saveAttempts(c, state.value.pending.filterNot { it.id == attempt.id })
            throw failure
        }
        saveAttempts(c, state.value.pending.filterNot { it.id == attempt.id })
        refresh() // A replay receipt is historical; fetch current data instead of caching its old snapshot.
        return result
    }
    fun createEvent(fields: JsonObject, expectedContext: String, done: (CalendarEventKey) -> Unit = {}) = create(CalendarAttemptKind.EVENT, fields, expectedContext) { done(CalendarEventKey(it.eventId!!)) }
    fun createCalendar(name: String, color: String, expectedContext: String, done: (String) -> Unit = {}) = create(CalendarAttemptKind.CALENDAR, buildJsonObject { put("name", name); put("color", color) }, expectedContext) { done(it.calendarId!!) }
    fun confirmImport(draft: String, selected: List<Int>, calendar: String?, expectedContext: String, done: (Int) -> Unit = {}): Job? {
        require(selected.size <= 200 && selected.all { it >= 0 }) { "Choose at most 200 import entries." }
        return create(CalendarAttemptKind.IMPORT, buildJsonObject {
            put("select", JsonArray(selected.distinct().sorted().map(::JsonPrimitive))); calendar?.let { put("calendar_id", it) }
        }, expectedContext, target = draft) { done(it.imported) }
    }
    fun retry(id: String, expectedContext: String, done: (CalendarCreated) -> Unit = {}) = mutate("retry", expectedContext) { c ->
        check(state.value.attemptsLoaded && !state.value.attemptsUnreadable)
        val attempt = state.value.pending.singleOrNull { it.id == id } ?: return@mutate
        execute(c, attempt, fresh = false)?.let { if (current(c)) done(it) }
    }
    fun discard(id: String, expectedContext: String) = mutate("discard", expectedContext) { c ->
        check(state.value.attemptsLoaded && !state.value.attemptsUnreadable)
        saveAttempts(c, state.value.pending.filterNot { it.id == id })
    }
    fun edit(key: CalendarEventKey, payload: CalendarWritePayload, expectedContext: String, done: (CalendarEventKey) -> Unit = {}) = mutate("edit", expectedContext) { c ->
        val result = c.updateEvent(key.eventId, payload)
        forget(c, key.eventId); refresh()
        val selected = CalendarEventKey(result.event.id, key.occurrenceId?.takeIf { result.event.id == key.eventId })
        if (current(c)) { loadDetail(selected); done(selected) }
    }
    fun cancel(key: CalendarEventKey, revision: Long, scope: CalendarEventScope, expectedContext: String) = mutate("cancel", expectedContext) { c ->
        c.cancelEvent(key, revision, scope); forget(c, key.eventId); refresh()
    }
    fun rsvp(key: CalendarEventKey, answer: CalendarAnswer, scope: CalendarAnswerScope, expectedContext: String, note: String? = null) = mutate("rsvp", expectedContext) { c ->
        c.rsvp(key, answer, scope, note); changed(c, key)
    }
    fun reminders(id: String, values: List<CalendarReminder>, expectedContext: String) = mutate("reminders", expectedContext) { c ->
        c.reminders(id, values); changed(c, state.value.openEvent?.takeIf { it.eventId == id } ?: CalendarEventKey(id))
    }
    fun saveSettings(fields: JsonObject, expectedContext: String) = mutate("settings", expectedContext) { c ->
        val settings = c.saveSettings(fields); update(c) { it.copy(settings = settings) }; refresh()
    }
    private fun changed(c: CalendarClient, key: CalendarEventKey) { forget(c, key.eventId, close = false); refresh(); loadDetail(key) }
    fun updateCalendar(id: String, revision: Long, fields: JsonObject, expectedContext: String) = mutate("calendar", expectedContext) { c ->
        c.updateCalendar(id, revision, fields); refresh()
    }
    fun deleteCalendar(id: String, revision: Long, expectedContext: String) = mutate("calendar", expectedContext) { c ->
        c.deleteCalendar(id, revision)
        val ids = state.value.occurrences.filter { it.calendarId == id }.map { it.eventId }.distinct()
        ids.forEach { forget(c, it) }; refresh()
    }
    fun attendees(key: CalendarEventKey, revision: Long, answerScope: CalendarAnswerScope, adding: List<JsonObject>, removing: List<String>, expectedContext: String) = mutate("attendees", expectedContext) { c ->
        c.attendees(key, revision, answerScope, adding, removing); changed(c, key)
    }
    fun linkGroup(key: CalendarEventKey, revision: Long, groupId: String?, expectedContext: String) = mutate("linked-group", expectedContext) { c ->
        c.linkGroup(key.eventId, revision, groupId); changed(c, key)
    }
    fun moveInvitation(key: CalendarEventKey, calendarId: String, expectedContext: String) = mutate("placement", expectedContext) { c ->
        c.moveInvitation(key.eventId, calendarId); changed(c, key)
    }
    /** Capability-bearing results live only in the current presentation, never CalendarState. */
    private fun <T> inspect(operation: String, expectedContext: String, request: suspend (CalendarClient) -> T, done: (T) -> Unit) =
        mutate(operation, expectedContext) { c -> val value = request(c); if (current(c)) done(value) }
    fun guestLink(id: String, attendee: String, expectedContext: String, done: (CalendarGuestLink) -> Unit) = inspect("guest-link", expectedContext, { it.guestLink(id, attendee) }, done)
    fun feed(id: String, expectedContext: String, done: (CalendarFeed) -> Unit) = inspect("feed", expectedContext, { it.feed(id) }, done)
    fun setFeed(id: String, enabled: Boolean, projection: String, expectedContext: String, done: (CalendarFeed) -> Unit) = inspect("feed", expectedContext, { it.setFeed(id, enabled, projection) }, done)
    fun ics(id: String, expectedContext: String, done: (ByteArray) -> Unit) = inspect("export", expectedContext, { it.ics(id) }, done)
    fun prepareIcsExport(id: String, expectedContext: String, done: (CalendarIcsExport) -> Unit) =
        inspect("export", expectedContext, { c -> CalendarIcsExport(c.ics(id), c.authorization) }, done)
    fun previewImport(bytes: ByteArray, calendar: String?, expectedContext: String, done: (CalendarImportPreview) -> Unit): Job? {
        val frozen = bytes.copyOf()
        return inspect("import-preview", expectedContext, { it.previewImport(frozen, calendar) }, done)
    }
    fun previewLocalImport(read: suspend () -> ByteArray, calendar: String?, expectedContext: String, done: (CalendarImportPreview) -> Unit) =
        inspect("import-preview", expectedContext, { c -> val bytes = read(); c.authorization.requireActive(); c.previewImport(bytes, calendar) }, done)
    fun freeBusy(attendees: List<String>, from: Long, to: Long, expectedContext: String, done: (Map<String, CalendarAvailability>) -> Unit) =
        inspect("availability", expectedContext, { it.freeBusy(attendees.toList(), from, to) }, done)
}
