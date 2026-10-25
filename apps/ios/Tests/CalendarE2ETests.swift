import Foundation

@MainActor final class Session {
    let bearer: String
    init(_ bearer: String) { self.bearer = bearer }
    func token() async throws -> String { bearer }
    func renew() async throws -> String { bearer }
}
enum AuthError: Error { case sessionExpired }

struct CalendarE2EFixture: Decodable {
    let postbox: String
    let lossProxy: String?
    let people: [String: CalendarE2EPerson]
}
struct CalendarE2EPerson: Decodable { let token: String; let address: String }

@main
struct CalendarE2ETests {
    @MainActor static var owners: [Session] = []
    static var checks = 0
    static var failures = 0
    static func check(_ value: Bool, _ label: String) {
        checks += 1
        if value { print("  ok   " + label) } else { failures += 1; print("  FAIL " + label) }
    }
    @MainActor static func refused(_ code: String, _ label: String, _ operation: () async throws -> Void) async {
        do { try await operation(); check(false, label) }
        catch let error as APIError { check(error.code == code, label + " (" + (error.code ?? "unknown") + ")") }
        catch { check(false, label + " (transport failed)") }
    }
    private static func loopback(_ url: URL, origin: URL? = nil) -> Bool {
        url.scheme == "http" && url.host == "127.0.0.1" && url.port != nil && (origin == nil || url.port == origin?.port)
    }

