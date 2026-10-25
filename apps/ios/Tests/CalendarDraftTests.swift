import Foundation

@main
struct CalendarDraftTests {
    static var checks = 0
    static var failures = 0
    static func check(_ value: Bool, _ label: String) {
        checks += 1
        if value { print("  ok   " + label) } else { failures += 1; print("  FAIL " + label) }
    }
    static func event(_ json: String) throws -> CalendarEventResponse {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(CalendarEventResponse.self, from: Data(json.utf8))
    }
    static func main() throws {
        let istanbul = TimeZone(identifier: "Europe/Istanbul")!
        var draft = CalendarEventDraft(day: try CalendarDay("2026-10-25"), zone: istanbul)
        do { _ = try draft.fields(); check(false, "an empty title is refused") }
        catch CalendarDraftError.title { check(true, "an empty title is refused") }
        draft.title = "  Review  "
        var body = try draft.fields()
        check(body["title"] as? String == "Review" && body["calendar_id"] == nil, "a first event can use the default without creating a calendar on read")
        check(body["start"] as? String == "2026-10-25T09:00" && body["tzid"] as? String == "Europe/Istanbul", "authoring retains the literal wall time and zone")
        draft.allDay = true; draft.lastDay = try CalendarDay("2026-10-27")
        body = try draft.fields()
        check(body["start_date"] as? String == "2026-10-25" && body["end_date"] as? String == "2026-10-28", "a multi-day inclusive UI end becomes an exclusive wire end")
        check(body["start"] == nil && body["tzid"] == nil, "all-day events do not acquire a timezone or timed start")
        draft.lastDay = try CalendarDay("2026-10-24")
        do { _ = try draft.fields(); check(false, "all-day backwards ranges are refused") }
        catch CalendarTimeError.order { check(true, "all-day backwards ranges are refused") }
        draft.lastDay = draft.startDay; draft.meeting.enabled = true
        do { _ = try draft.fields(); check(false, "a call needs an explicit timed range") }
        catch CalendarDraftError.meetingOnAllDay { check(true, "a call needs an explicit timed range") }

        draft = CalendarEventDraft(day: try CalendarDay("2026-03-08"), zone: TimeZone(identifier: "America/New_York")!)
        draft.title = "Gap"; draft.startTime = "02:30"; draft.endTime = "03:30"
        do { _ = try draft.fields(); check(false, "the editor refuses a spring gap instead of shifting it") }
        catch CalendarTimeError.nonexistent { check(true, "the editor refuses a spring gap instead of shifting it") }
        draft.startDay = try CalendarDay("2026-11-01"); draft.lastDay = draft.startDay
        draft.startTime = "01:15"; draft.endTime = "01:45"
        do { _ = try draft.fields(); check(false, "a repeated wall time requires an explicit choice") }
        catch CalendarTimeError.ambiguous { check(true, "a repeated wall time requires an explicit choice") }
        draft.fold = .later; body = try draft.fields()
        check(body["fold"] as? String == "later", "the chosen later offset reaches the wire")
        draft.changeTime()
        check(draft.fold == nil, "changing the wall time clears the previous repeated-time choice")

        let response = try event(#"{"event":{"id":"evt_series","revision":7,"title":"Series","description":"Keep","location":"Room","all_day":false,"start":"2026-10-22T09:00:00","end":"2026-10-22T10:00:00","tzid":"Europe/Istanbul","rrule":"FREQ=WEEKLY;COUNT=6","exdates":["20261029T090000"],"locale":"tr","meeting":{"code":"ABCDEFGHJK","waiting":"everyone_except_hosts","admit":"delegated","admitters":["/k/host"],"transport":"sfu","join_before_minutes":10}},"occurrence":{"occurrence_id":"occ_original","title":"Moved","start":"2026-11-02T14:30:00","end":"2026-11-02T15:30:00","tzid":"Europe/Istanbul","all_day":false}}"#)
        let key = CalendarEventKey(eventId: "evt_series", occurrenceId: "occ_original")
        let one = CalendarEditTarget(response: response, key: key, scope: .this)
        draft = try CalendarEventDraft(target: one, fallbackZone: istanbul)
        check(draft.title == "Moved" && draft.startDay.id == "2026-11-02" && draft.startTime == "14:30:00", "one occurrence starts from its effective moved values")
        check(draft.occurrenceId == "occ_original" && draft.revision == 7, "a moved edit retains its original opaque id and captured revision")
        check(try draft.fields().isEmpty, "opening an editor does not reschedule unchanged values")
        draft.title = "New occurrence title"
        body = try draft.fields()
        check(body.count == 1 && body["title"] as? String == "New occurrence title", "a title-only occurrence edit changes only its title")
        check(body["rrule"] == nil && body["attendees"] == nil && body["meeting"] == nil && body["exdates"] == nil,
              "a single-occurrence edit never replaces series settings or attendees")
        draft.startTime = "15:00"; body = try draft.fields()
        check(body["end"] as? String == "2026-11-02T15:30:00" && body["all_day"] as? Bool == false && body["tzid"] as? String == "Europe/Istanbul",
              "a time change sends the complete effective timing group")
        draft = try CalendarEventDraft(target: .init(response: response, key: key, scope: .all), fallbackZone: istanbul)
        check(draft.title == "Series" && draft.startDay.id == "2026-10-22", "whole-series editing uses the series rather than a moved instance")
        check(draft.meeting.enabled && draft.meeting.admitters == ["/k/host"] && draft.meeting.transport == "sfu", "existing co-host delegation and SFU transport survive an unrelated edit")
        draft.location = "New room"; body = try draft.fields()
        check(body.count == 1 && body["location"] as? String == "New room", "an unrelated series edit preserves recurrence exceptions and invitation language")
        draft.rrule = ""; body = try draft.fields()
        check(body["rrule"] is NSNull && body["exdates"] == nil, "removing recurrence is explicit without inventing an empty exception set")
        var missing = response; missing.occurrence = nil
        do { _ = try CalendarEventDraft(target: .init(response: missing, key: key, scope: .following), fallbackZone: istanbul); check(false, "an unavailable occurrence cannot silently edit the series") }
        catch let error as APIError { check(error.code == "calendar_unavailable", "an unavailable occurrence cannot silently edit the series") }
        let allDay = try event(#"{"event":{"id":"evt_day","revision":2,"title":"Offsite","all_day":true,"start_date":"2026-10-25","end_date":"2026-10-28"}}"#)
        draft = try CalendarEventDraft(target: .init(response: allDay, key: .init(eventId: "evt_day"), scope: .all), fallbackZone: istanbul)
        body = try draft.fields()
        check(draft.lastDay.id == "2026-10-27" && body.isEmpty, "opening an existing all-day event keeps its last displayed day")
        var guest = CalendarInviteeDraft(); guest.destination = "guest@example.test"; guest.cohost = true
        check(try guest.fields()?["role"] == nil, "a contact guest never acquires co-host powers")
        guest.destination = "/bekir/hr"; guest.optional = true
        let invitee = try guest.fields()
        check(invitee?["role"] as? String == "cohost" && invitee?["optional"] as? Bool == true, "a handle invitee can be an optional co-host explicitly")
        check(try CalendarReminderInput.parse("  ").isEmpty, "only an explicitly empty reminder field removes all reminders")
        check(try CalendarReminderInput.parse("0, 10, 40320") == [0, 10, 40320], "valid reminder boundaries and spaces are preserved")
        for invalid in ["abc", "10,abc", "10,", ",", "-1", "٤", "40321", "1,1", "1,2,3,4,5,6"] {
            do { _ = try CalendarReminderInput.parse(invalid); check(false, "invalid reminders cannot erase existing choices: " + invalid) }
            catch CalendarDraftError.reminders { check(true, "invalid reminders cannot erase existing choices: " + invalid) }
        }
        print("Calendar drafts: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }
}
