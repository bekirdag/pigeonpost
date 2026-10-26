package dev.pigeonpost.core.calendar

import kotlinx.serialization.json.*

/** Snapshot of the reviewed event, occurrence, revision and authorization; polling cannot rebase it. */
class CalendarEditTarget(val response: CalendarEventResponse, val key: CalendarEventKey,
                         val scope: CalendarEventScope, val contextId: String) {
    init {
        require(response.event.id == key.eventId && response.event.canEdit == true) { "This event cannot be edited by this inbox." }
        require(response.event.revision?.let { it >= 0 } == true) { "Reload this event before editing." }
        require(scope == CalendarEventScope.ALL || key.occurrenceId != null && response.occurrence?.occurrenceId == key.occurrenceId) { "Choose the occurrence you want to edit." }
    }
    override fun toString() = "CalendarEditTarget(scope=$scope)"
}

object CalendarReminderInput {
    fun parse(text: String): List<Int> {
        if (text.isBlank()) return emptyList()
        val parts = text.split(',').map(String::trim)
        val message = "Enter up to five distinct whole minutes between 0 and 40320, separated by commas."
        require(parts.size <= 5) { message }
        val values = parts.map { value ->
            require(Regex("[0-9]+").matches(value)) { message }
            value.toIntOrNull()?.takeIf { it in 0..40320 } ?: throw IllegalArgumentException(message)
        }
        require(values.distinct().size == values.size) { message }
        return values
    }
}

data class CalendarInviteeDraft(val destination: String = "", val name: String = "", val cohost: Boolean = false, val optional: Boolean = false) {
    fun fields(): JsonObject? {
        val value = destination.trim()
        if (value.isEmpty()) return null
        require(value.toByteArray().size <= 512 && value.none { it.isWhitespace() || it.isISOControl() } &&
            (value.startsWith('/') || value.contains('@'))) { "Enter a Pigeonpost handle or a guest's contact address." }
        return buildJsonObject {
            put(if (value.startsWith('/')) "handle" else "contact", value)
            if (name.isNotBlank()) put("name", name)
            if (value.startsWith('/') && cohost) put("role", "cohost")
            if (optional) put("optional", true)
        }
    }
    override fun toString() = "CalendarInviteeDraft()"
}

data class CalendarMeetingDraft(
    val enabled: Boolean = false, val waiting: String = "guests", val admit: String = "hosts",
    val allowKnock: Boolean = false, val joinBeforeMinutes: Int = 10, val guestsSeeList: Boolean = false,
    val transport: String = "mesh", val admitters: List<String> = emptyList(),
    private val original: JsonObject = JsonObject(emptyMap()),
) {
    fun fields(validate: Boolean = true): JsonObject {
        if (!enabled) return buildJsonObject { put("enabled", false) }
        if (validate) require(waiting in setOf("guests", "everyone_except_hosts", "none") &&
            admit in setOf("organizer", "hosts", "delegated") && transport in setOf("mesh", "sfu") && joinBeforeMinutes in 0..10) {
            "Review this meeting's lobby and admission settings."
        }
        // Preserve opaque policy fields, but never echo response-only codes or capability URLs.
        val opaque = original.filterKeys { it !in setOf("code", "url", "opens_at", "closes_at", "participant_limit") }
        return JsonObject(opaque + buildJsonObject {
            put("enabled", true); put("waiting", waiting); put("admit", admit); put("allow_knock", allowKnock)
            put("join_before_minutes", joinBeforeMinutes); put("guests_see_list", guestsSeeList); put("transport", transport)
            put("admitters", JsonArray(admitters.map(::JsonPrimitive)))
        })
    }
    override fun toString() = "CalendarMeetingDraft(enabled=$enabled)"
    companion object {
        fun from(value: ScheduledMeeting?, original: JsonObject) = CalendarMeetingDraft(
            enabled = value?.enabled ?: !value?.code.isNullOrEmpty(), waiting = value?.waiting ?: "guests", admit = value?.admit ?: "hosts",
            allowKnock = value?.allowKnock ?: false, joinBeforeMinutes = value?.joinBeforeMinutes ?: 10,
            guestsSeeList = value?.guestsSeeList ?: false, transport = value?.transport ?: "mesh", admitters = value?.admitters.orEmpty(), original = original)
    }
}

