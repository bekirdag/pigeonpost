package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.WireJson
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class CalendarDraftTest {
    private fun fails(work: () -> Unit) {
        try { work(); fail("Expected an invalid draft") } catch (_: IllegalArgumentException) { }
    }
    private fun response(extra: String = "", occurrence: EventOccurrenceDetail? = null): CalendarEventResponse {
        val raw = WireJson.parseToJsonElement("""{"id":"evt_one","revision":7,"title":"Reviewed","description":"Details","location":"Here","all_day":false,"start":"2026-10-28T09:00:07","end":"2026-10-28T10:00:09","tzid":"Europe/Istanbul","rrule":"FREQ=WEEKLY;COUNT=6","exdates":["20261104T090007"],"can_edit":true,"attendees":[{"attendee_id":"att_one"}]$extra}""").jsonObject
        return CalendarEventResponse(WireJson.decodeFromString<ScheduledEvent>(raw.toString()), occurrence, original = raw)
    }
    private fun draft(r: CalendarEventResponse = response(), scope: CalendarEventScope = CalendarEventScope.ALL, key: CalendarEventKey = CalendarEventKey("evt_one")) =
        CalendarEventDraft.from(CalendarEditTarget(r, key, scope, "reviewed-context"), "UTC")
    private fun new() = CalendarEventDraft("create-context", "2026-10-28", tzid = "Europe/Istanbul", title = "New")

    @Test fun untouchedEditorHasNoChangesAndCannotSendAPatch() {
        val d = draft(); assertTrue(d.changedFields().isEmpty()); fails { d.editPayload() }
    }
    @Test fun titleEditKeepsFrozenNumericRevisionWithoutReplacingIndependentState() {
        val body = draft().copy(title = "Changed").editPayload().fields()
        assertEquals(setOf("title", "expected_revision", "scope"), body.keys)
        assertEquals(7L, body["expected_revision"]!!.jsonPrimitive.long)
        assertFalse(body["expected_revision"]!!.jsonPrimitive.isString)
        assertEquals("all", body["scope"]!!.jsonPrimitive.content)
    }
    @Test fun timeEditUsesACompleteLiteralTimingGroupAndKeepsSeconds() {
        val body = draft().copy(startTime = "08:30:07").fields()
        assertEquals(setOf("all_day", "start", "end", "tzid"), body.keys)
        assertEquals("2026-10-28T08:30:07", body["start"]!!.jsonPrimitive.content)
        assertEquals("2026-10-28T10:00:09", body["end"]!!.jsonPrimitive.content)
        assertEquals("Europe/Istanbul", body["tzid"]!!.jsonPrimitive.content)
    }
    @Test fun switchingToAllDayUsesInclusiveUiAndExclusiveWireEnd() {
        val body = draft().copy(allDay = true, lastDay = "2026-10-30").fields()
        assertEquals("2026-10-28", body["start_date"]!!.jsonPrimitive.content)
        assertEquals("2026-10-31", body["end_date"]!!.jsonPrimitive.content)
        assertFalse("start" in body); assertFalse("end" in body); assertFalse("tzid" in body)
        fails { new().copy(allDay = true, lastDay = "2026-10-27").fields() }
    }
    @Test fun allDayEditorDoesNotShiftDatesThroughTheDisplayZone() {
        val r = response().copy(event = response().event.copy(allDay = true, startDate = "2026-10-28", endDate = "2026-10-30", tzid = null))
        val d = draft(r); assertEquals("2026-10-28", d.startDay); assertEquals("2026-10-29", d.lastDay)
        assertEquals(setOf("title"), d.copy(title = "Changed").fields().keys)
    }
    @Test fun oneOccurrenceKeepsItsOpaqueIdentityAndCannotPatchTheSeriesPolicy() {
        val occurrence = EventOccurrenceDetail(occurrenceId = "occ_original", title = "Moved", start = "2026-11-05T12:00", end = "2026-11-05T13:00")
        val d = draft(response(occurrence = occurrence), CalendarEventScope.THIS, CalendarEventKey("evt_one", "occ_original"))
        assertEquals("Moved", d.title); assertEquals("2026-11-05", d.startDay)
        val body = d.copy(title = "Changed", rrule = "", privacy = "private", meeting = CalendarMeetingDraft(enabled = true)).editPayload().fields()
        assertEquals(setOf("title", "expected_revision", "scope", "occurrence_id"), body.keys)
        assertEquals("this", body["scope"]!!.jsonPrimitive.content)
        assertEquals("occ_original", body["occurrence_id"]!!.jsonPrimitive.content)
    }
    @Test fun followingEditUsesTheReviewedOccurrenceAndSeriesRule() {
        val occurrence = EventOccurrenceDetail(occurrenceId = "occ_original", start = "2026-11-04T09:00:07", end = "2026-11-04T10:00:09")
        val body = draft(response(occurrence = occurrence), CalendarEventScope.FOLLOWING, CalendarEventKey("evt_one", "occ_original")).copy(rrule = "FREQ=WEEKLY;COUNT=3").editPayload().fields()
        assertEquals("following", body["scope"]!!.jsonPrimitive.content)
        assertEquals("occ_original", body["occurrence_id"]!!.jsonPrimitive.content)
        assertEquals("FREQ=WEEKLY;COUNT=3", body["rrule"]!!.jsonPrimitive.content)
        assertFalse("attendees" in body); assertFalse("exdates" in body)
    }
    @Test fun missingOrDifferentOccurrenceAndMissingPermissionAreRefused() {
        fails { draft(scope = CalendarEventScope.THIS) }
        fails { draft(response(occurrence = EventOccurrenceDetail(occurrenceId = "other")), CalendarEventScope.FOLLOWING, CalendarEventKey("evt_one", "selected")) }
        fails { draft(response().copy(event = response().event.copy(canEdit = null))) }
        fails { draft(response().copy(event = response().event.copy(revision = null))) }
        fails { draft(key = CalendarEventKey("evt_other")) }
    }
    @Test fun changingAnEditorContextCannotReuseItsOldProof() {
        fails { draft().copy(contextId = "replacement-context", title = "Changed").editPayload() }
    }
    @Test fun unknownPolicyValuesDoNotPreventAnUnrelatedEdit() {
        val d = draft(response(",\"privacy\":\"future_privacy\",\"meeting\":{\"enabled\":true,\"waiting\":\"future_waiting\",\"transport\":\"future_transport\"}"))
        assertEquals(setOf("title"), d.copy(title = "Changed").fields().keys)
        fails { d.copy(meeting = d.meeting.copy(allowKnock = true)).fields() }
    }
    @Test fun changingMeetingPolicyPreservesOpaqueFieldsAndOmitsResponseCapabilities() {
        val d = draft(response(",\"meeting\":{\"enabled\":true,\"waiting\":\"guests\",\"admit\":\"delegated\",\"admitters\":[\"/host\"],\"future_policy\":{\"mode\":\"strict\"},\"code\":\"LOCATOR\",\"url\":\"https://fixture.invalid/private\",\"opens_at\":1,\"participant_limit\":6}"))
        val meeting = d.copy(meeting = d.meeting.copy(allowKnock = true)).fields()["meeting"]!!.jsonObject
        assertEquals("strict", meeting["future_policy"]!!.jsonObject["mode"]!!.jsonPrimitive.content)
        assertEquals("/host", meeting["admitters"]!!.jsonArray.single().jsonPrimitive.content)
        for (key in listOf("url", "code", "opens_at", "participant_limit")) assertFalse(key in meeting)
    }
    @Test fun repeatedOffsetIsPreservedButChangingTimingRequiresAChoiceAgain() {
        val r = response().copy(event = response().event.copy(start = "2026-11-01T01:15", end = "2026-11-01T01:45", tzid = "America/New_York", startsAt = 1793513700, endsAt = 1793515500))
        val d = draft(r); assertEquals(CalendarFold.LATER, d.fold)
        assertEquals(setOf("title"), d.copy(title = "Changed").fields().keys)
        fails { d.copy(startTime = "01:20", fold = null).fields() }
        assertEquals("later", d.copy(startTime = "01:20").fields()["fold"]!!.jsonPrimitive.content)
    }
    @Test fun nonexistentOrReversedTimesAndOffsetZonesAreRefused() {
        fails { new().copy(startDay = "2026-03-08", lastDay = "2026-03-08", startTime = "02:30", endTime = "03:30", tzid = "America/New_York").fields() }
        fails { new().copy(endTime = "08:00").fields() }
        fails { new().copy(tzid = "+03:00").fields() }
    }
    @Test fun newEventDefaultsAndExplicitReminderClearAreEncoded() {
        val body = new().copy(reminderText = "").fields()
        assertTrue(body["attendees"]!!.jsonArray.isEmpty()); assertTrue(body["reminders"]!!.jsonArray.isEmpty())
        assertEquals(false, body["meeting"]!!.jsonObject["enabled"]!!.jsonPrimitive.boolean)
        assertEquals(JsonNull, body["rrule"])
        assertFalse("request_key" in body)
        assertFalse("reminders" in draft().copy(reminderText = "").copy(title = "Changed").fields())
    }
    @Test fun recurrenceClearsWithJsonNullAndInvalidHeadersAreRefused() {
        assertEquals(JsonNull, draft().copy(rrule = "").fields()["rrule"])
        for (rule in listOf("RRULE:FREQ=WEEKLY", "rrule:FREQ=WEEKLY", "FREQ=WEEKLY\nCOUNT=3", "FREQ=WEEKLY\rCOUNT=3")) fails { new().copy(rrule = rule).fields() }
    }
    @Test fun meetingRequiresATimedEventAndSupportedPolicies() {
        fails { new().copy(allDay = true, meeting = CalendarMeetingDraft(enabled = true)).fields() }
        fails { new().copy(meeting = CalendarMeetingDraft(enabled = true, joinBeforeMinutes = 11)).fields() }
        assertEquals("sfu", new().copy(meeting = CalendarMeetingDraft(enabled = true, transport = "sfu")).fields()["meeting"]!!.jsonObject["transport"]!!.jsonPrimitive.content)
    }
    @Test fun handleCohostAndContactGuestInputsStayDistinct() {
        val body = new().copy(invitees = listOf(CalendarInviteeDraft(" /wodo/hr ", cohost = true), CalendarInviteeDraft("candidate@example.invalid", "Candidate", cohost = true, optional = true), CalendarInviteeDraft())).fields()
        val people = body["attendees"]!!.jsonArray
        assertEquals(2, people.size); assertEquals("cohost", people[0].jsonObject["role"]!!.jsonPrimitive.content)
        assertFalse("role" in people[1].jsonObject); assertEquals("candidate@example.invalid", people[1].jsonObject["contact"]!!.jsonPrimitive.content)
        fails { CalendarInviteeDraft("not an address").fields() }; fails { CalendarInviteeDraft("invalid").fields() }
    }
    @Test fun remindersAcceptBoundsAndWhitespaceButRejectAmbiguousOrMalformedInput() {
        assertEquals(emptyList<Int>(), CalendarReminderInput.parse("  "))
        assertEquals(listOf(0, 1, 10, 60, 40320), CalendarReminderInput.parse(" 0, 01,10,60,40320 "))
        for (text in listOf("1,01", "-1", "1.5", "١", "１", "1,,2", ",1", "1,", "40321", "99999999999999999999", "0,1,2,3,4,5")) fails { CalendarReminderInput.parse(text) }
    }
    @Test fun draftsAndTargetsNeverPrintAuthoredTextOrCapabilityUrls() {
        val d = draft().copy(title = "private-title", description = "private-description")
        assertFalse(d.toString().contains("private")); assertFalse(d.target.toString().contains("Reviewed"))
        assertFalse(CalendarInviteeDraft("private@example.invalid").toString().contains("private"))
    }
}
