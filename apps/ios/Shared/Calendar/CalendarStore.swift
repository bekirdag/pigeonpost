import Foundation
import Observation

struct CalendarAccessScope: Equatable, Sendable {
    let subject: String
    let authorization: UUID
    let actor: UUID
    let mailbox: String
}

/// Calendar state belongs to one acting mailbox. The app supplies identity/capabilities rather
/// than letting a late response consult whichever mailbox happens to be selected afterwards.
@MainActor
@Observable
final class CalendarStore {
    private var storedMailbox: String?
    private(set) var mailbox: String? { get { contextIsCurrent ? storedMailbox : nil } set { storedMailbox = newValue } }
    private var storedEnabled = false
    private(set) var enabled: Bool { get { contextIsCurrent && storedEnabled } set { storedEnabled = newValue } }
    private var storedEmailEnabled = false
    private(set) var emailEnabled: Bool { get { contextIsCurrent && storedEmailEnabled } set { storedEmailEnabled = newValue } }
    private var storedFolders: [CalendarFolder] = []
    private(set) var folders: [CalendarFolder] { get { contextIsCurrent ? storedFolders : [] } set { storedFolders = newValue } }
    private var storedOccurrences: [EventOccurrence] = []
    private(set) var occurrences: [EventOccurrence] { get { contextIsCurrent ? storedOccurrences : [] } set { storedOccurrences = newValue } }
    private var storedInvitations: [CalendarInvitation] = []
    private(set) var invitations: [CalendarInvitation] { get { contextIsCurrent ? storedInvitations : [] } set { storedInvitations = newValue } }
    private var storedFilteredInvitations: [CalendarInvitation] = []
    private(set) var filteredInvitations: [CalendarInvitation] { get { contextIsCurrent ? storedFilteredInvitations : [] } set { storedFilteredInvitations = newValue } }
    private var storedSettings = CalendarSettings()
    private(set) var settings: CalendarSettings { get { contextIsCurrent ? storedSettings : CalendarSettings() } set { storedSettings = newValue } }
    private var storedDetails: [CalendarEventKey: CalendarEventResponse] = [:]
    private(set) var details: [CalendarEventKey: CalendarEventResponse] { get { contextIsCurrent ? storedDetails : [:] } set { storedDetails = newValue } }
    private var storedSelectedDay: CalendarDay
    private(set) var selectedDay: CalendarDay { get { contextIsCurrent ? storedSelectedDay : CalendarDay(date: now(), zone: .current) } set { storedSelectedDay = newValue } }
    /// nil means all calendars; an explicit empty set means show none.
    private var storedSelectedFolders: Set<String>?
    private(set) var selectedFolders: Set<String>? { get { contextIsCurrent ? storedSelectedFolders : nil } set { storedSelectedFolders = newValue } }
    private var storedLoading = false
    private(set) var loading: Bool { get { contextIsCurrent && storedLoading } set { storedLoading = newValue } }
    private var storedError: String?
    private(set) var lastError: String? { get { contextIsCurrent ? storedError : nil } set { storedError = newValue } }
    private var storedConflict = false
    private(set) var revisionConflict: Bool { get { contextIsCurrent && storedConflict } set { storedConflict = newValue } }
    var isOpen = false
    private var storedOpenEvent: CalendarEventKey?
    var openEvent: CalendarEventKey? { get { contextIsCurrent ? storedOpenEvent : nil } set { storedOpenEvent = newValue } }
    private var storedNavigation: CalendarEventKey?
    var pendingNavigation: CalendarEventKey? { get { contextIsCurrent ? storedNavigation : nil } set { storedNavigation = newValue } }

    private var generation = UUID()
    private var refreshId = UUID()
    private var detailReads: [CalendarEventKey: UUID] = [:]
    private var operations: [String: UUID] = [:]
    private var followingToday = true
    private var preservingMutationError = false
    @ObservationIgnored private let sourceClient: PostboxClient
    @ObservationIgnored private var client: PostboxClient?
    @ObservationIgnored private let scopeProvider: (() -> CalendarAccessScope?)?
    private var boundScope: CalendarAccessScope?
    private let retiredScope = UUID()
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private var attempts: [CreateIntent: CreateAttempt] = [:]

    private struct CreateIntent: Hashable { let scope: UUID; let kind: String; let body: Data }
    private struct CreateAttempt { let key: String; let at: Date }

