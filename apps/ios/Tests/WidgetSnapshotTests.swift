//  The widget snapshot: what the app may hand the Home Screen, and the encrypted text it must not.
//  Run by Tests/run.sh, compiled with swiftc like the other model tests.

import Foundation

@main
struct WidgetSnapshotTests {
    static var failures = 0

    static func check(_ condition: @autoclosure () -> Bool, _ message: String, line: Int = #line) {
        if !condition() {
            failures += 1
            print("FAIL (line \(line)): \(message)")
        }
    }

    static func message(_ id: String, _ kind: ThreadMessage.Kind, _ body: String, at: Int, read: Bool = true) -> ThreadMessage {
        var row = ThreadMessage(id: id, kind: kind, at: at, body: body, threadId: nil)
        row.read = read
        return row
    }

    static func main() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let secret = "The launch code is 0000. Do not share."

        // 1. Redaction in the builder: no message text unless previews are on.
        var ada = Conversation(peer: "/ada")
        ada.messages = [message("m1", .incoming, secret, at: 1_799_999_000, read: false)]
        ada.unread = 1
        ada.last = 1_799_999_000
        var emre = Conversation(peer: "/emre")
        emre.messages = [message("m2", .outgoing, "See you\nat four", at: 1_799_990_000)]
        emre.last = 1_799_990_000
        var gone = Conversation(peer: "/archived")
        gone.last = 1_799_999_500
        gone.unread = 4

        let hidden = WidgetSnapshotBuilder.conversations([emre, ada, gone], archived: ["/archived"], previews: false)
        check(hidden.map(\.peer) == ["/ada", "/emre"], "archived dropped, newest first: \(hidden.map(\.peer))")
        check(hidden.allSatisfy { $0.preview == nil }, "previews off carries no preview")
        check(hidden.first?.unread == 1, "unread kept")

        let shown = WidgetSnapshotBuilder.conversations([emre, ada], archived: [], previews: true)
        check(shown.first?.preview == secret, "previews on carries the text")
        check(shown.last?.preview == "You: See you at four", "outgoing preview is one line and marked: \(shown.last?.preview ?? "nil")")

        // 2. The codec enforces it too, whatever the snapshot was built with.
        var snapshot = WidgetFixtures.snapshot(now: now, previews: true)
        check(snapshot.conversations.contains { $0.preview != nil }, "fixture with previews has some")
        snapshot.showsPreviews = false
        let data = try! WidgetCodec.encode(snapshot)
        let text = String(decoding: data, as: UTF8.self)
        check(!text.contains("Signed the offer letter"), "encoded snapshot with previews off holds no message text")
        check(!text.contains("\"preview\""), "no preview key at all when off")
        let decoded = WidgetCodec.decode(data)
        check(decoded != nil, "round trip decodes")
        check(decoded?.conversations.allSatisfy { $0.preview == nil } == true, "decoded has no preview")
        check(decoded?.unreadCount == snapshot.unreadCount, "unread count survives")
        check(decoded?.conversations.map(\.peer) == snapshot.conversations.map(\.peer), "conversations survive in order")
        check(decoded?.nests == snapshot.nests, "nests survive")
        check(decoded?.agenda == snapshot.agenda, "agenda survives")

        // A file written with previews off but carrying one (tampered, or a bug) is redacted on read.
        var tampered = WidgetFixtures.snapshot(now: now, previews: false)
        tampered.conversations[0].preview = secret
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .secondsSince1970
        let raw = try! encoder.encode(tampered)
        check(String(decoding: raw, as: UTF8.self).contains("launch code"), "test setup: raw bytes carry the secret")
        check(WidgetCodec.decode(raw)?.conversations[0].preview == nil, "decoder drops a preview when previews are off")

        // With previews on: kept, but one line and bounded.
        var long = WidgetFixtures.snapshot(now: now, previews: true)
        long.conversations[0].preview = String(repeating: "word ", count: 100) + "\nsecond line"
        let bounded = WidgetCodec.decode(try! WidgetCodec.encode(long))?.conversations[0].preview ?? ""
        check(bounded.count == WidgetSnapshot.previewLimit, "preview cut to the limit: \(bounded.count)")
        check(!bounded.contains("\n"), "preview is one line")

        // 3. Versions and garbage.
        var future = WidgetFixtures.snapshot(now: now)
        future.version = WidgetSnapshot.currentVersion + 1
        check(WidgetCodec.decode(try! encoder.encode(future)) == nil, "a newer version is refused")
        check(WidgetCodec.decode(Data("not json".utf8)) == nil, "garbage is refused")
        check(WidgetCodec.decode(Data("{}".utf8)) == nil, "an empty object is refused")

        // 4. Nothing secret-shaped in the format: no token, key or subject fields exist.
        let keys = Set(((try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]).keys)
        check(keys == Set(["version", "generatedAt", "owner", "mailbox", "showsPreviews", "unreadCount", "conversations", "nests", "agenda"]),
              "top-level keys are exactly the minimal set: \(keys.sorted())")
        let owner = WidgetSnapshotBuilder.owner(subject: "f3c1-subject")
        check(owner.count == 24 && !owner.contains("f3c1"), "owner is a fingerprint, not the subject")
        check(owner == WidgetSnapshotBuilder.owner(subject: "f3c1-subject"), "owner is stable")
        check(owner != WidgetSnapshotBuilder.owner(subject: "other"), "owner differs per account")

