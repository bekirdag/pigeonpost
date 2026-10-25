import Foundation

@MainActor final class Session {
    var renewals = 0
    func token() async throws -> String { renewals == 0 ? "fixture-before" : "fixture-after" }
    func renew() async throws -> String { renewals += 1; return "fixture-after" }
}
enum AuthError: Error { case sessionExpired }

final class CalendarTransport: URLProtocol {
    static let lock = NSLock()
    static var requests: [URLRequest] = []
    static var bodies: [Data] = []
    static var challenge = true

    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "calendar.test" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        var body = request.httpBody ?? Data()
        if body.isEmpty, let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                body.append(contentsOf: buffer.prefix(count))
            }
        }
        Self.lock.lock()
        Self.requests.append(request)
        Self.bodies.append(body)
        let unauthorized = Self.challenge && request.url?.path == "/v1/calendar/events" && request.httpMethod == "POST"
        if unauthorized { Self.challenge = false }
        Self.lock.unlock()
        let path = request.url!.path
        let data: Data
        if unauthorized { data = Data(#"{"error":"unauthorized"}"#.utf8) }
        else if path == "/v1/calendar/events", request.httpMethod == "GET" { data = Data(#"{"occurrences":[]}"#.utf8) }
        else if path == "/v1/calendar/import/preview" { data = Data(#"{"draft_id":"imp_fixture","events":[]}"#.utf8) }
        else if path.hasSuffix(".ics") { data = Data("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".utf8) }
        else if path == "/v1/calendar/settings" { data = Data(#"{"previews":false,"timezone":null,"week_start":1}"#.utf8) }
        else if path.hasPrefix("/v1/calendar/feeds/") { data = Data(#"{"enabled":false,"projection":"busy"}"#.utf8) }
        else { data = Data(#"{"event":{"id":"evt_fixture","revision":8}}"#.utf8) }
        let response = HTTPURLResponse(url: request.url!, statusCode: unauthorized ? 401 : 200,
                                       httpVersion: nil, headerFields: ["Content-Type": path.hasSuffix(".ics") ? "text/calendar" : "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }

    static func snapshot() -> ([URLRequest], [Data]) {
        lock.lock(); defer { lock.unlock() }
        return (requests, bodies)
    }
}

@main
struct CalendarAPITests {
    static var checks = 0
    static var failures = 0
    static func check(_ value: Bool, _ label: String) {
        checks += 1
        if value { print("  ok   " + label) } else { failures += 1; print("  FAIL " + label) }
    }

    @MainActor static func main() async throws {
        URLProtocol.registerClass(CalendarTransport.self)
        defer { URLProtocol.unregisterClass(CalendarTransport.self) }
        let session = Session()
        let client = PostboxClient(base: URL(string: "https://calendar.test")!, tokens: session)
        let payload = try CalendarWritePayload(["request_key": "calendar_fixture_key", "title": "Design review",
            "all_day": false, "start": "2026-10-22T09:00", "end": "2026-10-22T10:00", "tzid": "Europe/Istanbul"])
        let created = try await client.createCalendarEvent(mailbox: "/k/mailbox_a", payload: payload)
        let (first, bodies) = CalendarTransport.snapshot()
        check(created.event.id == "evt_fixture" && session.renewals == 1, "a real shared-client 401 renews once then decodes the event")
        check(first.count == 2 && bodies[0] == payload.data && bodies[1] == payload.data, "renewal reuses byte-identical JSON and request identity")
        check(first.allSatisfy { $0.value(forHTTPHeaderField: "x-pigeonpost-identity") == "/k/mailbox_a" }, "renewal never changes the acting mailbox")
        check(first[0].value(forHTTPHeaderField: "authorization") == "Bearer fixture-before"
              && first[1].value(forHTTPHeaderField: "authorization") == "Bearer fixture-after", "only the bearer changes after renewal")
        let edit = try CalendarWritePayload(["expected_revision": 7, "scope": "this", "occurrence_id": "occ_moved", "title": "Moved review"])
        _ = try await client.updateCalendarEvent(mailbox: "/k/mailbox_b", id: "evt_fixture", payload: edit)
        let updated = CalendarTransport.snapshot()
        check(updated.0.last?.httpMethod == "PATCH" && updated.0.last?.url?.path == "/v1/calendar/events/evt_fixture", "edits use the scoped event route")
        check(updated.0.last?.value(forHTTPHeaderField: "x-pigeonpost-identity") == "/k/mailbox_b", "another mailbox keeps its own authority")
        check(updated.1.last == edit.data && String(data: edit.data, encoding: .utf8)!.contains("\"expected_revision\":7"),
              "revision remains an integer and the occurrence scope is preserved")
        let occurrences = try await client.calendarOccurrences(mailbox: "/k/mailbox_a", from: 100, to: 10000, calendars: ["cal_a", "cal_b"])
        let listing = CalendarTransport.snapshot().0.last!
        let query = Dictionary(uniqueKeysWithValues: (URLComponents(url: listing.url!, resolvingAgainstBaseURL: false)!.queryItems ?? []).map { ($0.name, $0.value ?? "") })
        check(occurrences.isEmpty && listing.httpMethod == "GET", "an empty occurrence listing performs no calendar creation")
        check(query["from"] == "100" && query["to"] == "10000" && query["calendars"] == "cal_a,cal_b" && query["include"] == "invitations",
              "listing preserves the requested window, folders and invitations")
        let before = CalendarTransport.snapshot().0.count
        do { _ = try await client.calendarEvent(mailbox: "/k/mailbox_a", id: "../identities"); check(false, "path traversal id is refused") }
        catch let error as APIError { check(error.code == "calendar_id", "path traversal id is refused") }
        do { _ = try await client.calendarOccurrences(mailbox: "/k/mailbox_a", from: .min, to: .max); check(false, "overflowing window is refused") }
        catch let error as APIError { check(error.code == "window_too_large", "overflowing window is refused") }
        do { try await client.calendarReminders(mailbox: "/k/mailbox_a", id: "evt_fixture", reminders: [.init(minutesBefore: 5, channel: "external")]); check(false, "member reminders cannot target an external channel") }
        catch let error as APIError { check(error.code == "calendar_input", "member reminders cannot target an external channel") }
        check(CalendarTransport.snapshot().0.count == before, "invalid requests do not reach an authenticated route")
        let imported = Data("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".utf8)
        _ = try await client.previewCalendarImport(mailbox: "/k/mailbox_a", data: imported, calendar: "cal_a")
        let preview = CalendarTransport.snapshot()
        check(preview.0.last?.httpMethod == "POST" && preview.0.last?.value(forHTTPHeaderField: "content-type") == "text/calendar"
              && preview.1.last == imported, "import sends exactly the selected calendar bytes as text/calendar")
        check(preview.0.last?.url?.query == "calendar_id=cal_a", "import preview belongs to the chosen calendar")
        let exported = try await client.calendarICS(mailbox: "/k/mailbox_a", id: "evt_fixture")
        check(exported == imported && CalendarTransport.snapshot().0.last?.url?.path == "/v1/calendar/events/evt_fixture.ics", "export returns the caller's raw ICS projection")
        let settings = try await client.saveCalendarSettings(mailbox: "/k/mailbox_a", fields: ["previews": false])
        let settingsBody = try JSONSerialization.jsonObject(with: CalendarTransport.snapshot().1.last!) as! [String: Any]
        check(settings.previews == false && settingsBody["previews"] as? Bool == false, "preview opt-out is a JSON boolean")
        check(settingsBody.count == 1 && settingsBody["timezone"] == nil && settingsBody["working_hours"] == nil,
              "a privacy-only change does not overwrite unrelated calendar settings")
        _ = try await client.calendarFeed(mailbox: "/k/mailbox_a", id: "cal_a")
        check(CalendarTransport.snapshot().0.last?.url?.path == "/v1/calendar/feeds/cal_a", "feed reads use the public postbox route")
        _ = try await client.setCalendarFeed(mailbox: "/k/mailbox_a", id: "cal_a", enabled: false, projection: "busy")
        let feedCall = CalendarTransport.snapshot()
        let feedBody = try JSONSerialization.jsonObject(with: feedCall.1.last!) as! [String: Any]
        check(feedCall.0.last?.url?.path == "/v1/calendar/feeds/cal_a" && feedCall.0.last?.httpMethod == "POST" && feedBody["enabled"] as? Bool == false,
              "revoking a subscription POSTs enabled false to the public feed route")
        print("Calendar API: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }
}