    init(client: PostboxClient, now: @escaping () -> Date = Date.init, scope: (() -> CalendarAccessScope?)? = nil) {
        self.sourceClient = client
        self.now = now
        self.scopeProvider = scope
        storedSelectedDay = CalendarDay(date: now(), zone: .current)
    }

    private var contextIsCurrent: Bool {
        guard let scopeProvider else { return true }
        guard let scope = scopeProvider() else { return false }
        return scope == boundScope && scope.mailbox == storedMailbox
    }
    private func current(_ visit: UUID) -> Bool { generation == visit && contextIsCurrent && !Task.isCancelled }
    var scopeId: UUID { contextIsCurrent ? generation : retiredScope }
    var zone: TimeZone { settings.timezone.flatMap(TimeZone.init(identifier:)) ?? .current }
    var weekStart: Int { settings.weekStart.flatMap { (0...6).contains($0) ? $0 : nil } ?? 1 }
    var pendingCount: Int { invitations.count }
    func busy(_ operation: String) -> Bool { contextIsCurrent && operations[operation] != nil }

    /// Synchronous reset first, before any new network work or rendering another mailbox.
    func configure(mailbox: String?, enabled: Bool, emailEnabled: Bool) {
        let nextScope = scopeProvider?()
        let allowed = enabled && mailbox != nil && (scopeProvider == nil || nextScope?.mailbox == mailbox)
        self.emailEnabled = emailEnabled && allowed
        guard storedMailbox != mailbox || storedEnabled != allowed || boundScope != nextScope else { return }
        client = nil
        generation = UUID()
        refreshId = UUID()
        folders = []; occurrences = []; invitations = []; filteredInvitations = []
        settings = CalendarSettings(); details = [:]; detailReads = [:]; operations = [:]; attempts = [:]
        preservingMutationError = false
        selectedFolders = nil; loading = false; lastError = nil; revisionConflict = false; openEvent = nil; pendingNavigation = nil; followingToday = true
        selectedDay = CalendarDay(date: now(), zone: .current)
        self.mailbox = mailbox
        self.enabled = allowed
        boundScope = nextScope
        guard allowed, let source = sourceClient.tokens else { return }
        let visit = generation
        let tokens = ScopedTokenProvider(source: source) { [weak self] in self?.current(visit) == true }
        client = PostboxClient(base: sourceClient.base, tokens: tokens, retainingTokens: true)
    }

    func refresh() async {
        guard enabled, let mailbox, let client, !Task.isCancelled else { return }
        let visit = generation
        let request = UUID()
        refreshId = request
        loading = true
        defer { if generation == visit && refreshId == request { loading = false } }
        do {
            async let listed = client.calendars(mailbox: mailbox)
            async let preferences = client.calendarSettings(mailbox: mailbox)
            async let pending = client.calendarInvitations(mailbox: mailbox, filtered: false)
            async let filtered = client.calendarInvitations(mailbox: mailbox, filtered: true)
            let result = try await (listed, preferences, pending, filtered)
            guard current(visit), refreshId == request else { return }
            folders = result.0; settings = result.1; invitations = result.2; filteredInvitations = result.3
            if let selected = selectedFolders { selectedFolders = selected.intersection(Set(folders.map(\.id))) }
            if followingToday { selectedDay = CalendarDay(date: now(), zone: zone) }
            if selectedFolders?.isEmpty == true {
                occurrences = []
                if !preservingMutationError { lastError = nil }
                if let openEvent { _ = await loadEvent(openEvent) }
                return
            }
            let range = try CalendarMonthWindow(containing: selectedDay, weekStart: weekStart).bounds(in: zone)
            let entries = try await client.calendarOccurrences(mailbox: mailbox,
                from: Int(range.lowerBound.timeIntervalSince1970), to: Int(range.upperBound.timeIntervalSince1970), calendars: selectedFolders?.sorted() ?? [])
            guard current(visit), refreshId == request else { return }
            occurrences = entries
            if !preservingMutationError { lastError = nil }
            if let openEvent { _ = await loadEvent(openEvent) }
        } catch {
            guard current(visit), refreshId == request else { return }
            report(error)
        }
    }

    func select(_ day: CalendarDay) async {
        guard enabled else { return }
        followingToday = false
        let differentMonth = day.year != selectedDay.year || day.month != selectedDay.month
        selectedDay = day
        if differentMonth { await refresh() }
    }

