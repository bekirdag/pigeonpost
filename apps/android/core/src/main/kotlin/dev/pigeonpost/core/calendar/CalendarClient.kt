package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.WireJson
import dev.pigeonpost.core.validAddress
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.*

/** One immutable encoding survives token renewal, explicit retry and protected outbox storage. */
class CalendarWritePayload(fields: JsonObject) {
    internal val text: String = canonical(fields).toString()
    fun bytes(): ByteArray = text.toByteArray(Charsets.UTF_8)
    fun fields(): JsonObject = WireJson.parseToJsonElement(text).jsonObject
    override fun equals(other: Any?) = other is CalendarWritePayload && text == other.text
    override fun hashCode() = text.hashCode()
    override fun toString() = "CalendarWritePayload(bytes=${bytes().size})"
    companion object {
        private fun canonical(value: JsonElement): JsonElement = when (value) {
            is JsonObject -> JsonObject(value.toSortedMap().mapValues { canonical(it.value) })
            is JsonArray -> JsonArray(value.map(::canonical))
            else -> value
        }
        fun restore(encoded: String): CalendarWritePayload {
            val result = CalendarWritePayload(WireJson.parseToJsonElement(encoded).jsonObject)
            require(result.text == encoded) { "The saved calendar attempt cannot be retried safely." }
            return result
        }
    }
}

