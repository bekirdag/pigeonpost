import Foundation

@MainActor final class Session {
    var pauseToken = false, pauseRenewal = false
    var tokenWaiter: CheckedContinuation<String, Never>?
    var renewalWaiter: CheckedContinuation<String, Never>?
    var renewals = 0
    func token() async throws -> String {
        if pauseToken { return await withCheckedContinuation { tokenWaiter = $0 } }
        return "calendar-store-fixture"
    }
    func renew() async throws -> String {
        renewals += 1
        if pauseRenewal { return await withCheckedContinuation { renewalWaiter = $0 } }
        return "calendar-store-fixture"
    }
}
enum AuthError: Error { case sessionExpired }

final class CalendarGate: URLProtocol {
    struct Entry {
        let mailbox: String
        let method: String
        let path: String
        let query: String
        let body: Data
        var key: String { CalendarGate.key(mailbox, method, path, query) }
    }
    static let lock = NSLock()
    static var held: Set<String> = []
    static var waiting: [CalendarGate] = []
    static var entries: [Entry] = []
    static var failCreateOnce = false
    static var rejectCreates = false
    static var conflict = false
    static var missingSeries = false
    private var entry: Entry!

    static func key(_ mailbox: String, _ method: String, _ path: String, _ query: String = "") -> String {
        [mailbox, method, path, query].joined(separator: "|")
    }
    static func hold(_ key: String) { lock.lock(); held.insert(key); lock.unlock() }
    static func history() -> [Entry] { lock.lock(); defer { lock.unlock() }; return entries }
    static func waitingFor(_ key: String) -> Bool { lock.lock(); defer { lock.unlock() }; return waiting.contains { $0.entry.key == key } }
    static func waitingCount(_ key: String) -> Int { lock.lock(); defer { lock.unlock() }; return waiting.filter { $0.entry.key == key }.count }
    static func releaseOne(_ key: String, status: Int = 200) {
        lock.lock()
        let index = waiting.firstIndex { $0.entry.key == key }
        let item = index.map { waiting.remove(at: $0) }
        lock.unlock()
        if status == 200 { item?.respond() } else { item?.reject(status) }
    }
    static func release(_ key: String) {
        lock.lock()
        held.remove(key)
        let pending = waiting.filter { $0.entry.key == key }
        waiting.removeAll { $0.entry.key == key }
        lock.unlock()
        for item in pending { item.respond() }
    }
    static func flags(failCreateOnce: Bool = false, rejectCreates: Bool = false, conflict: Bool = false, missingSeries: Bool = false) {
        lock.lock(); defer { lock.unlock() }
        self.failCreateOnce = failCreateOnce; self.rejectCreates = rejectCreates; self.conflict = conflict; self.missingSeries = missingSeries
    }

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
        entry = Entry(mailbox: request.value(forHTTPHeaderField: "x-pigeonpost-identity") ?? "", method: request.httpMethod ?? "GET",
                      path: request.url!.path, query: request.url!.query ?? "", body: body)
        Self.lock.lock()
        Self.entries.append(entry)
        if Self.held.contains(entry.key) { Self.waiting.append(self); Self.lock.unlock(); return }
        Self.lock.unlock()
        respond()
    }

    private func respond() {
        let e = entry!
        Self.lock.lock()
        var status = 200
        if e.method == "POST" && e.path == "/v1/calendar/events" {
            if Self.rejectCreates { status = 400 }
            else if Self.failCreateOnce { status = 503; Self.failCreateOnce = false }
        }
        if e.method == "PATCH" && Self.conflict { status = 409 }
        if e.method == "GET" && e.path == "/v1/calendar/events/evt_shared" && e.query.isEmpty && Self.missingSeries { status = 404 }
        Self.lock.unlock()
        let data: [String: Any]
        let suffix = e.mailbox.hasSuffix("a") ? "a" : "b"
        if status != 200 { data = ["error": status == 409 ? "revision_conflict" : status == 404 ? "not_found" : "bad_request"] }
        else if e.path == "/v1/calendars" { data = ["calendars": [["id": "cal_" + suffix, "name": e.mailbox, "revision": 1]]] }
        else if e.path == "/v1/calendar/settings" { data = ["previews": false, "timezone": "UTC", "week_start": 1] }
        else if e.path == "/v1/calendar/invitations" { data = ["invitations": [["event_id": "evt_inv_" + suffix, "title": e.mailbox]]] }
        else if e.path == "/v1/calendar/feeds/cal_shared" {
            // Deliberately include a URL even on GET: the store must discard it on a status read.
            data = ["enabled": true, "projection": "busy", "url": "https://calendar.test/v1/calendar/feed/synthetic-" + suffix + ".ics"]
        }
        else if e.path == "/v1/calendar/import/preview" {
            data = ["draft_id": "draft_shared", "calendar_id": "cal_" + suffix,
                    "events": [["index": 0, "title": "Private copy", "importable": true], ["index": 1, "importable": false]]]
        }
        else if e.path == "/v1/calendar/import/draft_shared/confirm" { data = ["created": [["id": "evt_imported", "title": "Private copy"]]] }
        else if e.method == "GET" && e.path == "/v1/calendar/events" {
            data = ["occurrences": [["occurrence_id": "occ_" + suffix, "event_id": "evt_shared", "title": e.mailbox]]]
        } else { data = ["event": ["id": "evt_shared", "title": e.mailbox, "revision": 3, "can_edit": true]] }
        let isICS = status == 200 && e.path.hasSuffix(".ics")
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil,
                                       headerFields: ["Content-Type": isICS ? "text/calendar" : "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: isICS ? Data("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".utf8) : try! JSONSerialization.data(withJSONObject: data))
        client?.urlProtocolDidFinishLoading(self)
    }

    private func reject(_ status: Int) {
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil,
                                       headerFields: ["Content-Type": "application/json"])!
        let code = status == 401 ? "unauthorized" : status == 403 ? "forbidden" : "unavailable"
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: try! JSONSerialization.data(withJSONObject: ["error": code]))
        client?.urlProtocolDidFinishLoading(self)
    }
}