    func selectFolders(_ ids: Set<String>?) async { guard enabled else { return }; selectedFolders = ids; await refresh() }

    func today() async {
        guard enabled else { return }
        followingToday = true
        selectedDay = CalendarDay(date: now(), zone: zone)
        await refresh()
    }

    func live() async {
        let visit = generation
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(isOpen ? 15 : 60))
            guard current(visit) else { return }
            if enabled { await refresh() }
        }
    }

    @discardableResult
    func loadEvent(_ key: CalendarEventKey) async -> CalendarEventResponse? {
        guard enabled, let mailbox, let client, !Task.isCancelled else { return nil }
        let visit = generation
        let request = UUID()
        detailReads[key] = request
        do {
            let response = try await client.calendarEvent(mailbox: mailbox, id: key.eventId, occurrenceId: key.occurrenceId)
            guard current(visit), detailReads[key] == request else { return nil }
            details[key] = response
            return response
        } catch {
            guard current(visit), detailReads[key] == request else { return nil }
            if (error as? APIError)?.status == 404 { forgetEvent(key.eventId) }
            report(error)
            return nil
        }
    }

    private func forgetEvent(_ id: String) {
        // An older aggregate or export response must not resurrect a now-unavailable item.
        refreshId = UUID()
        loading = false
        operations["export:" + id] = nil
        detailReads = detailReads.filter { $0.key.eventId != id }
        details = details.filter { $0.key.eventId != id }
        occurrences.removeAll { $0.eventId == id }
        invitations.removeAll { $0.eventId == id }
        filteredInvitations.removeAll { $0.eventId == id }
    }

    private func createAttempt(kind: String, fields: [String: Any]) throws -> (CreateIntent, CreateAttempt) {
        var clean = fields
        clean.removeValue(forKey: "request_key")
        let intent = CreateIntent(scope: generation, kind: kind, body: try CalendarWritePayload(clean).data)
        if let existing = attempts[intent] {
            guard now().timeIntervalSince(existing.at) >= 0, now().timeIntervalSince(existing.at) < 86400 else {
                throw APIError(status: 0, code: "calendar_retry_expired", detail: nil)
            }
            return (intent, existing)
        }
        guard attempts.count < 16 else { throw APIError(status: 0, code: "calendar_attempt_limit", detail: nil) }
        let attempt = CreateAttempt(key: "cal_" + UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased(), at: now())
        attempts[intent] = attempt
        return (intent, attempt)
    }

    private func mutation<T>(_ name: String, _ body: (String, PostboxClient, UUID) async throws -> T) async -> T? {
        guard enabled, let mailbox, let client, operations[name] == nil, !Task.isCancelled else { return nil }
        let visit = generation
        let operation = UUID()
        operations[name] = operation
        lastError = nil; revisionConflict = false; preservingMutationError = false
        defer { if generation == visit && operations[name] == operation { operations[name] = nil } }
        do {
            let result = try await body(mailbox, client, visit)
            guard current(visit), operations[name] == operation else { return nil }
            await refresh()
            guard current(visit), operations[name] == operation else { return nil }
            return result
        } catch {
            guard current(visit), operations[name] == operation else { return nil }
            report(error, mutation: true)
            return nil
        }
    }

    /// A read may finish after a mailbox switch or view cancellation. Never publish it into the
    /// next identity, or acknowledge an unrelated failed write just because a read succeeded.
    private func readScoped<T>(_ name: String, eventId: String? = nil, _ body: (String, PostboxClient, UUID) async throws -> T) async -> T? {
        guard enabled, let mailbox, let client, operations[name] == nil, !Task.isCancelled else { return nil }
        let visit = generation, operation = UUID()
        operations[name] = operation
        defer { if generation == visit && operations[name] == operation { operations[name] = nil } }
        do {
            let result = try await body(mailbox, client, visit)
            guard current(visit), operations[name] == operation else { return nil }
            return result
        } catch {
            guard current(visit), operations[name] == operation else { return nil }
            if (error as? APIError)?.status == 404, let eventId { forgetEvent(eventId) }
            report(error)
            return nil
        }
    }

    func createEvent(fields: [String: Any]) async -> CalendarEventResponse? {
        await mutation("create-event") { mailbox, client, visit in
            let (intent, attempt) = try createAttempt(kind: "event", fields: fields)
            var body = fields
            body["request_key"] = attempt.key
            do {
                let result = try await client.createCalendarEvent(mailbox: mailbox, payload: CalendarWritePayload(body))
                guard current(visit) else { throw CancellationError() }
                attempts[intent] = nil
                return result
            } catch {
                if current(visit) { discardRejectedAttempt(intent, error: error) }
                throw error
            }
        }
    }

    func createFolder(name: String, color: String) async -> CalendarFolder? {
        await mutation("create-folder") { mailbox, client, visit in
            let (intent, attempt) = try createAttempt(kind: "folder", fields: ["name": name, "color": color])
            do {
                let result = try await client.createCalendar(mailbox: mailbox, name: name, color: color, requestKey: attempt.key)
                guard current(visit) else { throw CancellationError() }
                attempts[intent] = nil
                return result
            } catch {
                if current(visit) { discardRejectedAttempt(intent, error: error) }
                throw error
            }
        }
    }

    func updateFolder(id: String, revision: Int, fields: [String: Any]) async -> Bool {
        await mutation("folder:" + id) { mailbox, client, _ in
            _ = try await client.updateCalendar(mailbox: mailbox, id: id, revision: revision, fields: fields)
            return true
        } ?? false
    }

    func deleteFolder(id: String, revision: Int) async -> Bool {
        await mutation("folder:" + id) { mailbox, client, _ in
            try await client.deleteCalendar(mailbox: mailbox, id: id, revision: revision)
            return true
        } ?? false
    }

    func updateEvent(id: String, revision: Int, scope: CalendarEventScope, occurrenceId: String?, fields: [String: Any]) async -> CalendarEventResponse? {
        await mutation("event:" + id) { mailbox, client, _ in
            var body = fields
            body["expected_revision"] = revision; body["scope"] = scope.rawValue
            if let occurrenceId { body["occurrence_id"] = occurrenceId }
            return try await client.updateCalendarEvent(mailbox: mailbox, id: id, payload: CalendarWritePayload(body))
        }
    }

    func cancelEvent(id: String, revision: Int, scope: CalendarEventScope, occurrenceId: String?, message: String?) async -> Bool {
        await mutation("event:" + id) { mailbox, client, _ in
            try await client.cancelCalendarEvent(mailbox: mailbox, id: id, revision: revision, scope: scope, occurrenceId: occurrenceId, message: message)
            return true
        } ?? false
    }

    func answer(id: String, status: CalendarAnswer, scope: CalendarAnswerScope, occurrenceId: String?, note: String?) async -> Bool {
        await mutation("answer:" + id) { mailbox, client, _ in
            try await client.calendarRSVP(mailbox: mailbox, id: id, status: status, scope: scope, occurrenceId: occurrenceId, note: note)
            return true
        } ?? false
    }

    func saveSettings(fields: [String: Any]) async -> Bool {
        await mutation("settings") { mailbox, client, _ in
            _ = try await client.saveCalendarSettings(mailbox: mailbox, fields: fields)
            return true
        } ?? false
    }

    func saveReminders(id: String, minutes: [Int]) async -> Bool {
        await mutation("reminders:" + id) { mailbox, client, _ in
            try await client.calendarReminders(mailbox: mailbox, id: id, reminders: minutes.map { CalendarReminder(minutesBefore: $0, channel: "push") })
            return true
        } ?? false
    }

    func guestLink(id: String, attendee: String) async -> CalendarGuestLink? {
        await mutation("guest-link:" + attendee) { mailbox, client, _ in
            try await client.calendarGuestLink(mailbox: mailbox, id: id, attendee: attendee)
        }
    }

    func changeAttendees(key: CalendarEventKey, revision: Int, scope: CalendarAnswerScope,
                         adding: [[String: Any]], removing: [String]) async -> Bool {
        await mutation("event:" + key.eventId) { mailbox, client, _ in
            try await client.calendarAttendees(mailbox: mailbox, id: key.eventId, revision: revision,
                scope: scope, occurrenceId: scope == .this ? key.occurrenceId : nil, adding: adding, removing: removing)
            return true
        } ?? false
    }

    func placeInvitation(id: String, calendar: String) async -> Bool {
        await mutation("placement:" + id) { mailbox, client, _ in
            try await client.moveCalendarInvitation(mailbox: mailbox, id: id, calendar: calendar)
            return true
        } ?? false
    }

    func linkGroup(id: String, revision: Int, group: String?) async -> Bool {
        await mutation("event:" + id) { mailbox, client, _ in
            try await client.linkCalendarGroup(mailbox: mailbox, id: id, revision: revision, group: group)
            return true
        } ?? false
    }

    func feedStatus(id: String) async -> CalendarFeed? {
        await readScoped("feed:" + id) { mailbox, client, _ in
            var status = try await client.calendarFeed(mailbox: mailbox, id: id)
            status.url = nil
            return status
        }
    }

    func changeFeed(id: String, enabled: Bool, projection: String) async -> CalendarFeed? {
        await mutation("feed:" + id) { mailbox, client, _ in
            try await client.setCalendarFeed(mailbox: mailbox, id: id, enabled: enabled, projection: projection)
        }
    }

    func exportEvent(id: String) async -> Data? {
        await readScoped("export:" + id, eventId: id) { mailbox, client, _ in
            try await client.calendarICS(mailbox: mailbox, id: id)
        }
    }

    func previewImport(data: Data, calendar: String?) async -> CalendarImportPreview? {
        // Preview may create the default calendar, so refresh the folder list afterwards.
        await mutation("import-preview") { mailbox, client, _ in
            try await client.previewCalendarImport(mailbox: mailbox, data: data, calendar: calendar)
        }
    }

    func confirmImport(draft: String, selected: [Int], calendar: String) async -> CalendarImportResult? {
        await mutation("import-confirm:" + draft) { mailbox, client, _ in
            try await client.confirmCalendarImport(mailbox: mailbox, draft: draft,
                selected: Array(Set(selected)).sorted(), calendar: calendar)
        }
    }

    func clearError() { guard contextIsCurrent else { return }; lastError = nil; revisionConflict = false; preservingMutationError = false }

    private func discardReadAccess() {
        refreshId = UUID()
        detailReads = [:]
        operations = [:]
        folders = []; occurrences = []; invitations = []; filteredInvitations = []
        details = [:]; settings = CalendarSettings(); selectedFolders = nil
        loading = false; openEvent = nil; pendingNavigation = nil
    }

    private func discardRejectedAttempt(_ intent: CreateIntent, error: Error) {
        if let status = (error as? APIError)?.status, [400, 401, 403, 404, 413, 422, 429].contains(status) {
            attempts[intent] = nil
        }
    }

    private func report(_ error: Error, mutation: Bool = false) {
        let api = error as? APIError
        let accessLost = api.map { [401, 403, 404, 410].contains($0.status) } ?? false
        if let status = api?.status, [401, 403, 410].contains(status) { discardReadAccess() }
        // A successful poll is not an acknowledgement of a failed write. Likewise an unrelated
        // read failure must not replace its actionable conflict/retry state. Access loss wins.
        if preservingMutationError && !mutation && !accessLost { return }
        revisionConflict = api?.code == "revision_conflict"
        if api?.code == "feature_disabled" {
            configure(mailbox: mailbox, enabled: false, emailEnabled: false)
        }
        preservingMutationError = mutation
        lastError = CalendarProblems.message(error)
    }
}