/** A canonical mailbox plus a revocable child of the accepted login, never a mutable handle. */
class CalendarClient(
    private val transport: CalendarTransport,
    val mailbox: String,
    val authorization: AuthorizationLease = AuthorizationLease(),
) {
    init { require(validAddress(mailbox)) { "Choose a valid calendar inbox." } }
    fun close() { authorization.retire() }

    private suspend fun <T> scoped(work: suspend () -> T): T {
        authorization.requireActive()
        currentCoroutineContext()[AuthorizationContext]?.lease?.let { caller ->
            caller.requireActive()
            if (!caller.sharesAuthorization(authorization)) throw AuthorizationRetired()
        }
        return withContext(AuthorizationContext(authorization)) {
            currentCoroutineContext().ensureActive(); authorization.requireActive()
            work().also { currentCoroutineContext().ensureActive(); authorization.requireActive() }
        }
    }
    private suspend fun raw(path: String, method: String = "GET", query: Map<String, String> = emptyMap(), payload: CalendarWritePayload? = null,
                            bytes: ByteArray? = null, mediaType: String = "application/json") = scoped {
        transport.calendarRequest(mailbox, method, path, query, payload?.bytes() ?: bytes, mediaType)
    }
    private suspend inline fun <reified T> read(path: String, method: String = "GET", query: Map<String, String> = emptyMap(), payload: CalendarWritePayload? = null): T {
        val bytes = raw(path, method, query, payload)
        return withContext(Dispatchers.Default) {
            authorization.withActive {
                try { WireJson.decodeFromString<T>(bytes.toString(Charsets.UTF_8)) }
                catch (_: SerializationException) { throw ApiException(200, "bad_response", "The calendar returned an unreadable response.") }
            }.also { authorization.requireActive() }
        }
    }
    private suspend fun event(path: String, method: String = "GET", query: Map<String, String> = emptyMap(), payload: CalendarWritePayload? = null): CalendarEventResponse {
        val bytes = raw(path, method, query, payload)
        return withContext(Dispatchers.Default) {
            authorization.withActive {
                try {
                    val text = bytes.toString(Charsets.UTF_8)
                    val root = WireJson.parseToJsonElement(text) as? JsonObject ?: throw SerializationException("Missing response")
                    val original = root["event"] as? JsonObject ?: throw SerializationException("Missing event")
                    WireJson.decodeFromString<CalendarEventResponse>(text).copy(original = original)
                } catch (_: SerializationException) { throw ApiException(200, "bad_response", "The calendar returned an unreadable event.") }
            }.also { authorization.requireActive() }
        }
    }
    private fun segment(id: String): String {
        if (!Regex("[A-Za-z0-9_-]{1,128}").matches(id)) throw ApiException(0, "calendar_id", "This calendar item is invalid.")
        return id
    }
    private fun eventPath(id: String, suffix: String = "") = "/v1/calendar/events/" + segment(id) + suffix
    private fun payload(fields: Map<String, JsonElement>) = CalendarWritePayload(JsonObject(fields))
    private fun revision(value: Long): JsonPrimitive {
        require(value >= 0) { "This calendar revision is invalid." }
        return JsonPrimitive(value)
    }
    private fun window(from: Long, to: Long, maximum: Long = 62L * 86400) {
        val span = try { Math.subtractExact(to, from) } catch (_: ArithmeticException) { -1 }
        if (span !in 1..maximum) throw ApiException(0, "window_too_large", "Choose a calendar window of at most 62 days.")
    }
    private fun occurrence(scope: String, occurrenceId: String?): Map<String, JsonElement> {
        require(scope == "all" || occurrenceId != null) { "Choose an occurrence for this change." }
        return mapOf("scope" to JsonPrimitive(scope)) + (occurrenceId?.let { mapOf("occurrence_id" to JsonPrimitive(segment(it))) } ?: emptyMap())
    }
    private fun createKey(payload: CalendarWritePayload) {
        val key = payload.fields()["request_key"] as? JsonPrimitive
        require(key?.isString == true && Regex("[A-Za-z0-9_-]{1,64}").matches(key.content)) { "This calendar attempt has no valid identity." }
    }

    suspend fun calendars(): List<CalendarFolder> = read<CalendarFoldersResponse>("/v1/calendars").calendars.orEmpty()
    suspend fun createCalendar(name: String, color: String, requestKey: String): CalendarFolder {
        val body = payload(mapOf("name" to JsonPrimitive(name), "color" to JsonPrimitive(color), "request_key" to JsonPrimitive(requestKey)))
        return createCalendar(body)
    }
    suspend fun createCalendar(body: CalendarWritePayload): CalendarFolder {
        createKey(body)
        return read<CalendarFolderResponse>("/v1/calendars", "POST", payload = body).calendar
    }
    suspend fun updateCalendar(id: String, expectedRevision: Long, fields: JsonObject): CalendarFolder =
        read<CalendarFolderResponse>("/v1/calendars/" + segment(id), "PATCH", payload = payload(fields + ("expected_revision" to revision(expectedRevision)))).calendar
    suspend fun deleteCalendar(id: String, expectedRevision: Long) {
        raw("/v1/calendars/" + segment(id), "DELETE", payload = payload(mapOf("expected_revision" to revision(expectedRevision))))
    }
    suspend fun occurrences(from: Long, to: Long, calendars: List<String> = emptyList()): List<EventOccurrence> {
        window(from, to)
        val query = mapOf("from" to from.toString(), "to" to to.toString(), "include" to "invitations") +
            if (calendars.isEmpty()) emptyMap() else mapOf("calendars" to calendars.map(::segment).joinToString(","))
        return read<EventOccurrencesResponse>("/v1/calendar/events", query = query).occurrences.orEmpty()
    }
    suspend fun detail(key: CalendarEventKey): CalendarEventResponse {
        val result = event(eventPath(key.eventId), query = key.occurrenceId?.let { mapOf("occurrence_id" to segment(it)) }.orEmpty())
        if (result.event.id != key.eventId || key.occurrenceId != null && result.occurrence?.occurrenceId != key.occurrenceId)
            throw ApiException(200, "bad_response", "The calendar returned a different event.")
        return result
    }
    suspend fun createEvent(body: CalendarWritePayload): CalendarEventResponse {
        createKey(body)
        return event("/v1/calendar/events", "POST", payload = body)
    }
    suspend fun updateEvent(id: String, body: CalendarWritePayload): CalendarEventResponse {
        val fields = body.fields()
        val expectedRevision = fields["expected_revision"] as? JsonPrimitive
        require(expectedRevision?.isString == false && expectedRevision.longOrNull?.let { it >= 0 } == true) { "This calendar revision is invalid." }
        val scope = (fields["scope"] as? JsonPrimitive)?.content
        require(scope in setOf("all", "this", "following")) { "Choose the scope of this change." }
        val selected = fields["occurrence_id"]
        require(selected == null || selected is JsonPrimitive && selected.isString) { "Choose a valid occurrence." }
        occurrence(scope!!, (selected as? JsonPrimitive)?.content)
        val result = event(eventPath(id), "PATCH", payload = body)
        if (scope != "following" && result.event.id != id || scope == "following" && result.splitFrom?.eventId != id)
            throw ApiException(200, "bad_response", "The calendar returned a different event.")
        return result
    }
    suspend fun cancelEvent(key: CalendarEventKey, expectedRevision: Long, scope: CalendarEventScope, message: String? = null) {
        val fields = occurrence(scope.wire, key.occurrenceId) + ("expected_revision" to revision(expectedRevision)) +
            (message?.let { mapOf("message" to JsonPrimitive(it)) } ?: emptyMap())
        raw(eventPath(key.eventId, "/cancel"), "POST", payload = payload(fields))
    }
    suspend fun rsvp(key: CalendarEventKey, answer: CalendarAnswer, scope: CalendarAnswerScope, note: String? = null) {
        require(note == null || note.length <= 300) { "Keep your reply note within 300 characters." }
        val fields = occurrence(scope.wire, key.occurrenceId) + ("status" to JsonPrimitive(answer.wire)) + (note?.let { mapOf("note" to JsonPrimitive(it)) } ?: emptyMap())
        raw(eventPath(key.eventId, "/rsvp"), "POST", payload = payload(fields))
    }
    suspend fun attendees(key: CalendarEventKey, expectedRevision: Long, scope: CalendarAnswerScope, adding: List<JsonObject>, removing: List<String>) {
        val fields = occurrence(scope.wire, key.occurrenceId) + mapOf("expected_revision" to revision(expectedRevision),
            "add" to JsonArray(adding), "remove" to JsonArray(removing.map { JsonPrimitive(segment(it)) }))
        raw(eventPath(key.eventId, "/attendees"), "POST", payload = payload(fields))
    }
    suspend fun linkGroup(id: String, expectedRevision: Long, group: String?) {
        raw(eventPath(id, "/linked-group"), "PATCH", payload = payload(mapOf("expected_revision" to revision(expectedRevision), "group_id" to (group?.let { JsonPrimitive(segment(it)) } ?: JsonNull))))
    }
    suspend fun reminders(id: String, reminders: List<CalendarReminder>) {
        require(reminders.size <= 5 && reminders.all { it.minutesBefore in 0..40320 && (it.channel == null || it.channel == "push") }) { "Choose up to five push reminders, within four weeks." }
        raw(eventPath(id, "/reminders"), "PUT", payload = payload(mapOf("reminders" to JsonArray(reminders.map {
            buildJsonObject { put("minutes_before", it.minutesBefore); put("channel", "push") }
        }))))
    }
    suspend fun moveInvitation(id: String, calendar: String) {
        raw(eventPath(id, "/placement"), "PUT", payload = payload(mapOf("calendar_id" to JsonPrimitive(segment(calendar)))))
    }
    suspend fun invitations(filtered: Boolean): List<CalendarInvitation> =
        read<CalendarInvitationsResponse>("/v1/calendar/invitations", query = mapOf("state" to if (filtered) "filtered" else "pending")).invitations.orEmpty()
    suspend fun guestLink(id: String, attendee: String): CalendarGuestLink =
        read(eventPath(id, "/attendees/" + segment(attendee) + "/link"), "POST")
    suspend fun settings(): CalendarSettings = read("/v1/calendar/settings")
    suspend fun saveSettings(fields: JsonObject): CalendarSettings = read("/v1/calendar/settings", "PUT", payload = CalendarWritePayload(fields))
    suspend fun feed(calendar: String): CalendarFeed = read("/v1/calendar/feeds/" + segment(calendar))
    suspend fun setFeed(calendar: String, enabled: Boolean, projection: String): CalendarFeed {
        require(projection in setOf("busy", "personal_redacted")) { "Choose a calendar feed projection." }
        return read("/v1/calendar/feeds/" + segment(calendar), "POST", payload = payload(mapOf("enabled" to JsonPrimitive(enabled), "projection" to JsonPrimitive(projection))))
    }
    suspend fun ics(id: String): ByteArray = raw(eventPath(id, ".ics"))
    suspend fun freeBusy(attendees: List<String>, from: Long, to: Long): Map<String, CalendarAvailability> {
        require(to > from && attendees.all { validAddress(it) }) { "Choose valid inboxes and an availability window." }
        val result = read<CalendarBusyResponse>("/v1/calendar/freebusy", "POST", payload = payload(mapOf("attendees" to JsonArray(attendees.map(::JsonPrimitive)), "from" to JsonPrimitive(from), "to" to JsonPrimitive(to))))
        return attendees.associateWith { address ->
            val value = result.busy?.get(address)
            if (value !is JsonArray) CalendarAvailability.Unknown else {
                val intervals = runCatching { WireJson.decodeFromString<List<CalendarBusyInterval>>(value.toString()) }.getOrNull()
                if (intervals == null || intervals.any { it.end <= it.start }) CalendarAvailability.Unknown else CalendarAvailability.Known(intervals)
            }
        }.also { authorization.requireActive() }
    }
    suspend fun previewImport(bytes: ByteArray, calendar: String? = null): CalendarImportPreview {
        require(bytes.size <= 512 * 1024) { "Calendar imports must be at most 512 KB." }
        val result = raw("/v1/calendar/import/preview", "POST", query = calendar?.let { mapOf("calendar_id" to segment(it)) }.orEmpty(), bytes = bytes.copyOf(), mediaType = "text/calendar")
        return withContext(Dispatchers.Default) {
            authorization.withActive { WireJson.decodeFromString<CalendarImportPreview>(result.toString(Charsets.UTF_8)) }.also { authorization.requireActive() }
        }
    }
    suspend fun confirmImport(draft: String, selected: List<Int>, calendar: String? = null): CalendarImportResult {
        require(selected.size <= 200 && selected.all { it >= 0 }) { "Choose at most 200 import entries." }
        val fields = mapOf("select" to JsonArray(selected.distinct().sorted().map(::JsonPrimitive))) +
            (calendar?.let { mapOf("calendar_id" to JsonPrimitive(segment(it))) } ?: emptyMap())
        return confirmImport(draft, payload(fields))
    }
    suspend fun confirmImport(draft: String, body: CalendarWritePayload): CalendarImportResult {
        val fields = body.fields()
        val selection = fields["select"] as? JsonArray
        require(selection != null && selection.size <= 200 && selection.all { it is JsonPrimitive && !it.isString && it.intOrNull?.let { index -> index >= 0 } == true }) { "Choose at most 200 import entries." }
        fields["calendar_id"]?.let { require(it is JsonPrimitive && it.isString); segment(it.content) }
        return read("/v1/calendar/import/" + segment(draft) + "/confirm", "POST", payload = body)
    }
}
