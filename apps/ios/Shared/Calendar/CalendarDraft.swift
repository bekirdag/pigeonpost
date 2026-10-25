import Foundation

/// The revision and scope are a snapshot. A poll never rewrites an editor's draft underneath it.
struct CalendarEditTarget: Identifiable {
    let id = UUID()
    let response: CalendarEventResponse
    let key: CalendarEventKey
    let scope: CalendarEventScope
}

enum CalendarDraftError: LocalizedError {
    case title, invitee, meetingOnAllDay, unsupportedMeeting, recurrence, reminders
    var errorDescription: String? {
        switch self {
        case .title: return "Give this event a title."
        case .invitee: return "Enter a Pigeonpost handle or a guest's contact address."
        case .meetingOnAllDay: return "Choose a start and end time for a call."
        case .unsupportedMeeting: return "Review this meeting's lobby and admission settings."
        case .recurrence: return "Enter a recurrence rule without the RRULE: prefix."
        case .reminders: return "Enter up to five distinct whole minutes between 0 and 40320, separated by commas."
        }
    }
}

enum CalendarReminderInput {
    static func parse(_ text: String) throws -> [Int] {
        let clean = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !clean.isEmpty else { return [] }
        let parts = clean.split(separator: ",", omittingEmptySubsequences: false).map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
        guard parts.count <= 5 else { throw CalendarDraftError.reminders }
        let values = try parts.map { value -> Int in
            guard !value.isEmpty, value.utf8.allSatisfy({ (48...57).contains($0) }), let minutes = Int(value), (0...40320).contains(minutes) else { throw CalendarDraftError.reminders }
            return minutes
        }
        guard Set(values).count == values.count else { throw CalendarDraftError.reminders }
        return values
    }
}

struct CalendarInviteeDraft: Identifiable {
    let id = UUID()
    var destination = ""
    var name = ""
    var cohost = false
    var optional = false

    func fields() throws -> [String: Any]? {
        let value = destination.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return nil }
        guard !value.contains(where: { $0.isWhitespace || $0.isNewline }), value.utf8.count <= 512 else { throw CalendarDraftError.invitee }
        var result: [String: Any] = value.hasPrefix("/") ? ["handle": value] : ["contact": value]
        if !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { result["name"] = name }
        if value.hasPrefix("/"), cohost { result["role"] = "cohost" }
        if optional { result["optional"] = true }
        return result
    }
}

struct CalendarMeetingDraft: Equatable {
    var enabled = false
    var waiting = "guests"
    var admit = "hosts"
    var allowKnock = false
    var joinBeforeMinutes = 10
    var guestsSeeList = false
    var transport = "mesh"
    var admitters: [String] = []

    init(_ meeting: ScheduledMeeting? = nil) {
        guard let meeting else { return }
        enabled = meeting.enabled ?? (meeting.code?.isEmpty == false)
        waiting = meeting.waiting ?? waiting; admit = meeting.admit ?? admit
        allowKnock = meeting.allowKnock ?? false
        joinBeforeMinutes = meeting.joinBeforeMinutes ?? 10
        guestsSeeList = meeting.guestsSeeList ?? false
        transport = meeting.transport ?? transport
        admitters = meeting.admitters ?? []
    }

    func fields() throws -> [String: Any] {
        guard enabled else { return ["enabled": false] }
        guard ["guests", "everyone_except_hosts", "none"].contains(waiting),
              ["organizer", "hosts", "delegated"].contains(admit), ["mesh", "sfu"].contains(transport),
              (0...10).contains(joinBeforeMinutes) else { throw CalendarDraftError.unsupportedMeeting }
        return ["enabled": true, "waiting": waiting, "admit": admit, "allow_knock": allowKnock,
                "join_before_minutes": joinBeforeMinutes, "guests_see_list": guestsSeeList,
                "transport": transport, "admitters": admitters]
    }
}

struct CalendarEventDraft {
    var title = ""
    var description = ""
    var location = ""
    var calendarId: String?
    var allDay = false
    var startDay: CalendarDay
    /// The UI's last day is inclusive. The wire's end_date is the next date.
    var lastDay: CalendarDay
    var startTime = "09:00"
    var endTime = "10:00"
    var tzid: String
    var fold: CalendarFold?
    var rrule = ""
    var transparency = "opaque"
    var privacy = "default"
    var locale = "en"
    var meeting = CalendarMeetingDraft()
    var invitees: [CalendarInviteeDraft] = []
    var reminderMinutes: Int? = 10
    let target: CalendarEditTarget?
    private var original: [String: Data] = [:]

    init(day: CalendarDay, zone: TimeZone, calendarId: String? = nil) {
        startDay = day; lastDay = day; tzid = zone.identifier; self.calendarId = calendarId
        target = nil
    }

