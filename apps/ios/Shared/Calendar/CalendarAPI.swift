import Foundation

/// Preserve one encoded body through token renewal and an explicit failed-create retry.
/// The store owns its lifetime and request_key; it is never persisted with calendar details.
struct CalendarWritePayload: Equatable {
    let data: Data

    init(_ fields: [String: Any]) throws {
        guard JSONSerialization.isValidJSONObject(fields) else {
            throw APIError(status: 0, code: "calendar_input", detail: "The calendar request is invalid.")
        }
        data = try JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys, .withoutEscapingSlashes])
    }
}

extension PostboxClient {
    private static func calendarHeaders(_ mailbox: String) -> [String: String] { ["x-pigeonpost-identity": mailbox] }

    private static func calendarSegment(_ id: String) throws -> String {
        // Server ids are opaque ASCII tokens. Refuse path syntax before URL construction rather
        // than letting an imported/stale id normalize into a different authenticated route.
        guard !id.isEmpty, id.utf8.count <= 128, id.utf8.allSatisfy({ byte in
            (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte) || byte == 95 || byte == 45
        }) else { throw APIError(status: 0, code: "calendar_id", detail: "This calendar item is invalid.") }
        return id
    }

    private static func calendarEventPath(_ id: String, _ suffix: String = "") throws -> String {
        "/v1/calendar/events/" + (try calendarSegment(id)) + suffix
    }

    private func calendarWrite(_ path: String, method: String, mailbox: String, payload: CalendarWritePayload) async throws -> Data {
        try await sendRaw(path, method: method, body: payload.data, contentType: "application/json", headers: Self.calendarHeaders(mailbox))
    }

    func calendars(mailbox: String) async throws -> [CalendarFolder] {
        try await send("/v1/calendars", headers: Self.calendarHeaders(mailbox), as: CalendarFoldersResponse.self).calendars ?? []
    }

    func createCalendar(mailbox: String, name: String, color: String, requestKey: String) async throws -> CalendarFolder {
        let payload = try CalendarWritePayload(["name": name, "color": color, "request_key": requestKey])
        let data = try await calendarWrite("/v1/calendars", method: "POST", mailbox: mailbox, payload: payload)
        return try Self.decode(CalendarFolderResponse.self, from: data).calendar
    }

    func updateCalendar(mailbox: String, id: String, revision: Int, fields: [String: Any]) async throws -> CalendarFolder {
        var body = fields
        body["expected_revision"] = revision
        let data = try await calendarWrite("/v1/calendars/" + Self.calendarSegment(id), method: "PATCH", mailbox: mailbox,
                                           payload: CalendarWritePayload(body))
        return try Self.decode(CalendarFolderResponse.self, from: data).calendar
    }

    func deleteCalendar(mailbox: String, id: String, revision: Int) async throws {
        _ = try await calendarWrite("/v1/calendars/" + Self.calendarSegment(id), method: "DELETE", mailbox: mailbox,
                                    payload: CalendarWritePayload(["expected_revision": revision]))
    }

    func calendarOccurrences(mailbox: String, from: Int, to: Int, calendars: [String] = []) async throws -> [EventOccurrence] {
        let span = to.subtractingReportingOverflow(from)
        guard to > from, !span.overflow, span.partialValue <= 62 * 86400 else { throw APIError(status: 0, code: "window_too_large", detail: nil) }
        var query: [URLQueryItem] = [.init(name: "from", value: String(from)), .init(name: "to", value: String(to)),
                                     .init(name: "include", value: "invitations")]
        if !calendars.isEmpty { query.append(.init(name: "calendars", value: try calendars.map(Self.calendarSegment).joined(separator: ","))) }
        return try await send("/v1/calendar/events", query: query, headers: Self.calendarHeaders(mailbox), as: EventOccurrencesResponse.self).occurrences ?? []
    }

