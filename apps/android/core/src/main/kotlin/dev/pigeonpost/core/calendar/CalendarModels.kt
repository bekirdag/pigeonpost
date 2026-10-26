package dev.pigeonpost.core.calendar

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.Transient
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

// Calendar data is sealed at rest by the service, not end-to-end encrypted.
// Unknown statuses stay strings; absent optional fields never grant permission.
enum class CalendarEventScope(val wire: String) { ALL("all"), THIS("this"), FOLLOWING("following") }
enum class CalendarAnswerScope(val wire: String) { ALL("all"), THIS("this") }
enum class CalendarAnswer(val wire: String) { ACCEPTED("accepted"), TENTATIVE("tentative"), DECLINED("declined") }
data class CalendarEventKey(val eventId: String, val occurrenceId: String? = null)

@Serializable data class CalendarFolder(
    val id: String,
    val name: String? = null,
    val color: String? = null,
    @SerialName("is_default") val isDefault: Boolean? = null,
    val revision: Long? = null,
)
@Serializable data class CalendarPerson(val address: String? = null, val handle: String? = null, val name: String? = null)
@Serializable data class CalendarReminder(@SerialName("minutes_before") val minutesBefore: Int, val channel: String? = null)
@Serializable data class CalendarFoldersResponse(val calendars: List<CalendarFolder>? = null)
@Serializable data class CalendarFolderResponse(val calendar: CalendarFolder)

@Serializable data class ScheduledMeeting(
    val enabled: Boolean? = null,
    val code: String? = null,
    val url: String? = null,
    @SerialName("opens_at") val opensAt: Long? = null,
    @SerialName("closes_at") val closesAt: Long? = null,
    val waiting: String? = null,
    val admit: String? = null,
    @SerialName("allow_knock") val allowKnock: Boolean? = null,
    @SerialName("join_before_minutes") val joinBeforeMinutes: Int? = null,
    @SerialName("guests_see_list") val guestsSeeList: Boolean? = null,
    val transport: String? = null,
    @SerialName("participant_limit") val participantLimit: Int? = null,
    val admitters: List<String>? = null,
)

@Serializable data class EventOccurrence(
    @SerialName("occurrence_id") val occurrenceId: String,
    @SerialName("event_id") val eventId: String,
    @SerialName("recurrence_key") val recurrenceKey: String? = null,
    @SerialName("calendar_id") val calendarId: String? = null,
    val color: String? = null,
    @SerialName("all_day") val allDay: Boolean? = null,
    @SerialName("starts_at") val startsAt: Long? = null,
    @SerialName("ends_at") val endsAt: Long? = null,
    val tzid: String? = null,
    @SerialName("start_date") val startDate: String? = null,
    @SerialName("end_date") val endDate: String? = null,
    val title: String? = null,
    val location: String? = null,
    val status: String? = null,
    val recurring: Boolean? = null,
    val overridden: Boolean? = null,
    val organizer: CalendarPerson? = null,
    val role: String? = null,
    @SerialName("my_rsvp") val myRsvp: String? = null,
    val invited: Boolean? = null,
    val meeting: ScheduledMeeting? = null,
) { val key get() = CalendarEventKey(eventId, occurrenceId) }
@Serializable data class EventOccurrencesResponse(val occurrences: List<EventOccurrence>? = null)

@Serializable data class CalendarAttendee(
    @SerialName("attendee_id") val attendeeId: String,
    val kind: String? = null,
    val address: String? = null,
    val handle: String? = null,
    val contact: String? = null,
    val name: String? = null,
    val role: String? = null,
    val optional: Boolean? = null,
    val rsvp: String? = null,
    @SerialName("rsvp_at") val rsvpAt: Long? = null,
    val proof: String? = null,
    val delivery: String? = null,
    val invited: Boolean? = null,
)
@Serializable data class CalendarMembership(
    val role: String? = null,
    @SerialName("attendee_id") val attendeeId: String? = null,
    val rsvp: String? = null,
    @SerialName("calendar_id") val calendarId: String? = null,
)
@Serializable data class ScheduledEvent(
    val id: String,
    val uid: String? = null,
    @SerialName("calendar_id") val calendarId: String? = null,
    val revision: Long? = null,
    val sequence: Long? = null,
    val status: String? = null,
    val organizer: CalendarPerson? = null,
    val title: String? = null,
    val description: String? = null,
    val location: String? = null,
    @SerialName("all_day") val allDay: Boolean? = null,
    val start: String? = null,
    val end: String? = null,
    val tzid: String? = null,
    @SerialName("starts_at") val startsAt: Long? = null,
    @SerialName("ends_at") val endsAt: Long? = null,
    @SerialName("start_date") val startDate: String? = null,
    @SerialName("end_date") val endDate: String? = null,
    val rrule: String? = null,
    val exdates: List<String>? = null,
    val transparency: String? = null,
    val privacy: String? = null,
    val meeting: ScheduledMeeting? = null,
    val attendees: List<CalendarAttendee>? = null,
    val counts: Map<String, Int>? = null,
    val my: CalendarMembership? = null,
    val reminders: List<CalendarReminder>? = null,
    @SerialName("linked_group") val linkedGroup: String? = null,
    @SerialName("can_edit") val canEdit: Boolean? = null,
    val locale: String? = null,
)

