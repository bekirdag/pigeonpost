package dev.pigeonpost.inbox

import dev.pigeonpost.core.*
import dev.pigeonpost.core.calendar.*
import kotlinx.serialization.json.*
import java.io.IOException
import java.time.Instant
import java.util.UUID

/** Opt-in, in-memory native workflow data. It cannot make HTTP requests or send invitations. */
internal class CalendarFixture(private val base: PostboxApi, private val mode: String) : PostboxApi by base, CalendarTransport, CalendarMeetingApi {
    override suspend fun identities() = listOf(IdentityRow("/k/demomain", "Main"), IdentityRow("/k/demoagent", "Build agent"))
    override suspend fun whoami(identity: String) = WhoAmI(identity, if (identity == "/k/demomain") "/demo/main" else "/demo/agent")
    override suspend fun capabilities() = Capabilities(listOf("calendar"))
    override suspend fun scheduledMeeting(mailbox: String, code: String): CalendarMeetingStatus {
        if (mailbox != "/k/demomain" || code != "ABCDEFGHJK") return CalendarMeetingStatus("unavailable", emptyList())
        val instant = Instant.now().epochSecond
        return CalendarMeetingStatus("open", listOf(
            CalendarMeetingOccurrence("occ_first", instant - 300, instant + 1800, instant - 900, instant + 2700),
            CalendarMeetingOccurrence("occ_second", instant - 60, instant + 2100, instant - 600, instant + 3000)))
    }
    private var settings = CalendarSettings(timezone = "Europe/Istanbul", weekStart = 1, previews = false)
    private val folders = mutableListOf(CalendarFolder("cal_main", "Personal", "blue", true, 1))
    private val events = linkedMapOf<String, ScheduledEvent>()
    private val receipts = mutableMapOf<String, ByteArray>()
    private var lost = false
    private var conflict = false
    private val day = CalendarDay.at(Instant.now(), CalendarTime.zone("Europe/Istanbul")).text
    init {
        events["evt_review"] = ScheduledEvent("evt_review", revision = 7, title = "Planning review", description = "A native calendar workflow", location = "Meeting room",
            allDay = false, start = day + "T09:00", end = day + "T10:00", tzid = "Europe/Istanbul",
            startsAt = CalendarWallTime(day + "T09:00", "Europe/Istanbul").resolve(), endsAt = CalendarWallTime(day + "T10:00", "Europe/Istanbul").resolve(),
            canEdit = true, calendarId = "cal_main", organizer = CalendarPerson(handle = "/demo/main"), my = CalendarMembership(role = "organizer"), reminders = listOf(CalendarReminder(10, "push")),
            attendees = listOf(CalendarAttendee("att_guest", kind = "external", name = "Candidate", contact = "candidate@example.invalid", role = "attendee", rsvp = "pending")))
        events["evt_invited"] = events["evt_review"]!!.copy(id = "evt_invited", title = "Shared planning", canEdit = false,
            organizer = CalendarPerson(handle = "/demo/builder"), my = CalendarMembership(role = "attendee", rsvp = "pending"), attendees = listOf(CalendarAttendee("att_redacted", kind = "external", name = "Guest")))
    }
    private fun eventBytes(e: ScheduledEvent, occurrence: String? = null) = WireJson.encodeToString(CalendarEventResponse.serializer(), CalendarEventResponse(e,
        occurrence?.let { EventOccurrenceDetail(occurrenceId = it, start = e.start, end = e.end, startsAt = e.startsAt, endsAt = e.endsAt, title = e.title, myRsvp = e.my?.rsvp) })).toByteArray()
    private fun rows(mailbox: String) = if (mailbox != "/k/demomain") emptyList() else events.values.map { e -> EventOccurrence("occ_" + e.id, e.id,
        calendarId = e.calendarId, allDay = e.allDay, startsAt = e.startsAt, endsAt = e.endsAt, startDate = e.startDate, endDate = e.endDate, tzid = e.tzid,
        title = e.title, location = e.location, status = e.status, organizer = e.organizer, role = e.my?.role, myRsvp = e.my?.rsvp, meeting = e.meeting) }
    override suspend fun calendarRequest(mailbox: String, method: String, path: String, query: Map<String, String>, body: ByteArray?, mediaType: String): ByteArray {
        val fields = body?.takeIf { mediaType == "application/json" }?.let { WireJson.parseToJsonElement(it.toString(Charsets.UTF_8)).jsonObject } ?: JsonObject(emptyMap())
        fun string(key: String) = fields[key]?.jsonPrimitive?.contentOrNull
        when (path) {
            "/v1/calendars" -> if (method == "GET") return WireJson.encodeToString(CalendarFoldersResponse.serializer(), CalendarFoldersResponse(folders)).toByteArray() else {
                val key = string("request_key")!!
                return receipts.getOrPut(key) {
                    val folder = CalendarFolder("cal_" + folders.size, string("name"), string("color"), false, 1); folders += folder
                    WireJson.encodeToString(CalendarFolderResponse.serializer(), CalendarFolderResponse(folder)).toByteArray()
                }
            }
            "/v1/calendar/settings" -> {
                if (method == "GET" && mode == "calendar-zone-delay") kotlinx.coroutines.delay(1500)
                if (method == "PUT") settings = settings.copy(timezone = string("timezone") ?: settings.timezone, previews = fields["previews"]?.jsonPrimitive?.boolean ?: settings.previews,
                    weekStart = fields["week_start"]?.jsonPrimitive?.int ?: settings.weekStart,
                    workingHours = fields["working_hours"]?.let { WireJson.decodeFromString<CalendarWorkingHours>(it.toString()) } ?: settings.workingHours)
                return WireJson.encodeToString(CalendarSettings.serializer(), settings).toByteArray()
            }
            "/v1/calendar/invitations" -> return if (query["state"] == "filtered" || mailbox != "/k/demomain" || events["evt_invited"]?.my?.rsvp != "pending") "{\"invitations\":[]}".toByteArray()
                else """{"invitations":[{"event_id":"evt_invited","occurrence_id":"occ_evt_invited","title":"Shared planning","organizer":{"handle":"/demo/builder"}}]}""".toByteArray()
            "/v1/calendar/events" -> if (method == "GET") return WireJson.encodeToString(EventOccurrencesResponse.serializer(), EventOccurrencesResponse(rows(mailbox))).toByteArray() else {
                val key = string("request_key")!!
                val result = receipts.getOrPut(key) {
                    val allDay = fields["all_day"]?.jsonPrimitive?.boolean ?: false
                    val zone = string("tzid") ?: "UTC"
                    val fold = string("fold")?.let { value -> CalendarFold.entries.first { it.wire == value } }
                    val event = ScheduledEvent("evt_" + UUID.randomUUID().toString(), revision = 1, title = string("title"), description = string("description"), location = string("location"), allDay = allDay,
                        start = string("start"), end = string("end"), startDate = string("start_date"), endDate = string("end_date"), tzid = zone,
                        startsAt = string("start")?.let { CalendarWallTime(it, zone).resolve(fold) }, endsAt = string("end")?.let { CalendarWallTime(it, zone).resolve(fold) }, rrule = string("rrule"), calendarId = string("calendar_id") ?: "cal_main", canEdit = true, my = CalendarMembership(role = "organizer"))
                    events[event.id] = event; eventBytes(event)
                }
                if (mode == "calendar-lost" && !lost) { lost = true; throw IOException("Fixture lost an accepted response") }
                return result
            }
        }
        if (path.startsWith("/v1/calendars/")) {
            val id = path.substringAfterLast('/'); val index = folders.indexOfFirst { it.id == id }
            if (index < 0) throw ApiException(404, "not_found")
            if (method == "DELETE") { folders.removeAt(index); return "{}".toByteArray() }
            val updated = folders[index].copy(name = string("name") ?: folders[index].name, color = string("color") ?: folders[index].color, isDefault = fields["is_default"]?.jsonPrimitive?.boolean ?: folders[index].isDefault, revision = folders[index].revision!! + 1)
            folders[index] = updated; return WireJson.encodeToString(CalendarFolderResponse.serializer(), CalendarFolderResponse(updated)).toByteArray()
        }
        if (path.startsWith("/v1/calendar/events/")) {
            val rest = path.removePrefix("/v1/calendar/events/"); val id = rest.substringBefore('/')
            val event = events[id] ?: throw ApiException(404, "not_found")
            if (method == "GET") return eventBytes(event, query["occurrence_id"])
            if (method == "PATCH" && rest == id) {
                if (mode == "calendar-conflict" && !conflict) { conflict = true; events[id] = event.copy(revision = event.revision!! + 1, title = "Changed by another client"); throw ApiException(409, "revision_conflict") }
                if (fields["expected_revision"]?.jsonPrimitive?.long != event.revision) throw ApiException(409, "revision_conflict")
                val updated = event.copy(title = string("title") ?: event.title, description = string("description") ?: event.description, location = string("location") ?: event.location, revision = event.revision!! + 1)
                events[id] = updated; return eventBytes(updated)
            }
            if (rest.endsWith("/cancel")) events[id] = event.copy(status = "cancelled")
            if (rest.endsWith("/rsvp")) events[id] = event.copy(my = event.my?.copy(rsvp = string("status")))
            if (rest.endsWith("/reminders")) events[id] = event.copy(reminders = WireJson.decodeFromString(fields["reminders"].toString()))
            if (rest.endsWith("/link")) return """{"url":"https://fixture.invalid/personal-invitation","expires_at":1900000000}""".toByteArray()
            return "{}".toByteArray()
        }
        if (path.startsWith("/v1/calendar/feeds/")) return if (method == "GET") "{\"enabled\":false}".toByteArray() else """{"enabled":true,"url":"https://fixture.invalid/feed"}""".toByteArray()
        throw ApiException(404, "not_found")
    }
}