    func calendarEvent(mailbox: String, id: String, occurrenceId: String? = nil) async throws -> CalendarEventResponse {
        let query: [URLQueryItem] = occurrenceId.map { [.init(name: "occurrence_id", value: $0)] } ?? []
        return try await send(Self.calendarEventPath(id), query: query, headers: Self.calendarHeaders(mailbox), as: CalendarEventResponse.self)
    }

    func createCalendarEvent(mailbox: String, payload: CalendarWritePayload) async throws -> CalendarEventResponse {
        let data = try await calendarWrite("/v1/calendar/events", method: "POST", mailbox: mailbox, payload: payload)
        return try Self.decode(CalendarEventResponse.self, from: data)
    }

    func updateCalendarEvent(mailbox: String, id: String, payload: CalendarWritePayload) async throws -> CalendarEventResponse {
        let data = try await calendarWrite(Self.calendarEventPath(id), method: "PATCH", mailbox: mailbox, payload: payload)
        return try Self.decode(CalendarEventResponse.self, from: data)
    }

    func cancelCalendarEvent(mailbox: String, id: String, revision: Int, scope: CalendarEventScope,
                             occurrenceId: String?, message: String? = nil) async throws {
        var body: [String: Any] = ["expected_revision": revision, "scope": scope.rawValue]
        if let occurrenceId { body["occurrence_id"] = occurrenceId }
        if let message { body["message"] = message }
        _ = try await calendarWrite(Self.calendarEventPath(id, "/cancel"), method: "POST", mailbox: mailbox, payload: CalendarWritePayload(body))
    }

    func calendarRSVP(mailbox: String, id: String, status: CalendarAnswer, scope: CalendarAnswerScope,
                      occurrenceId: String?, note: String? = nil) async throws {
        var body: [String: Any] = ["status": status.rawValue, "scope": scope.rawValue]
        if let occurrenceId { body["occurrence_id"] = occurrenceId }
        if let note { body["note"] = note }
        _ = try await calendarWrite(Self.calendarEventPath(id, "/rsvp"), method: "POST", mailbox: mailbox, payload: CalendarWritePayload(body))
    }

    func calendarAttendees(mailbox: String, id: String, revision: Int, scope: CalendarAnswerScope,
                           occurrenceId: String?, adding: [[String: Any]], removing: [String]) async throws {
        var body: [String: Any] = ["expected_revision": revision, "scope": scope.rawValue, "add": adding, "remove": removing]
        if let occurrenceId { body["occurrence_id"] = occurrenceId }
        _ = try await calendarWrite(Self.calendarEventPath(id, "/attendees"), method: "POST", mailbox: mailbox, payload: CalendarWritePayload(body))
    }

    func linkCalendarGroup(mailbox: String, id: String, revision: Int, group: String?) async throws {
        let body: [String: Any] = ["expected_revision": revision, "group_id": group.map { $0 as Any } ?? NSNull()]
        _ = try await calendarWrite(Self.calendarEventPath(id, "/linked-group"), method: "PATCH", mailbox: mailbox, payload: CalendarWritePayload(body))
    }

    func calendarReminders(mailbox: String, id: String, reminders: [CalendarReminder]) async throws {
        guard reminders.count <= 5, reminders.allSatisfy({ (0...40320).contains($0.minutesBefore) && ($0.channel == nil || $0.channel == "push") }) else {
            throw APIError(status: 0, code: "calendar_input", detail: "Choose up to five push reminders, within four weeks.")
        }
        let body: [String: Any] = ["reminders": reminders.map { ["minutes_before": $0.minutesBefore, "channel": "push"] as [String: Any] }]
        _ = try await calendarWrite(Self.calendarEventPath(id, "/reminders"), method: "PUT", mailbox: mailbox, payload: CalendarWritePayload(body))
    }