/** A detail projection may omit event_id and may change values without changing recurrence identity. */
@Serializable data class EventOccurrenceDetail(
    @SerialName("occurrence_id") val occurrenceId: String? = null,
    @SerialName("recurrence_key") val recurrenceKey: String? = null,
    @SerialName("starts_at") val startsAt: Long? = null,
    @SerialName("ends_at") val endsAt: Long? = null,
    @SerialName("all_day") val allDay: Boolean? = null,
    val tzid: String? = null,
    @SerialName("opens_at") val opensAt: Long? = null,
    @SerialName("closes_at") val closesAt: Long? = null,
    val status: String? = null,
    @SerialName("my_rsvp") val myRsvp: String? = null,
    val title: String? = null,
    val description: String? = null,
    val location: String? = null,
    val start: String? = null,
    val end: String? = null,
    @SerialName("start_date") val startDate: String? = null,
    @SerialName("end_date") val endDate: String? = null,
)
@Serializable data class CalendarEventResponse(
    val event: ScheduledEvent,
    val occurrence: EventOccurrenceDetail? = null,
    @SerialName("delivery_status") val deliveryStatus: String? = null,
    @SerialName("split_from") val splitFrom: CalendarSplit? = null,
    val carried: Map<String, Int>? = null,
    val dropped: Map<String, Int>? = null,
    /** Preserves fields unknown to this app when a person edits a nested policy object. */
    @Transient val original: JsonObject = JsonObject(emptyMap()),
)
@Serializable data class CalendarSplit(@SerialName("event_id") val eventId: String, val revision: Long? = null)
@Serializable data class CalendarInvitation(
    @SerialName("event_id") val eventId: String,
    @SerialName("occurrence_id") val occurrenceId: String? = null,
    val title: String? = null,
    @SerialName("starts_at") val startsAt: Long? = null,
    @SerialName("ends_at") val endsAt: Long? = null,
    @SerialName("all_day") val allDay: Boolean? = null,
    @SerialName("start_date") val startDate: String? = null,
    @SerialName("end_date") val endDate: String? = null,
    val tzid: String? = null,
    val organizer: CalendarPerson? = null,
    val recurring: Boolean? = null,
    val meeting: Boolean? = null,
    @SerialName("invited_at") val invitedAt: Long? = null,
) { val key get() = CalendarEventKey(eventId, occurrenceId) }
@Serializable data class CalendarInvitationsResponse(val invitations: List<CalendarInvitation>? = null)

@Serializable data class CalendarWorkingHours(val start: String? = null, val end: String? = null, val days: List<Int>? = null)
@Serializable data class CalendarSettings(
    val previews: Boolean? = null,
    val timezone: String? = null,
    @SerialName("week_start") val weekStart: Int? = null,
    @SerialName("working_hours") val workingHours: CalendarWorkingHours? = null,
)
@Serializable data class CalendarGuestLink(val url: String, @SerialName("expires_at") val expiresAt: Long? = null) {
    override fun toString() = "CalendarGuestLink(expiresAt=$expiresAt)"
}
@Serializable data class CalendarFeed(
    val url: String? = null,
    val enabled: Boolean? = null,
    val projection: String? = null,
    @SerialName("created_at") val createdAt: Long? = null,
    @SerialName("expires_at") val expiresAt: Long? = null,
) { override fun toString() = "CalendarFeed(enabled=$enabled, projection=$projection)" }
@Serializable data class CalendarImportPreview(
    @SerialName("draft_id") val draftId: String,
    @SerialName("calendar_id") val calendarId: String? = null,
    val events: List<CalendarImportEvent>? = null,
    val warnings: List<String>? = null,
    @SerialName("expires_at") val expiresAt: Long? = null,
)
@Serializable data class CalendarImportEvent(
    val index: Int,
    val importable: Boolean? = null,
    val title: String? = null,
    @SerialName("all_day") val allDay: Boolean? = null,
    val start: String? = null,
    val end: String? = null,
    @SerialName("start_date") val startDate: String? = null,
    @SerialName("end_date") val endDate: String? = null,
    val tzid: String? = null,
    val rrule: String? = null,
    val warnings: List<String>? = null,
)
@Serializable data class CalendarImportResult(val created: List<ScheduledEvent>? = null)
@Serializable data class CalendarBusyInterval(val start: Long, val end: Long)
@Serializable data class CalendarBusyResponse(val busy: Map<String, JsonElement>? = null)
sealed interface CalendarAvailability {
    data object Unknown : CalendarAvailability
    data class Known(val intervals: List<CalendarBusyInterval>) : CalendarAvailability
}