    init(target: CalendarEditTarget, fallbackZone: TimeZone) throws {
        self.target = target
        let event = target.response.event
        if target.scope != .all {
            guard let id = target.key.occurrenceId, target.response.occurrence?.occurrenceId == id else {
                throw APIError(status: 0, code: "calendar_unavailable", detail: nil)
            }
        }
        let occurrence = target.scope == .all ? nil : target.response.occurrence
        title = occurrence?.title ?? event.title ?? ""
        description = occurrence?.description ?? event.description ?? ""
        location = occurrence?.location ?? event.location ?? ""
        calendarId = event.calendarId
        allDay = occurrence?.allDay ?? event.allDay ?? false
        tzid = occurrence?.tzid ?? event.tzid ?? fallbackZone.identifier
        let start = occurrence?.start ?? event.start
        let end = occurrence?.end ?? event.end
        if allDay {
            startDay = try CalendarDay(occurrence?.startDate ?? event.startDate ?? "")
            lastDay = try CalendarDay(occurrence?.endDate ?? event.endDate ?? "").adding(days: -1)
        } else {
            let first = try CalendarWallTime(start ?? "", tzid: tzid)
            let last = try CalendarWallTime(end ?? "", tzid: tzid)
            startDay = first.day; lastDay = last.day
            startTime = String(first.text.dropFirst(11)); endTime = String(last.text.dropFirst(11))
            // Preserve the original offset when editing an existing repeated wall time.
            for (wall, instant) in [(first, occurrence?.startsAt ?? event.startsAt), (last, occurrence?.endsAt ?? event.endsAt)] {
                do { _ = try wall.resolve() }
                catch CalendarTimeError.ambiguous(let earlier, let later) {
                    if instant == later { fold = .later }
                    else if instant == earlier { fold = .earlier }
                }
            }
        }
        rrule = event.rrule ?? ""; transparency = event.transparency ?? "opaque"
        privacy = event.privacy ?? "default"; locale = event.locale ?? "en"
        meeting = CalendarMeetingDraft(event.meeting)
        reminderMinutes = event.reminders?.first?.minutesBefore
        // Snapshot only fields this screen understands. Exdates, attendees and other independent
        // settings are deliberately absent from PATCH rather than replaced with empty arrays.
        original = try Self.fingerprints(authored(validate: false))
    }

    var isOccurrenceEdit: Bool { target?.scope == .this }
    var revision: Int? { target?.response.event.revision }
    var occurrenceId: String? { target?.key.occurrenceId }

    mutating func changeTime() { fold = nil }

    func fields() throws -> [String: Any] {
        let body = try authored(validate: true)
        guard target != nil else { return body }
        let encoded = try Self.fingerprints(body)
        var changed = body.filter { original[$0.key] != encoded[$0.key] }
        // The server accepts a complete timing group, never half a start/end or a mixed kind.
        let timing = ["all_day", "start", "end", "start_date", "end_date", "tzid", "fold"]
        if timing.contains(where: { changed[$0] != nil }) {
            for key in timing { if let value = body[key] { changed[key] = value } }
        }
        return changed
    }

    private func authored(validate: Bool) throws -> [String: Any] {
        let cleanTitle = title.trimmingCharacters(in: .whitespacesAndNewlines)
        if validate && cleanTitle.isEmpty { throw CalendarDraftError.title }
        var body: [String: Any] = ["title": cleanTitle, "description": description, "location": location, "all_day": allDay]
        if allDay {
            if validate && lastDay < startDay { throw CalendarTimeError.order }
            body["start_date"] = startDay.id; body["end_date"] = lastDay.adding(days: 1).id
        } else {
            let start = startDay.id + "T" + startTime, end = lastDay.id + "T" + endTime
            if validate { _ = try CalendarWallTime.range(start: start, end: end, tzid: tzid, fold: fold) }
            body["start"] = start; body["end"] = end; body["tzid"] = tzid
            if let fold { body["fold"] = fold.rawValue }
        }
        guard !isOccurrenceEdit else { return body }
        if let calendarId { body["calendar_id"] = calendarId }
        let rule = rrule.trimmingCharacters(in: .whitespacesAndNewlines)
        if validate && (rule.uppercased().hasPrefix("RRULE:") || rule.contains("\n") || rule.contains("\r")) { throw CalendarDraftError.recurrence }
        body["rrule"] = rule.isEmpty ? NSNull() : rule as Any
        body["transparency"] = transparency; body["privacy"] = privacy; body["locale"] = locale
        if validate && allDay && meeting.enabled { throw CalendarDraftError.meetingOnAllDay }
        body["meeting"] = validate ? try meeting.fields() : Self.meetingSnapshot(meeting)
        if target == nil {
            body["attendees"] = try invitees.compactMap { try $0.fields() }
            body["reminders"] = reminderMinutes.map { [["minutes_before": $0, "channel": "push"] as [String: Any]] } ?? []
        }
        return body
    }

    private static func meetingSnapshot(_ value: CalendarMeetingDraft) -> [String: Any] {
        if !value.enabled { return ["enabled": false] }
        return ["enabled": true, "waiting": value.waiting, "admit": value.admit, "allow_knock": value.allowKnock,
                "join_before_minutes": value.joinBeforeMinutes, "guests_see_list": value.guestsSeeList,
                "transport": value.transport, "admitters": value.admitters]
    }

    private static func fingerprints(_ body: [String: Any]) throws -> [String: Data] {
        try body.mapValues { try JSONSerialization.data(withJSONObject: ["value": $0], options: [.sortedKeys, .withoutEscapingSlashes]) }
    }
}