        // 5. Limits.
        var many = WidgetFixtures.snapshot(now: now)
        many.conversations = (0..<20).map { WidgetConversation(peer: "/p\($0)", name: "P\($0)", lastAt: now, unread: -1, preview: nil) }
        let cut = many.redacted()
        check(cut.conversations.count == WidgetSnapshot.conversationLimit, "conversations capped")
        check(cut.conversations.allSatisfy { $0.unread == 0 }, "negative unread clamped")

        // 6. The store: write, read, clear.
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("widget-tests-\(UUID().uuidString)")
        let url = WidgetStore.url(inContainer: folder)
        try! WidgetStore.write(snapshot, to: url)
        check(WidgetStore.read(from: url)?.owner == snapshot.owner, "store reads back what it wrote")
        check(WidgetStore.read(from: url)?.conversations.allSatisfy { $0.preview == nil } == true, "store redacts")
        WidgetStore.clear(at: url)
        check(WidgetStore.read(from: url) == nil, "clear removes the file")
        try? FileManager.default.removeItem(at: folder)

        // 7. Links.
        let routes: [WidgetRoute] = [.inbox, .compose, .search, .nests, .getNest, .calendar,
                                     .conversation(mailbox: "/k/ab+cd=", peer: "/bekir/agent1"),
                                     .event(mailbox: "/k/x", eventId: "ev 1", occurrenceId: "o&2"),
                                     .event(mailbox: "/k/x", eventId: "ev", occurrenceId: nil)]
        for route in routes {
            check(WidgetRoute(url: route.url) == route, "round trip \(route) via \(route.url)")
        }
        check(WidgetRoute(url: URL(string: "dev.pigeonpost.inbox://oauth2redirect?code=1")!) == nil, "the OAuth redirect is not a widget link")
        check(WidgetRoute(url: URL(string: "https://widget/inbox")!) == nil, "another scheme is not a widget link")
        check(WidgetRoute(url: URL(string: "dev.pigeonpost.inbox://widget/conversation?peer=/ada")!) == nil, "a conversation link needs its mailbox")
        check(WidgetRoute(url: URL(string: "dev.pigeonpost.inbox://widget/settings")!) == nil, "unknown routes are refused")

        // 8. Agenda: ended events drop, all-day spans the day, cancelled are left out.
        let zone = TimeZone(identifier: "Europe/Istanbul")!
        func occurrence(_ id: String, start: Int?, end: Int?, allDay: Bool = false, startDate: String? = nil, endDate: String? = nil,
                        status: String? = nil) -> EventOccurrence {
            let json: [String: Any?] = ["occurrence_id": id, "event_id": id, "starts_at": start, "ends_at": end, "all_day": allDay,
                                        "start_date": startDate, "end_date": endDate, "title": "T " + id, "status": status]
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            return try! decoder.decode(EventOccurrence.self, from: JSONSerialization.data(withJSONObject: json.compactMapValues { $0 }))
        }
        let t = Int(now.timeIntervalSince1970)
        let agenda = WidgetSnapshotBuilder.agenda([
            occurrence("past", start: t - 7200, end: t - 3600),
            occurrence("later", start: t + 7200, end: t + 9000),
            occurrence("soon", start: t + 600, end: t + 1800),
            occurrence("cancelled", start: t + 60, end: t + 120, status: "cancelled"),
            occurrence("day", start: nil, end: nil, allDay: true, startDate: "2027-01-15", endDate: "2027-01-16"),
        ], now: now, zone: zone)
        // The all-day event began at midnight in Istanbul and is still running: it comes first.
        check(agenda.events.map(\.eventId) == ["day", "soon", "later"], "agenda order and filtering: \(agenda.events.map(\.eventId))")
        check(agenda.nextChange(after: now) == Date(timeIntervalSince1970: TimeInterval(t + 600)), "redraws when the next event starts")
        if let day = agenda.events.first(where: { $0.eventId == "day" }) {
            check(day.allDay && day.endsAt.timeIntervalSince(day.startsAt) == 86_400, "all-day spans one day")
        }

        // 9. Nests: running-hours meter, setups, unavailable and not-yet-loaded.
        let usage = NestUsage(period: "2027-01", inputTokens: .init(used: 0, limit: 0), outputTokens: .init(used: 0, limit: 0),
                              runningSecs: 36_000, filesBytes: 0, package: .init(code: "starter", activeHoursPooled: 40))
        let view = NestView(nestId: "n1", address: "/k/n1", label: "Research", state: "ready", funding: "package", status: "ready",
                            statusReason: nil, pausedReason: nil, deleteAfter: nil)
        let nestSnapshot = NestSnapshot(allowance: NestAllowance(funding: nil, slots: 2, used: 1, duplicates: []), package: nil,
                                        nests: [view], setups: [NestSetup(setupId: "s1", label: "New one", status: "pending", attempts: 1, lastError: nil)])
        let nests = WidgetSnapshotBuilder.nests(nestSnapshot, available: true, usage: ["n1": usage])
        check(nests?.items.count == 2, "a Nest and a setup")
        check(nests?.items.first?.runningHours == 10 && nests?.items.first?.hoursLimit == 40, "meter from usage")
        check(nests?.items.first?.meterFraction == 0.25, "meter fraction")
        check(nests?.items.last?.tone == .setup, "setup tone")
        check(WidgetSnapshotBuilder.nests(nil, available: true, usage: [:]) == nil, "not loaded is nil, not empty")
        check(WidgetSnapshotBuilder.nests(nil, available: false, usage: [:]) == WidgetNests(available: false, items: []), "unavailable")

        if failures > 0 {
            print("widget snapshot: \(failures) failure(s)")
            exit(1)
        }
        print("widget snapshot: ok")
    }
}
