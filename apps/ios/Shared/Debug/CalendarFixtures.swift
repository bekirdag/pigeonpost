#if DEBUG
import Foundation

/// Calendar interaction tests use the real API/store against this in-process transport. It only
/// handles a reserved .invalid origin and cannot send fixture invitations to a production server.
@MainActor
enum CalendarFixtures {
    private static let tokens = CalendarFixtureTokens()
    static func client(account: Account) -> PostboxClient {
        guard Fixtures.enabled else { return account.client }
        URLProtocol.registerClass(CalendarFixtureTransport.self)
        return PostboxClient(base: URL(string: "https://calendar-fixture.invalid")!, tokens: tokens)
    }
    static func now() -> Date {
        Fixtures.enabled ? ISO8601DateFormatter().date(from: "2026-10-25T12:00:00Z")! : Date()
    }
}

@MainActor
private final class CalendarFixtureTokens: TokenProviding {
    func token() async throws -> String { "calendar-local-fixture" }
    func renew() async throws -> String { "calendar-local-fixture" }
}

private final class CalendarFixtureTransport: URLProtocol {
    private struct Box {
        var settings: [String: Any] = ["previews": false, "timezone": "Europe/Istanbul", "week_start": 1,
            "working_hours": ["start": "09:00", "end": "18:00", "days": [1, 2, 3, 4, 5]]]
        var folders: [[String: Any]] = [["id": "cal_work", "name": "Work", "color": "blue", "is_default": true, "revision": 1],
                                      ["id": "cal_personal", "name": "Personal", "color": "green", "is_default": false, "revision": 1]]
        var events: [String: [String: Any]] = [:]
        var overrides: [String: [String: Any]] = [:]
        var creates: [String: (Data, [String: Any])] = [:]
        var next = 1
    }
    private static let lock = NSLock()
    private static var boxes: [String: Box] = [:]
    private static var conflict = CommandLine.arguments.contains("-calendar-conflict-once")
    private static var loseCreate = CommandLine.arguments.contains("-calendar-create-loss-once")

    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "calendar-fixture.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        var bytes = request.httpBody ?? Data()
        if bytes.isEmpty, let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }; bytes.append(contentsOf: buffer.prefix(count))
            }
        }
        let mailbox = request.value(forHTTPHeaderField: "x-pigeonpost-identity") ?? ""
        Self.lock.lock()
        var box = Self.boxes[mailbox] ?? Self.seed(mailbox)
        let reply = Self.route(request, bytes: bytes, mailbox: mailbox, box: &box)
        Self.boxes[mailbox] = box
        Self.lock.unlock()
        let response = HTTPURLResponse(url: request.url!, statusCode: reply.0, httpVersion: nil, headerFields: ["content-type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: try! JSONSerialization.data(withJSONObject: reply.1))
        client?.urlProtocolDidFinishLoading(self)
    }

    private static func seed(_ mailbox: String) -> Box {
        var box = Box()
        func make(_ id: String, _ title: String, invited: Bool = false, filtered: Bool = false) -> [String: Any] {
            ["id": id, "title": title, "description": "Agenda and notes", "location": "Room 2", "revision": 1, "sequence": 1,
             "calendar_id": "cal_work", "all_day": false, "start": "2026-10-25T09:00:00", "end": "2026-10-25T10:00:00", "tzid": "Europe/Istanbul",
             "status": "confirmed", "rrule": "FREQ=WEEKLY;COUNT=6", "locale": "tr", "can_edit": !invited,
             "organizer": ["handle": invited ? "/fixture/organizer" : mailbox],
             "my": invited ? ["role": "attendee", "attendee_id": "att_self", "rsvp": "needs_action"] : ["role": "organizer"],
             "attendees": invited ? [["attendee_id": "att_guest", "kind": "external", "name": "Guest", "rsvp": "accepted"]] : [],
             "reminders": [["minutes_before": 10, "channel": "push"]], "_invited": invited, "_filtered": filtered]
        }
        box.events["evt_review"] = make("evt_review", "Design review")
        box.events["evt_invited"] = make("evt_invited", "Team invitation", invited: true)
        box.events["evt_filtered"] = make("evt_filtered", "Filtered invitation", invited: true, filtered: true)
        return box
    }
    private static func timing(_ event: [String: Any]) -> [String: Any] {
        guard event["all_day"] as? Bool != true else { return [:] }
        let zone = event["tzid"] as? String ?? "UTC"
        let fold = (event["fold"] as? String).flatMap(CalendarFold.init(rawValue:))
        let first = (event["start"] as? String).flatMap { try? CalendarWallTime($0, tzid: zone).resolve(fold: fold) }
        let last = (event["end"] as? String).flatMap { try? CalendarWallTime($0, tzid: zone).resolve(fold: fold) }
        var result: [String: Any] = [:]
        if let first { result["starts_at"] = Int(first.timeIntervalSince1970) }
        if let last { result["ends_at"] = Int(last.timeIntervalSince1970) }
        return result
    }
    private static func occurrence(_ event: [String: Any], box: Box) -> [String: Any] {
        let id = event["id"] as! String
        var item = event
        for (key, value) in box.overrides[id] ?? [:] { item[key] = value }
        for (key, value) in timing(item) { item[key] = value }
        item["event_id"] = id; item["occurrence_id"] = "occ_" + id + "_original"
        item["invited"] = item["_invited"]
        item["my_rsvp"] = (item["my"] as? [String: Any])?["rsvp"]
        return item
    }

    private static func route(_ request: URLRequest, bytes: Data, mailbox: String, box: inout Box) -> (Int, [String: Any]) {
        let path = request.url!.path, method = request.httpMethod ?? "GET"
        let body = (try? JSONSerialization.jsonObject(with: bytes)) as? [String: Any] ?? [:]
        let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
        func q(_ name: String) -> String? { query.first { $0.name == name }?.value }
        if path == "/v1/calendars", method == "GET" { return (200, ["calendars": box.folders]) }
        if path == "/v1/calendars", method == "POST" {
            let folder: [String: Any] = ["id": "cal_new_\(box.next)", "name": body["name"] ?? "Calendar", "color": body["color"] ?? "blue", "is_default": false, "revision": 1]
            box.next += 1; box.folders.append(folder); return (201, ["calendar": folder])
        }
        if path.hasPrefix("/v1/calendars/"), let index = box.folders.firstIndex(where: { $0["id"] as? String == path.components(separatedBy: "/").last }) {
            guard body["expected_revision"] as? Int == box.folders[index]["revision"] as? Int else { return (409, ["error": "revision_conflict"]) }
            if method == "DELETE" { box.folders.remove(at: index); return (200, ["ok": true]) }
            for (key, value) in body where key != "expected_revision" { box.folders[index][key] = value }
            box.folders[index]["revision"] = (box.folders[index]["revision"] as? Int ?? 1) + 1
            return (200, ["calendar": box.folders[index]])
        }
        if path == "/v1/calendar/settings" {
            if method == "PUT" { for (key, value) in body { box.settings[key] = value } }
            return (200, box.settings)
        }
        if path == "/v1/calendar/invitations" {
            let filtered = q("state") == "filtered"
            let items = box.events.values.filter {
                $0["_invited"] as? Bool == true && ($0["_filtered"] as? Bool ?? false) == filtered && ($0["my"] as? [String: Any])?["rsvp"] as? String == "needs_action"
            }.map { event -> [String: Any] in
                var item = occurrence(event, box: box); item["meeting"] = false; return item
            }
            return (200, ["invitations": items])
        }
        if path == "/v1/calendar/events", method == "GET" {
            let selected = q("calendars").map { Set($0.components(separatedBy: ",")) }
            let items = box.events.values.filter { $0["_filtered"] as? Bool != true && (selected == nil || selected!.contains($0["calendar_id"] as? String ?? "")) }.map { occurrence($0, box: box) }
            return (200, ["occurrences": items])
        }
        if path == "/v1/calendar/events", method == "POST" {
            let requestKey = body["request_key"] as? String ?? ""
            if let cached = box.creates[requestKey] { return cached.0 == bytes ? (201, cached.1) : (409, ["error": "idempotency_conflict"]) }
            var event = body; let id = "evt_new_\(box.next)"; box.next += 1
            event["id"] = id; event["revision"] = 1; event["can_edit"] = true; event["status"] = "confirmed"
            event["calendar_id"] = body["calendar_id"] ?? "cal_work"; event["my"] = ["role": "organizer"]
            event["attendees"] = []; box.events[id] = event
            let reply: [String: Any] = ["event": event.merging(timing(event)) { _, new in new }]
            box.creates[requestKey] = (bytes, reply)
            if loseCreate { loseCreate = false; return (503, ["error": "unavailable"]) }
            return (201, reply)
        }
        let parts = path.components(separatedBy: "/")
        guard parts.count >= 5, parts[1...3].joined(separator: "/") == "v1/calendar/events", var event = box.events[parts[4]] else { return (404, ["error": "not_found"]) }
        let id = parts[4], suffix = parts.count > 5 ? parts[5] : ""
        if method == "GET" {
            var result: [String: Any] = ["event": event.merging(timing(event)) { _, new in new }]
            if q("occurrence_id") != nil { result["occurrence"] = occurrence(event, box: box) }
            return (200, result)
        }
        if suffix == "rsvp" {
            var mine = event["my"] as? [String: Any] ?? [:]; mine["rsvp"] = body["status"]; event["my"] = mine; event["_filtered"] = false
        } else if suffix == "reminders" { event["reminders"] = body["reminders"] }
        else {
            if conflict {
                conflict = false; event["revision"] = (event["revision"] as? Int ?? 1) + 1; event["title"] = "Updated on another device"; box.events[id] = event
                return (409, ["error": "revision_conflict"])
            }
            guard body["expected_revision"] as? Int == event["revision"] as? Int else { return (409, ["error": "revision_conflict"]) }
            if suffix == "cancel" {
                if body["scope"] as? String == "this" { box.overrides[id, default: [:]]["status"] = "cancelled" }
                else { event["status"] = "cancelled" }
            } else if body["scope"] as? String == "this" {
                for (key, value) in body where !["expected_revision", "scope", "occurrence_id"].contains(key) { box.overrides[id, default: [:]][key] = value }
            } else {
                for (key, value) in body where !["expected_revision", "scope", "occurrence_id"].contains(key) { event[key] = value }
            }
        }
        event["revision"] = (event["revision"] as? Int ?? 1) + 1; box.events[id] = event
        return (200, ["event": event.merging(timing(event)) { _, new in new }, "occurrence": occurrence(event, box: box)])
    }
}
#endif
