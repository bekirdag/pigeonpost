import Foundation

@main
struct CalendarTests {
    static var checks = 0
    static var failures = 0

    static func check(_ value: Bool, _ label: String) {
        checks += 1
        if value { print("  ok   " + label) } else { failures += 1; print("  FAIL " + label) }
    }

    static func rejects(_ error: CalendarTimeError, _ label: String, _ body: () throws -> Void) {
        do { try body(); check(false, label) }
        catch let actual as CalendarTimeError { check(actual == error, label) }
        catch { check(false, label) }
    }

    static func instant(_ text: String) -> Date { ISO8601DateFormatter().date(from: text)! }

    static func main() throws {
        let leap = try CalendarDay("2024-02-29")
        check(leap.adding(days: 1).id == "2024-03-01", "civil date arithmetic keeps leap days")
        check(try CalendarDay("2026-12-31").adding(days: 1).id == "2027-01-01", "year rollover is a calendar day")
        rejects(.invalidFormat, "a non-leap February does not normalize into March") { _ = try CalendarDay("2026-02-29") }
        for text in ["2026-04-31", "2026-1-01", "2026/10/22", "2026-00-02", "2026-10-00", "٢٠٢٦-10-22", "2026-10-22Z"] {
            rejects(.invalidFormat, "refuses a malformed civil date: " + text) { _ = try CalendarDay(text) }
        }
        let istanbul = try CalendarWallTime("2026-10-22T09:00:30", tzid: "Europe/Istanbul")
        check(try istanbul.resolve() == instant("2026-10-22T06:00:30Z"), "Istanbul wall time resolves without changing seconds")
        check(istanbul.text == "2026-10-22T09:00:30" && istanbul.tzid == "Europe/Istanbul", "the typed text and timezone remain unchanged")
        for text in ["2026-10-22T24:00", "2026-10-22T09:60", "2026-10-22T09:00Z", "2026-10-22 09:00", "2026-10-22T9:00", "2026-10-22T09:00:60"] {
            rejects(.invalidFormat, "refuses a malformed wall time: " + text) { _ = try CalendarWallTime(text, tzid: "UTC") }
        }
        rejects(.unknownZone, "an unknown zone cannot become the system default") { _ = try CalendarWallTime("2026-10-22T09:00", tzid: "Not/A_Zone") }
        rejects(.nonexistent, "New York's spring gap does not silently move to 03:30") {
            _ = try CalendarWallTime("2026-03-08T02:30", tzid: "America/New_York").resolve()
        }
        let repeated = try CalendarWallTime("2026-11-01T01:30", tzid: "America/New_York")
        let first = instant("2026-11-01T05:30:00Z"), second = instant("2026-11-01T06:30:00Z")
        rejects(.ambiguous(earlier: Int(first.timeIntervalSince1970), later: Int(second.timeIntervalSince1970)),
                "a repeated time requires the person's fold decision") { _ = try repeated.resolve() }
        check(try repeated.resolve(fold: .earlier) == first, "Earlier selects the first offset")
        check(try repeated.resolve(fold: .later) == second, "Later selects the second offset")
        rejects(.order, "an end at the start is refused") {
            _ = try CalendarWallTime.range(start: "2026-10-22T09:00", end: "2026-10-22T09:00", tzid: "UTC")
        }
        rejects(.order, "an end before the start is refused") {
            _ = try CalendarWallTime.range(start: "2026-10-22T10:00", end: "2026-10-22T09:00", tzid: "UTC")
        }
        let zone = TimeZone(identifier: "America/New_York")!
        let springDay = try CalendarDay("2026-03-08")
        let autumnDay = try CalendarDay("2026-11-01")
        check(try springDay.adding(days: 1).start(in: zone).timeIntervalSince(springDay.start(in: zone)) == 23 * 3600,
              "a spring calendar day has 23 hours")
        check(try autumnDay.adding(days: 1).start(in: zone).timeIntervalSince(autumnDay.start(in: zone)) == 25 * 3600,
              "an autumn calendar day has 25 hours")
        let grid = try CalendarMonthWindow(containing: CalendarDay("2026-10-25"), weekStart: 1)
        check(grid.days.count == 42 && grid.days.first?.id == "2026-09-28" && grid.days.last?.id == "2026-11-08",
              "a Monday-first month includes six full weeks")
        let bounds = try grid.bounds(in: zone)
        check(bounds.upperBound.timeIntervalSince(bounds.lowerBound) == 42 * 86400 + 3600,
              "the month request spans local dates rather than fixed 24-hour blocks")
        rejects(.invalidFormat, "an invalid first weekday is refused") { _ = try CalendarMonthWindow(containing: leap, weekStart: 7) }
        let localDay = CalendarDay(date: instant("2026-10-21T23:30:00Z"), zone: TimeZone(identifier: "Europe/Istanbul")!)
        check(localDay.id == "2026-10-22", "Today comes from the selected zone, not UTC")
        check(try CalendarDay("2026-10-22").adding(days: 1).id == "2026-10-23", "an all-day inclusive last day can be encoded as the exclusive end")

        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let folders = try decoder.decode(CalendarFoldersResponse.self, from: Data(#"{"calendars":[]}"#.utf8))
        check(folders.calendars?.isEmpty == true, "reading an empty calendar collection invents no default")
        let occurrence = try decoder.decode(EventOccurrence.self, from: Data(#"{"occurrence_id":"occ_moved","event_id":"evt_one","recurrence_key":"20261022T090000","starts_at":1792738800,"ends_at":1792742400,"title":"Moved","status":"future_status","role":"future_role","meeting":null}"#.utf8))
        check(occurrence.id == "occ_moved" && occurrence.recurrenceKey == "20261022T090000", "moved instances keep their opaque identity and original key")
        check(occurrence.role == "future_role" && occurrence.status == "future_status", "unknown server roles/statuses remain decodable data")
        let detail = try decoder.decode(CalendarEventResponse.self, from: Data(#"{"event":{"id":"evt_one","revision":3,"can_edit":false,"attendees":[{"attendee_id":"att_guest","kind":"external","name":"Guest"}],"meeting":{"code":"ABCDEFGHJK","transport":"sfu","participant_limit":30}},"occurrence":{"occurrence_id":"occ_moved","my_rsvp":"accepted"}}"#.utf8))
        check(detail.event.attendees?.first?.contact == nil && detail.event.canEdit == false,
              "attendee projections do not invent a guest contact or editing permission")
        check(detail.occurrence?.occurrenceId == "occ_moved" && detail.event.meeting?.participantLimit == 30,
              "partial detail occurrences and scheduled meeting limits decode")
        let settings = try decoder.decode(CalendarSettings.self, from: Data(#"{"previews":false,"timezone":null,"week_start":1,"working_hours":{"start":"09:00","end":"18:00","days":[1,2,3,4,5]}}"#.utf8))
        check(settings.previews == false && settings.timezone == nil && settings.workingHours?.days == [1,2,3,4,5],
              "preview opt-out and unset timezone remain explicit")
        let availability = try decoder.decode(CalendarBusyResponse.self, from: Data(#"{"busy":{"/k/a":"unknown","/k/b":[],"/k/c":[{"start":10,"end":20}],"/k/d":"future_status"}}"#.utf8))
        check(availability.busy?["/k/a"] == .unknown && availability.busy?["/k/d"] == .unknown,
              "unknown or future availability is never displayed as free")
        check(availability.busy?["/k/b"] == .known([]) && availability.busy?["/k/c"] == .known([.init(start: 10, end: 20)]),
              "known empty availability stays distinct from unknown")
        print("Calendar models: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }
}