    @MainActor static func main() async {
        guard let path = CommandLine.arguments.dropFirst().first,
              let data = FileManager.default.contents(atPath: path),
              let fixture = try? JSONDecoder().decode(CalendarE2EFixture.self, from: data),
              let base = URL(string: fixture.postbox), loopback(base),
              let proxyText = fixture.lossProxy, let proxy = URL(string: proxyText), loopback(proxy) else {
            print("A synthetic loopback postbox fixture is required."); exit(2)
        }
        func client(_ name: String, at url: URL? = nil) -> PostboxClient {
            let session = Session(fixture.people[name]!.token); owners.append(session)
            return PostboxClient(base: url ?? base, tokens: session)
        }
        let a = fixture.people["alice"]!.address, b = fixture.people["bob"]!.address
        let alice = client("alice"), bob = client("bob")
        let zone = TimeZone(identifier: "Europe/Istanbul")!
        do {
            let empty = try await alice.calendars(mailbox: a)
            check(empty.isEmpty, "calendar reads do not create a default calendar")
            let folder = try await alice.createCalendar(mailbox: a, name: "Work", color: "blue", requestKey: "native_folder")
            check(folder.revision != nil, "a real folder decodes its optimistic revision")
            for field in ["unknown_messages", "unnamed_messages"] {
                _ = try await bob.sendRaw("/v1/policy", method: "PUT", json: ["identity": b, field: "filter"])
            }
            var draft = CalendarEventDraft(day: try CalendarDay("2026-10-30"), zone: zone, calendarId: folder.id)
            draft.title = "Native review"; draft.rrule = "FREQ=DAILY;COUNT=3"
            draft.meeting.enabled = true; draft.meeting.transport = "sfu"
            var member = CalendarInviteeDraft(); member.destination = b; member.cohost = true
            var guest = CalendarInviteeDraft(); guest.destination = "private-guest@calendar.test"; guest.name = "Guest"
            draft.invitees = [member, guest]
            var fields = try draft.fields(); fields["request_key"] = "native_create"
            let payload = try CalendarWritePayload(fields)
            let created = try await alice.createCalendarEvent(mailbox: a, payload: payload)
            let retried = try await alice.createCalendarEvent(mailbox: a, payload: payload)
            check(created.event.id == retried.event.id && created.event.revision == retried.event.revision, "an identical real create retry returns the original event")
            check(created.event.meeting?.code?.count == 10 && created.event.meeting?.transport == "sfu", "a scheduled30-person call decodes its locator and transport")
            let id = created.event.id
            let from = Int(try CalendarWallTime("2026-10-29T00:00", tzid: zone.identifier).resolve().timeIntervalSince1970)
            let to = Int(try CalendarWallTime("2026-11-04T00:00", tzid: zone.identifier).resolve().timeIntervalSince1970)
            var listed = try await alice.calendarOccurrences(mailbox: a, from: from, to: to, calendars: [folder.id]).filter { $0.eventId == id }
            check(listed.count == 3, "the server expands the recurrence into three native occurrences")
            let first = listed.sorted { ($0.startsAt ?? 0) < ($1.startsAt ?? 0) }[0]
            let key = CalendarEventKey(eventId: id, occurrenceId: first.occurrenceId)
            let detail = try await alice.calendarEvent(mailbox: a, id: id, occurrenceId: key.occurrenceId)
            check(detail.occurrence?.start == "2026-10-30T09:00" && detail.occurrence?.occurrenceId == first.occurrenceId, "detail keeps the authored wall time and original occurrence id")
            let invited = try await bob.calendarInvitations(mailbox: b, filtered: false)
            let filtered = try await bob.calendarInvitations(mailbox: b, filtered: true)
            check(filtered.contains { $0.eventId == id }, "the recipient's explicit stranger policy places the invitation in Filtered")
            check(!invited.contains { $0.eventId == id } || !filtered.contains { $0.eventId == id }, "pending and filtered invitation lists are disjoint")
            let hidden = try await bob.calendarOccurrences(mailbox: b, from: from, to: to)
            check(!hidden.contains { $0.eventId == id }, "filtered invitations do not enter the native agenda")
            let bobView = try await bob.calendarEvent(mailbox: b, id: id, occurrenceId: first.occurrenceId)
            check(bobView.event.canEdit == false && bobView.event.my?.role == "cohost", "a co-host does not acquire organizer editing rights")
            check(bobView.event.attendees?.contains { $0.kind == "external" && $0.contact != nil } == false, "member projections never expose the contact guest's address")
            let contactsBefore = try await bob.contacts(identity: b).contacts ?? []
            try await bob.calendarRSVP(mailbox: b, id: id, status: .accepted, scope: .this, occurrenceId: first.occurrenceId, note: "I can join")
            let accepted = try await bob.calendarEvent(mailbox: b, id: id, occurrenceId: first.occurrenceId)
            check(accepted.occurrence?.myRsvp == "accepted", "an occurrence-only response reaches the native detail")
            let afterAnswer = try await bob.calendarInvitations(mailbox: b, filtered: true)
            check(!afterAnswer.contains { $0.eventId == id }, "answering a filtered invitation removes it from Filtered")
            let contactsAfter = try await bob.contacts(identity: b).contacts ?? []
            check(contactsBefore.count == contactsAfter.count, "answering an invitation does not create a contact")
            await refused("forbidden", "an attendee cannot modify the organizer's event") {
                _ = try await bob.updateCalendarEvent(mailbox: b, id: id, payload: CalendarWritePayload(["expected_revision": accepted.event.revision!, "scope": "all", "title": "Unauthorized"]))
            }
            let privateGuest = detail.event.attendees!.first { $0.kind == "external" }!
            let personalLink = try await alice.calendarGuestLink(mailbox: a, id: id, attendee: privateGuest.id)
            check(URL(string: personalLink.url)?.fragment?.isEmpty == false, "automatic delivery being off still permits the organizer's personal guest link")
            await refused("not_found", "a co-host cannot mint or discover another guest's personal link") {
                _ = try await bob.calendarGuestLink(mailbox: b, id: id, attendee: privateGuest.id)
            }
            let ics = try await bob.calendarICS(mailbox: b, id: id)
            check(!String(decoding: ics, as: UTF8.self).contains("private-guest@calendar.test"), "native ICS export does not reveal another attendee's contact address")

            let latest = try await alice.calendarEvent(mailbox: a, id: id, occurrenceId: first.occurrenceId)
            var edit = try CalendarEventDraft(target: .init(response: latest, key: key, scope: .this), fallbackZone: zone)
            edit.startDay = try CalendarDay("2026-11-02"); edit.lastDay = edit.startDay; edit.startTime = "14:30"; edit.endTime = "15:30"
            var changed = try edit.fields(); changed["expected_revision"] = latest.event.revision!; changed["scope"] = "this"; changed["occurrence_id"] = first.occurrenceId
            let moved = try await alice.updateCalendarEvent(mailbox: a, id: id, payload: CalendarWritePayload(changed))
            check(moved.occurrence?.occurrenceId == first.occurrenceId && moved.occurrence?.start == "2026-11-02T14:30", "a real moved occurrence retains its opaque id")
            await refused("revision_conflict", "a captured older revision cannot overwrite the moved event") {
                _ = try await alice.updateCalendarEvent(mailbox: a, id: id, payload: CalendarWritePayload(["expected_revision": latest.event.revision!, "scope": "all", "title": "Stale"]))
            }
            let removedStore = CalendarStore(client: bob)
            removedStore.configure(mailbox: b, enabled: true, emailEnabled: false)
            _ = await removedStore.loadEvent(key)
            check(removedStore.details[key] != nil, "the member's native cache contains the authorized event before removal")
            try await alice.calendarAttendees(mailbox: a, id: id, revision: moved.event.revision!, scope: .all, occurrenceId: nil, adding: [], removing: [bobView.event.my!.attendeeId!])
            _ = await removedStore.loadEvent(key)
            check(removedStore.details.isEmpty, "a removed attendee cannot retain a fetched event in the native cache")
            try await alice.calendarReminders(mailbox: a, id: id, reminders: [.init(minutesBefore: 5, channel: "push"), .init(minutesBefore: 60, channel: "push")])
            let reminded = try await alice.calendarEvent(mailbox: a, id: id)
            check(Set((reminded.event.reminders ?? []).map(\.minutesBefore)) == [5, 60], "multiple native reminders survive a real write/read round trip")
            let initialSettings = try await alice.calendarSettings(mailbox: a)
            _ = try await alice.saveCalendarSettings(mailbox: a, fields: ["timezone": zone.identifier])
            let privateSettings = try await alice.saveCalendarSettings(mailbox: a, fields: ["previews": false])
            check(privateSettings.previews == false && privateSettings.timezone == zone.identifier && privateSettings.weekStart == initialSettings.weekStart, "a privacy-only settings save preserves timezone and week start on the server")
            let feed = try await alice.setCalendarFeed(mailbox: a, id: folder.id, enabled: true, projection: "busy")
            let feedRead = try await alice.calendarFeed(mailbox: a, id: folder.id)
            check(feedRead.enabled == true && feedRead.url == nil, "subscription links are shown once and not returned by later native reads")
            guard let feedText = feed.url, let feedURL = URL(string: feedText), loopback(feedURL, origin: base) else { throw APIError(status: 0, code: "fixture_feed_origin", detail: nil) }
            let http = URLSession(configuration: .ephemeral)
            let (feedBytes, feedResponse) = try await http.data(from: feedURL)
            let exported = String(decoding: feedBytes, as: UTF8.self)
            check((feedResponse as? HTTPURLResponse)?.statusCode == 200 && exported.contains("SUMMARY:Busy") && !exported.contains("Native review"), "the busy subscription hides titles from a real bearer-link request")
            _ = try await alice.setCalendarFeed(mailbox: a, id: folder.id, enabled: false, projection: "busy")
            let (_, revoked) = try await http.data(from: feedURL)
            check((revoked as? HTTPURLResponse)?.statusCode != 200, "revoking a feed invalidates the previously copied link")
            let rawICS = Data("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:external-example\r\nDTSTAMP:20261025T000000Z\r\nDTSTART;VALUE=DATE:20261101\r\nDTEND;VALUE=DATE:20261102\r\nSUMMARY:Imported private copy\r\nATTENDEE:mailto:never-send@calendar.test\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n".utf8)
            let preview = try await alice.previewCalendarImport(mailbox: a, data: rawICS, calendar: folder.id)
            check(preview.events?.count == 1, "native import previews real ICS without sending invitations")
            let imported = try await alice.confirmCalendarImport(mailbox: a, draft: preview.draftId, selected: [0], calendar: folder.id)
            check(imported.created?.count == 1, "import confirms the selected private copy")
            let privateCopy = try await alice.calendarEvent(mailbox: a, id: imported.created!.first!.id)
            check(privateCopy.event.attendees?.isEmpty == true, "a fetched imported event has no imported attendees")

            let lossClient = client("alice", at: proxy)
            let lossStore = CalendarStore(client: lossClient)
            lossStore.configure(mailbox: a, enabled: true, emailEnabled: false)
            var uncertain = CalendarEventDraft(day: try CalendarDay("2026-10-31"), zone: zone, calendarId: folder.id)
            uncertain.title = "Response lost after commit"
            var control = URLRequest(url: proxy.appendingPathComponent("__test/control")); control.httpMethod = "POST"
            control.setValue("application/json", forHTTPHeaderField: "content-type"); control.httpBody = Data(#"{"mode":"after"}"#.utf8)
            _ = try await http.data(for: control)
            let lost = try await lossStore.createEvent(fields: uncertain.fields())
            check(lost == nil, "a dropped response after commit leaves the native create unconfirmed")
            control.httpBody = Data(#"{"mode":"off"}"#.utf8); _ = try await http.data(for: control)
            let recovered = try await lossStore.createEvent(fields: uncertain.fields())
            check(recovered != nil, "an explicit retry recovers the committed event")
            listed = try await alice.calendarOccurrences(mailbox: a, from: from, to: to)
            check(listed.filter { $0.title == uncertain.title }.count == 1, "a real response-loss retry creates exactly one calendar event")
            let (attemptData, _) = try await http.data(from: proxy.appendingPathComponent("__test/attempts"))
            let evidence = try JSONSerialization.jsonObject(with: attemptData) as! [String: [[String: Any]]]
            let attempts = evidence["attempts"] ?? []
            check(attempts.count >= 2 && Set(attempts.compactMap { $0["sha"] as? String }).count == 1 && Set(attempts.compactMap { $0["requestKey"] as? String }).count == 1,
                  "all real loss-proxy attempts retain exact body bytes and request identity")
            http.invalidateAndCancel()
        } catch let error as APIError { check(false, "real postbox error: " + String(error.status) + " " + (error.code ?? "unknown")) }
        catch { check(false, "unexpected real postbox failure: " + String(describing: type(of: error))) }
        print("Calendar end to end: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }
}