enum CalendarProblems {
    static func message(_ error: Error) -> String {
        if let time = error as? CalendarTimeError {
            switch time {
            case .invalidFormat: return "Check the date and time."
            case .unknownZone: return "Choose a known timezone."
            case .nonexistent: return "That local time does not exist because the clocks change. Choose another time."
            case .ambiguous: return "That local time happens twice. Choose Earlier or Later."
            case .order: return "The end must be after the start."
            }
        }
        switch (error as? APIError)?.code {
        case "revision_conflict": return "This event changed. Review the current version before saving again."
        case "feature_disabled": return "Calendar is not available on this server."
        case "not_found", "calendar_unavailable": return "This calendar item is no longer available."
        case "human_required": return "Sign in as yourself with a verified address to do that."
        case "forbidden": return "You do not have permission to change this event."
        case "recurrence_invalid": return "Check the recurrence rule."
        case "window_too_large", "expansion_limit": return "Choose a smaller calendar window."
        case "meeting_capacity": return "There are too many required attendees for this meeting size."
        case "external_unavailable": return "Invitations to contact addresses are unavailable. You can copy a guest's personal invitation link."
        case "event_cancelled": return "This event was cancelled."
        case "rate_limited": return "Too many changes. Wait a moment and try again."
        case "calendar_retry_expired", "calendar_attempt_limit": return "Check your calendar for unconfirmed events before creating them again."
        default: return "Could not update the calendar. Try again."
        }
    }
}