/** In-memory authored strings only. The protected pending store owns a sent create's frozen bytes. */
data class CalendarEventDraft(
    val contextId: String, val startDay: String, val lastDay: String = startDay, val tzid: String,
    val title: String = "", val description: String = "", val location: String = "", val calendarId: String? = null,
    val allDay: Boolean = false, val startTime: String = "09:00", val endTime: String = "10:00", val fold: CalendarFold? = null,
    val rrule: String = "", val transparency: String = "opaque", val privacy: String = "default", val locale: String = "en",
    val meeting: CalendarMeetingDraft = CalendarMeetingDraft(), val invitees: List<CalendarInviteeDraft> = emptyList(), val reminderText: String = "10",
    val target: CalendarEditTarget? = null, private val original: JsonObject = JsonObject(emptyMap()),
) {
    val isOccurrenceEdit get() = target?.scope == CalendarEventScope.THIS
    private fun authored(): JsonObject = buildJsonObject {
        put("title", title.trim()); put("description", description); put("location", location); put("all_day", allDay)
        if (allDay) { put("start_date", CalendarDay(startDay).text); put("end_date", CalendarDay(lastDay).plus(1).text) }
        else { put("start", startDay + "T" + startTime); put("end", lastDay + "T" + endTime); put("tzid", tzid); fold?.let { put("fold", it.wire) } }
        if (isOccurrenceEdit) return@buildJsonObject
        calendarId?.let { put("calendar_id", it) }
        put("rrule", rrule.trim().takeIf { it.isNotEmpty() }?.let(::JsonPrimitive) ?: JsonNull)
        put("transparency", transparency); put("privacy", privacy); put("locale", locale); put("meeting", meeting.fields(validate = false))
    }
    fun changedFields(): JsonObject {
        val body = authored()
        if (target == null) return body
        require(contextId == target.contextId) { "Reopen this editor in the current inbox." }
        val changed = body.filter { original[it.key] != it.value }.toMutableMap()
        val timing = setOf("all_day", "start", "end", "start_date", "end_date", "tzid", "fold")
        if (timing.any { body[it] != original[it] }) body.filterKeys { it in timing }.forEach { (k, v) -> changed[k] = v }
        return JsonObject(changed)
    }
    fun fields(): JsonObject {
        val body = changedFields().toMutableMap()
        if (target == null || "title" in body) require(title.isNotBlank() && title.length <= 300) { "Give this event a title of at most 300 characters." }
        if (target == null || "all_day" in body) {
            if (allDay) require(CalendarDay(lastDay) >= CalendarDay(startDay)) { "The last day must not be before the first day." }
            else CalendarWallTime.range(startDay + "T" + startTime, lastDay + "T" + endTime, tzid, fold)
            require(!allDay || !meeting.enabled) { "Choose a start and end time for a call." }
        }
        if ("rrule" in body) require(!rrule.trim().startsWith("RRULE:", ignoreCase = true) && '\n' !in rrule && '\r' !in rrule && rrule.length <= 1000) { "Enter a recurrence rule without the RRULE: prefix." }
        if ("meeting" in body) body["meeting"] = meeting.fields()
        if ("privacy" in body) require(privacy in setOf("default", "private")) { "Choose an event privacy setting." }
        if ("transparency" in body) require(transparency in setOf("opaque", "transparent")) { "Choose whether this event blocks your time." }
        if ("locale" in body) require(locale in setOf("en", "tr")) { "Choose English or Turkish for guest invitations." }
        if (target == null) {
            val people = invitees.mapNotNull { it.fields() }
            require(people.count { "handle" in it } <= 500 && people.count { "contact" in it } <= 50) { "Choose at most 500 Pigeonpost invitees and 50 contact guests." }
            body["attendees"] = JsonArray(people)
            body["reminders"] = JsonArray(CalendarReminderInput.parse(reminderText).map { buildJsonObject { put("minutes_before", it); put("channel", "push") } })
        }
        return JsonObject(body)
    }
    fun editPayload(): CalendarWritePayload {
        val t = requireNotNull(target) { "This is a new event." }
        val changed = fields()
        require(changed.isNotEmpty()) { "There are no changes to save." }
        return CalendarWritePayload(JsonObject(changed + buildJsonObject {
            put("expected_revision", t.response.event.revision!!); put("scope", t.scope.wire)
            t.key.occurrenceId?.let { put("occurrence_id", it) }
        }))
    }
    override fun toString() = "CalendarEventDraft(editing=${target != null})"
    companion object {
        fun from(target: CalendarEditTarget, fallbackZone: String): CalendarEventDraft {
            val e = target.response.event
            val o = target.response.occurrence.takeUnless { target.scope == CalendarEventScope.ALL }
            val allDay = o?.allDay ?: e.allDay ?: false
            val zone = o?.tzid ?: e.tzid ?: fallbackZone
            val first = if (allDay) null else CalendarWallTime(o?.start ?: e.start.orEmpty(), zone)
            val last = if (allDay) null else CalendarWallTime(o?.end ?: e.end.orEmpty(), zone)
            val choices = listOf(first to (o?.startsAt ?: e.startsAt), last to (o?.endsAt ?: e.endsAt)).mapNotNull { (wall, instant) ->
                if (wall == null) null else try { wall.resolve(); null } catch (ambiguity: CalendarTimeError.Ambiguous) {
                    when (instant) { ambiguity.earlier -> CalendarFold.EARLIER; ambiguity.later -> CalendarFold.LATER; else -> null }
                } catch (_: CalendarTimeError.Nonexistent) { null } // Untouched historical timing is never sent back.
            }.distinct()
            val draft = CalendarEventDraft(contextId = target.contextId, target = target,
                title = o?.title ?: e.title.orEmpty(), description = o?.description ?: e.description.orEmpty(), location = o?.location ?: e.location.orEmpty(),
                calendarId = e.calendarId, allDay = allDay, tzid = zone,
                startDay = if (allDay) CalendarDay(o?.startDate ?: e.startDate.orEmpty()).text else first!!.day.text,
                lastDay = if (allDay) CalendarDay(o?.endDate ?: e.endDate.orEmpty()).plus(-1).text else last!!.day.text,
                startTime = first?.text?.substring(11) ?: "09:00", endTime = last?.text?.substring(11) ?: "10:00", fold = choices.singleOrNull(),
                rrule = e.rrule.orEmpty(), transparency = e.transparency ?: "opaque", privacy = e.privacy ?: "default", locale = e.locale ?: "en",
                meeting = CalendarMeetingDraft.from(e.meeting, target.response.original["meeting"] as? JsonObject ?: JsonObject(emptyMap())),
                reminderText = e.reminders.orEmpty().joinToString(", ") { it.minutesBefore.toString() })
            return draft.copy(original = draft.authored())
        }
    }
}