    func moveCalendarInvitation(mailbox: String, id: String, calendar: String) async throws {
        _ = try await calendarWrite(Self.calendarEventPath(id, "/placement"), method: "PUT", mailbox: mailbox,
                                    payload: CalendarWritePayload(["calendar_id": Self.calendarSegment(calendar)]))
    }

    func calendarInvitations(mailbox: String, filtered: Bool) async throws -> [CalendarInvitation] {
        try await send("/v1/calendar/invitations", query: [.init(name: "state", value: filtered ? "filtered" : "pending")],
                       headers: Self.calendarHeaders(mailbox), as: CalendarInvitationsResponse.self).invitations ?? []
    }

    func calendarGuestLink(mailbox: String, id: String, attendee: String) async throws -> CalendarGuestLink {
        try await send(Self.calendarEventPath(id, "/attendees/" + Self.calendarSegment(attendee) + "/link"), method: "POST",
                       headers: Self.calendarHeaders(mailbox), as: CalendarGuestLink.self)
    }

    func calendarSettings(mailbox: String) async throws -> CalendarSettings {
        try await send("/v1/calendar/settings", headers: Self.calendarHeaders(mailbox), as: CalendarSettings.self)
    }

    func saveCalendarSettings(mailbox: String, fields: [String: Any]) async throws -> CalendarSettings {
        let data = try await calendarWrite("/v1/calendar/settings", method: "PUT", mailbox: mailbox, payload: CalendarWritePayload(fields))
        return try Self.decode(CalendarSettings.self, from: data)
    }

    func calendarFeed(mailbox: String, id: String) async throws -> CalendarFeed {
        try await send("/v1/calendar/feeds/" + Self.calendarSegment(id), headers: Self.calendarHeaders(mailbox), as: CalendarFeed.self)
    }

    func setCalendarFeed(mailbox: String, id: String, enabled: Bool, projection: String) async throws -> CalendarFeed {
        let data = try await calendarWrite("/v1/calendar/feeds/" + Self.calendarSegment(id), method: "POST", mailbox: mailbox,
                                           payload: CalendarWritePayload(["enabled": enabled, "projection": projection]))
        return try Self.decode(CalendarFeed.self, from: data)
    }

    func calendarICS(mailbox: String, id: String) async throws -> Data {
        try await sendRaw(Self.calendarEventPath(id, ".ics"), headers: Self.calendarHeaders(mailbox))
    }

    func calendarFreeBusy(mailbox: String, attendees: [String], from: Int, to: Int) async throws -> [String: CalendarBusy] {
        let payload = try CalendarWritePayload(["attendees": attendees, "from": from, "to": to])
        let data = try await calendarWrite("/v1/calendar/freebusy", method: "POST", mailbox: mailbox, payload: payload)
        return try Self.decode(CalendarBusyResponse.self, from: data).busy ?? [:]
    }

    func previewCalendarImport(mailbox: String, data: Data, calendar: String?) async throws -> CalendarImportPreview {
        guard data.count <= 512 * 1024 else { throw APIError(status: 0, code: "calendar_input", detail: "Calendar imports must be at most 512 KB.") }
        let query: [URLQueryItem] = try calendar.map { [.init(name: "calendar_id", value: try Self.calendarSegment($0))] } ?? []
        let response = try await sendRaw("/v1/calendar/import/preview", method: "POST", query: query, body: data, contentType: "text/calendar",
                                         headers: Self.calendarHeaders(mailbox))
        return try Self.decode(CalendarImportPreview.self, from: response)
    }

    func confirmCalendarImport(mailbox: String, draft: String, selected: [Int], calendar: String) async throws -> CalendarImportResult {
        let data = try await calendarWrite("/v1/calendar/import/" + Self.calendarSegment(draft) + "/confirm", method: "POST", mailbox: mailbox,
                                           payload: CalendarWritePayload(["select": selected, "calendar_id": Self.calendarSegment(calendar)]))
        return try Self.decode(CalendarImportResult.self, from: data)
    }
}