@main
struct CalendarStoreTests {
    @MainActor static let session = Session()
    static var checks = 0
    static var failures = 0
    static func check(_ value: Bool, _ label: String) {
        checks += 1
        if value { print("  ok   " + label) } else { failures += 1; print("  FAIL " + label) }
    }
    @MainActor static func until(_ condition: () -> Bool) async -> Bool {
        for _ in 0..<500 {
            if condition() { return true }
            try? await Task.sleep(for: .milliseconds(10))
        }
        return false
    }

    @MainActor static func main() async throws {
        setbuf(stdout, nil)
        URLProtocol.registerClass(CalendarGate.self)
        defer { URLProtocol.unregisterClass(CalendarGate.self) }
        // PostboxClient holds its provider weakly. The fixture host owns it, just as App does.
        let client = PostboxClient(base: URL(string: "https://calendar.test")!, tokens: session)
        check(client.tokens != nil, "the fixture owns the shared client's token provider")
        let now = ISO8601DateFormatter().date(from: "2026-10-25T23:30:00Z")!
        let store = CalendarStore(client: client, now: { now })
        let a = "/k/a", b = "/k/b"
        let first = CalendarGate.key(a, "GET", "/v1/calendars")
        CalendarGate.hold(first)
        store.configure(mailbox: a, enabled: true, emailEnabled: false)
        let oldRead = Task { await store.refresh() }
        check(await until { CalendarGate.waitingFor(first) }, "the first mailbox's refresh is deliberately held")
        store.configure(mailbox: b, enabled: true, emailEnabled: true)
        await store.refresh()
        check(store.folders.first?.name == b && store.occurrences.first?.title == b, "the next mailbox loads its own calendar")
        CalendarGate.release(first)
        await oldRead.value
        check(store.folders.first?.name == b && store.invitations.first?.title == b && !store.loading,
              "a late old refresh cannot replace another mailbox's state or loading status")
        check(store.selectedDay.id == "2026-10-25", "Today follows the saved timezone after it loads")
        let count = CalendarGate.history().filter { $0.path == "/v1/calendar/events" && $0.method == "GET" }.count
        await store.selectFolders([])
        check(store.occurrences.isEmpty && CalendarGate.history().filter { $0.path == "/v1/calendar/events" && $0.method == "GET" }.count == count,
              "choosing no calendars does not accidentally query all calendars")
        await store.selectFolders(nil)
        check(store.occurrences.count == 1, "All calendars is distinct from an empty selection")

        let key = CalendarEventKey(eventId: "evt_shared")
        let delayed = CalendarGate.key(a, "GET", "/v1/calendar/events/evt_shared")
        CalendarGate.hold(delayed)
        store.configure(mailbox: a, enabled: true, emailEnabled: false)
        let oldDetail = Task { await store.loadEvent(key) }
        check(await until { CalendarGate.waitingFor(delayed) }, "an old event detail request is held")
        store.configure(mailbox: b, enabled: true, emailEnabled: false)
        _ = await store.loadEvent(key)
        CalendarGate.release(delayed)
        _ = await oldDetail.value
        check(store.details[key]?.event.title == b, "even the same event id cannot carry another mailbox's detail into the cache")
        let occurrence = CalendarEventKey(eventId: "evt_shared", occurrenceId: "occ_old")
        let lateOccurrence = CalendarGate.key(b, "GET", "/v1/calendar/events/evt_shared", "occurrence_id=occ_old")
        CalendarGate.hold(lateOccurrence)
        let lateDetail = Task { await store.loadEvent(occurrence) }
        check(await until { CalendarGate.waitingFor(lateOccurrence) }, "a sibling occurrence detail is held")
        CalendarGate.flags(missingSeries: true)
        _ = await store.loadEvent(key)
        CalendarGate.flags()
        CalendarGate.release(lateOccurrence)
        _ = await lateDetail.value
        check(store.details.isEmpty, "a 404 invalidates other in-flight details of that event instead of restoring removed data")

        let fields: [String: Any] = ["title": "Keep this draft", "all_day": false, "start": "2026-10-25T09:00", "end": "2026-10-25T10:00", "tzid": "UTC"]
        CalendarGate.flags(failCreateOnce: true)
        let lost = await store.createEvent(fields: fields)
        check(lost == nil && !store.busy("create-event"), "a failed create leaves the caller's draft and permits an explicit retry")
        let failedCreateMessage = store.lastError
        await store.refresh()
        check(failedCreateMessage != nil && store.lastError == failedCreateMessage, "a background refresh cannot hide an unconfirmed create's error")
        let retried = await store.createEvent(fields: fields)
        let creates = CalendarGate.history().filter { $0.method == "POST" && $0.path == "/v1/calendar/events" }
        check(retried?.event.id == "evt_shared" && creates.count == 2 && creates[0].body == creates[1].body,
              "an ambiguous create retry keeps its encoded body and request key")
        let holdA = CalendarGate.key(a, "POST", "/v1/calendar/events"), holdB = CalendarGate.key(b, "POST", "/v1/calendar/events")
        CalendarGate.hold(holdA); CalendarGate.hold(holdB)
        store.configure(mailbox: a, enabled: true, emailEnabled: false)
        let oldCreate = Task { await store.createEvent(fields: fields) }
        check(await until { CalendarGate.waitingFor(holdA) }, "the old mailbox's create is held")
        store.configure(mailbox: b, enabled: true, emailEnabled: false)
        let newCreate = Task { await store.createEvent(fields: fields) }
        check(await until { CalendarGate.waitingFor(holdB) }, "the new mailbox can start a separate create")
        CalendarGate.release(holdA)
        let ignored = await oldCreate.value
        check(ignored == nil && store.busy("create-event"), "an old completion cannot clear the new mailbox's operation")
        CalendarGate.release(holdB)
        let current = await newCreate.value
        check(current != nil && !store.busy("create-event"), "the current create finishes independently")
        let lastCreates = Array(CalendarGate.history().filter { $0.method == "POST" && $0.path == "/v1/calendar/events" }.suffix(2))
        check(lastCreates.count == 2 && lastCreates[0].mailbox != lastCreates[1].mailbox && lastCreates[0].body != lastCreates[1].body,
              "identical text in two mailboxes has independent create identities")
        CalendarGate.flags(conflict: true)
        let changed = await store.updateEvent(id: "evt_shared", revision: 1, scope: .this, occurrenceId: "occ_old", fields: ["title": "Unsent edit"])
        check(changed == nil && store.revisionConflict, "a stale revision is a conflict, never a silent overwrite")
        let conflictMessage = store.lastError
        await store.refresh()
        check(conflictMessage != nil && store.lastError == conflictMessage && store.revisionConflict,
              "a background refresh keeps the conflict's recovery action available")
        check(CalendarGate.history().filter { $0.method == "PATCH" }.count == 1, "a conflict is never automatically retried with another revision")
        CalendarGate.flags()
        let feedStatus = await store.feedStatus(id: "cal_shared")
        check(feedStatus?.enabled == true && feedStatus?.url == nil, "feed status cannot expose a subscription capability, even if a server includes one")
        check(store.lastError == conflictMessage && store.revisionConflict, "a successful independent read cannot acknowledge a pending write conflict")

        let feedA = CalendarGate.key(a, "GET", "/v1/calendar/feeds/cal_shared")
        let feedB = CalendarGate.key(b, "GET", "/v1/calendar/feeds/cal_shared")
        CalendarGate.hold(feedA); CalendarGate.hold(feedB)
        store.configure(mailbox: a, enabled: true, emailEnabled: false)
        let oldFeed = Task { await store.feedStatus(id: "cal_shared") }
        check(await until { CalendarGate.waitingFor(feedA) }, "the old mailbox's subscription read is held")
        store.configure(mailbox: b, enabled: true, emailEnabled: false)
        let nextFeed = Task { await store.feedStatus(id: "cal_shared") }
        check(await until { CalendarGate.waitingFor(feedB) }, "the new mailbox starts its own subscription read")
        CalendarGate.release(feedA)
        let staleFeed = await oldFeed.value
        check(staleFeed == nil && store.busy("feed:cal_shared"), "a late feed read cannot escape its mailbox or clear the new operation")
        CalendarGate.release(feedB)
        check(await nextFeed.value != nil && !store.busy("feed:cal_shared"), "the current subscription read completes independently")

        let rotateA = CalendarGate.key(a, "POST", "/v1/calendar/feeds/cal_shared")
        CalendarGate.hold(rotateA)
        store.configure(mailbox: a, enabled: true, emailEnabled: false)
        let oldRotation = Task { await store.changeFeed(id: "cal_shared", enabled: true, projection: "busy") }
        check(await until { CalendarGate.waitingFor(rotateA) }, "a subscription rotation is held across a mailbox change")
        store.configure(mailbox: b, enabled: true, emailEnabled: false)
        CalendarGate.release(rotateA)
        check(await oldRotation.value == nil, "an old mailbox's newly minted feed capability is discarded")
        let currentFeed = await store.changeFeed(id: "cal_shared", enabled: true, projection: "busy")
        check(currentFeed?.url?.hasSuffix("synthetic-b.ics") == true, "only the current mailbox receives its newly created subscription URL")

        let attendeePath = "/v1/calendar/events/evt_shared/attendees"
        let attendeeGate = CalendarGate.key(b, "POST", attendeePath)
        CalendarGate.hold(attendeeGate)
        let attendeeChange = Task { await store.changeAttendees(key: occurrence, revision: 13, scope: .this,
            adding: [["handle": "/guest"]], removing: ["att_opaque"]) }
        check(await until { CalendarGate.waitingFor(attendeeGate) }, "an attendee change holds the event revision lock")
        let patches = CalendarGate.history().filter { $0.method == "PATCH" }.count
        let overlapping = await store.updateEvent(id: "evt_shared", revision: 13, scope: .all, occurrenceId: nil, fields: ["title": "Overlapping change"])
        check(overlapping == nil && CalendarGate.history().filter { $0.method == "PATCH" }.count == patches,
              "another revision write cannot race an in-flight attendee change")
        CalendarGate.release(attendeeGate)
        check(await attendeeChange.value, "the current attendee change completes")
        let attendeeBody = try JSONSerialization.jsonObject(with: CalendarGate.history().last { $0.path == attendeePath }!.body) as! [String: Any]
        check(attendeeBody["expected_revision"] as? Int == 13 && attendeeBody["scope"] as? String == "this"
              && attendeeBody["occurrence_id"] as? String == "occ_old" && attendeeBody["remove"] as? [String] == ["att_opaque"],
              "attendee changes retain the chosen revision, occurrence and opaque attendee identifier")

        check(await store.placeInvitation(id: "evt_shared", calendar: "cal_b"), "an attendee can place an invitation in their own calendar")
        let placed = CalendarGate.history().last { $0.path.hasSuffix("/placement") }!
        let placementBody = try JSONSerialization.jsonObject(with: placed.body) as! [String: Any]
        check(placed.mailbox == b && placed.method == "PUT" && placementBody["calendar_id"] as? String == "cal_b",
              "invitation placement uses the acting mailbox and selected destination")
        check(await store.linkGroup(id: "evt_shared", revision: 14, group: nil), "a linked chat can be explicitly removed")
        let linked = try JSONSerialization.jsonObject(with: CalendarGate.history().last { $0.path.hasSuffix("/linked-group") }!.body) as! [String: Any]
        check(linked["expected_revision"] as? Int == 14 && linked["group_id"] is NSNull, "unlinking retains its revision and sends an explicit null")

        let imported = await store.previewImport(data: Data("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".utf8), calendar: nil)
        check(imported?.calendarId == "cal_b" && imported?.events?.last?.importable == false, "import preview retains its resolved destination and unimportable entries")
        let confirmed = await store.confirmImport(draft: "draft_shared", selected: [1, 0, 1, 0], calendar: "cal_b")
        let confirmedAgain = await store.confirmImport(draft: "draft_shared", selected: [0, 1], calendar: "cal_b")
        let confirmations = CalendarGate.history().filter { $0.path.hasSuffix("/draft_shared/confirm") }
        let selection = try JSONSerialization.jsonObject(with: confirmations[0].body) as! [String: Any]
        check(confirmed?.created?.count == 1 && confirmedAgain?.created?.count == 1 && selection["select"] as? [Int] == [0, 1]
              && confirmations[0].body == confirmations[1].body, "confirmation retries keep one canonical selection and destination")
        let exported = await store.exportEvent(id: "evt_shared")
        check(exported == Data("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".utf8), "native export preserves the server's exact iCalendar bytes")

        let exportGate = CalendarGate.key(b, "GET", "/v1/calendar/events/evt_shared.ics")
        CalendarGate.hold(exportGate)
        let cancelledExport = Task { await store.exportEvent(id: "evt_shared") }
        check(await until { CalendarGate.waitingFor(exportGate) }, "an export is held until its view cancels")
        cancelledExport.cancel()
        CalendarGate.release(exportGate)
        check(await cancelledExport.value == nil, "a cancelled export cannot deliver a file after the requesting view is gone")
        CalendarGate.flags(rejectCreates: true)
        for index in 0..<17 { _ = await store.createEvent(fields: ["title": "Rejected \(index)"]) }
        CalendarGate.flags()
        check(await store.createEvent(fields: ["title": "Valid after corrections"]) != nil, "definite validation failures do not consume the ambiguous-attempt budget")
        let beforeDisabled = CalendarGate.history().count
        store.configure(mailbox: b, enabled: false, emailEnabled: false)
        await store.refresh()
        _ = await store.createEvent(fields: fields)
        check(store.details.isEmpty && store.occurrences.isEmpty && CalendarGate.history().count == beforeDisabled,
              "feature-off erases local calendar state and performs no reads or writes")

        // Live authorization proof changes before a SwiftUI lifecycle callback reconfigures.
        let credentials = Session()
        let scopedClient = PostboxClient(base: client.base, tokens: credentials)
        var context: CalendarAccessScope? = .init(subject: "first", authorization: UUID(), actor: UUID(), mailbox: a)
        let scoped = CalendarStore(client: scopedClient, now: { now }, scope: { context })
        scoped.configure(mailbox: a, enabled: true, emailEnabled: true)
        await scoped.refresh(); _ = await scoped.loadEvent(key)
        check(scoped.folders.count == 1 && scoped.details[key] != nil, "scoped calendar loads current metadata")
        let editorScope = scoped.scopeId
        CalendarGate.hold(delayed)
        let oldAuthorizationRead = Task { await scoped.loadEvent(key) }
        check(await until { CalendarGate.waitingFor(delayed) }, "old authorization detail is held")
        context = .init(subject: "first", authorization: UUID(), actor: context!.actor, mailbox: a)
        check(!scoped.enabled && scoped.mailbox == nil && scoped.folders.isEmpty && scoped.details.isEmpty
              && scoped.occurrences.isEmpty && scoped.invitations.isEmpty && scoped.scopeId != editorScope,
              "same-inbox reauthorization immediately hides data and retires the editor proof")
        scoped.configure(mailbox: a, enabled: true, emailEnabled: true)
        let newAuthorizationRead = Task { await scoped.loadEvent(key) }
        check(await until { CalendarGate.waitingCount(delayed) == 2 }, "replacement authorization has its own detail read")
        CalendarGate.releaseOne(delayed, status: 401)
        check(await oldAuthorizationRead.value == nil && credentials.renewals == 0, "late old 401 cannot renew the replacement grant")
        CalendarGate.release(delayed)
        check(await newAuthorizationRead.value?.event.title == a && scoped.details[key] != nil, "old read cannot clear the new read's result")

        let beforeActorChange = scoped.scopeId
        let authorization = context!.authorization
        context = .init(subject: "first", authorization: authorization, actor: UUID(), mailbox: b)
        context = .init(subject: "first", authorization: authorization, actor: UUID(), mailbox: a)
        check(scoped.scopeId != beforeActorChange && scoped.details.isEmpty, "A-to-B-to-A cannot revive an earlier calendar activation")
        scoped.configure(mailbox: a, enabled: true, emailEnabled: false)
        credentials.pauseToken = true
        let tokenRead = Task { await scoped.loadEvent(key) }
        check(await until { credentials.tokenWaiter != nil }, "calendar token acquisition is held")
        let beforeTokenReply = CalendarGate.history().count
        context = .init(subject: "replacement", authorization: UUID(), actor: UUID(), mailbox: a)
        credentials.tokenWaiter?.resume(returning: "synthetic-replacement-token"); credentials.tokenWaiter = nil; credentials.pauseToken = false
        check(await tokenRead.value == nil && CalendarGate.history().count == beforeTokenReply,
              "late token cannot send an old calendar request with replacement credentials")
        scoped.configure(mailbox: a, enabled: true, emailEnabled: false)

        credentials.pauseRenewal = true
        CalendarGate.hold(delayed)
        let renewingRead = Task { await scoped.loadEvent(key) }
        check(await until { CalendarGate.waitingFor(delayed) }, "current calendar read reaches the controlled server")
        CalendarGate.releaseOne(delayed, status: 401)
        check(await until { credentials.renewalWaiter != nil }, "ordinary renewal is held before its result")
        let beforeRenewalReply = CalendarGate.history().count
        context = .init(subject: "replacement", authorization: UUID(), actor: UUID(), mailbox: a)
        credentials.renewalWaiter?.resume(returning: "synthetic-new-grant"); credentials.renewalWaiter = nil; credentials.pauseRenewal = false
        check(await renewingRead.value == nil && CalendarGate.history().count == beforeRenewalReply,
              "late renewal cannot replay the earlier calendar operation")
        CalendarGate.release(delayed)
        scoped.configure(mailbox: a, enabled: true, emailEnabled: false)

        // Old create cleanup cannot clear a replacement mutation or its uncertain retry body.
        CalendarGate.hold(holdA)
        let obsoleteCreate = Task { await scoped.createEvent(fields: fields) }
        check(await until { CalendarGate.waitingCount(holdA) == 1 }, "earlier grant's creation is held")
        context = .init(subject: "replacement", authorization: UUID(), actor: UUID(), mailbox: a)
        scoped.configure(mailbox: a, enabled: true, emailEnabled: false)
        let currentCreate = Task { await scoped.createEvent(fields: fields) }
        check(await until { CalendarGate.waitingCount(holdA) == 2 }, "replacement creation owns a separate busy operation")
        let pendingCreates = CalendarGate.history().filter { $0.mailbox == a && $0.method == "POST" && $0.path == "/v1/calendar/events" }
        let retryBody = pendingCreates.last!.body
        check(pendingCreates[pendingCreates.count - 2].body != retryBody, "explicit new authorization gets a separate create intent")
        CalendarGate.releaseOne(holdA)
        check(await obsoleteCreate.value == nil && scoped.busy("create-event"), "old create completion cannot clear current mutation ownership")
        CalendarGate.releaseOne(holdA, status: 503)
        check(await currentCreate.value == nil && !scoped.busy("create-event"), "uncertain current creation remains retryable")
        let retryCreation = Task { await scoped.createEvent(fields: fields) }
        check(await until { CalendarGate.waitingCount(holdA) == 1 }, "explicit retry reaches the controlled server")
        let retriedBody = CalendarGate.history().last { $0.mailbox == a && $0.method == "POST" && $0.path == "/v1/calendar/events" }!.body
        check(retriedBody == retryBody, "late old cleanup preserves the replacement attempt's exact request and key")
        CalendarGate.release(holdA)
        check(await retryCreation.value != nil, "current uncertain creation can resolve normally")

        // A refusal invalidates older aggregates and file exports under the same authorization.
        await scoped.refresh(); _ = await scoped.loadEvent(key)
        CalendarGate.hold(first)
        let accessRead = Task { await scoped.refresh() }
        check(await until { CalendarGate.waitingFor(first) }, "aggregate read is held before permission loss")
        let accessExportKey = CalendarGate.key(a, "GET", "/v1/calendar/events/evt_shared.ics")
        CalendarGate.hold(accessExportKey)
        let accessExport = Task { await scoped.exportEvent(id: "evt_shared") }
        check(await until { CalendarGate.waitingFor(accessExportKey) }, "export is held before permission loss")
        CalendarGate.hold(delayed)
        let refusedDetail = Task { await scoped.loadEvent(key) }
        check(await until { CalendarGate.waitingFor(delayed) }, "permission refusal is independently observable")
        CalendarGate.releaseOne(delayed, status: 403)
        check(await refusedDetail.value == nil && scoped.details.isEmpty && scoped.folders.isEmpty && !scoped.loading,
              "denied access clears visible reads and their loading state")
        CalendarGate.release(first); CalendarGate.release(accessExportKey); CalendarGate.release(delayed)
        await accessRead.value
        check(await accessExport.value == nil && scoped.details.isEmpty && scoped.occurrences.isEmpty,
              "older successful aggregate and export cannot resurrect data after access loss")
        check(scoped.lastError != nil, "access loss remains visible instead of acknowledging an unrelated success")
        context = nil
        let beforeRetired = CalendarGate.history().count
        await scoped.refresh(); _ = await scoped.confirmImport(draft: "draft_shared", selected: [0], calendar: "cal_a")
        check(!scoped.enabled && scoped.settings.timezone == nil && CalendarGate.history().count == beforeRetired,
              "retired identity cannot read or import under the next session")
        print("Calendar store: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }
}
