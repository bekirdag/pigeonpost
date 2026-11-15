//  What the widgets are allowed to know, and nothing more.
//
//  The app writes one of these to the keychain group it shares with the widget extension whenever the inbox, the Nests or the
//  calendar change; the widget extension only ever reads it. The extension holds no token, no key
//  and no crypto state, and makes no network call — so this file is the whole of what a widget can
//  show, and its shape is the security boundary.
//
//  Conversations are end-to-end encrypted, so message text is left out unless the person turned on
//  "Show message previews in widgets" (off by default). That rule is enforced here, in the codec, on
//  both the way out and the way in: a snapshot that says previews are off never carries one, however
//  it was built.
//
//  Foundation only: compiled into the app, the widget extension and the swiftc tests alike.

import Foundation

struct WidgetSnapshot: Codable, Equatable, Sendable {
    /// Bumped on any change a reader of the previous version would misread. A reader refuses a
    /// version newer than its own rather than guessing (an extension older than the app, for the
    /// minutes between an update landing and the widget process restarting).
    static let currentVersion = 1
    /// The longest preview kept, in characters. A widget row shows one or two lines of it.
    static let previewLimit = 140
    /// How many conversations are kept. The extra-large iPad widget shows eight.
    static let conversationLimit = 8
    static let nestLimit = 6
    static let eventLimit = 8

    var version: Int = WidgetSnapshot.currentVersion
    var generatedAt: Date
    /// A one-way fingerprint of the signed-in account (not the subject itself), so the app can tell
    /// a snapshot written for somebody else and clear it before anything else is drawn.
    var owner: String
    var mailbox: WidgetMailbox
    var showsPreviews: Bool
    var unreadCount: Int
    var conversations: [WidgetConversation]
    /// Nil when the app has not loaded the account's Nests yet.
    var nests: WidgetNests?
    /// Nil when the app has not loaded the calendar yet, or the account has no calendar.
    var agenda: WidgetAgenda?

    /// The snapshot as it may be stored and shown: previews dropped unless they are switched on,
    /// trimmed to one line and to `previewLimit`, and every list cut to what a widget can show.
    func redacted() -> WidgetSnapshot {
        var copy = self
        copy.unreadCount = max(0, unreadCount)
        copy.conversations = Array(conversations.prefix(Self.conversationLimit)).map { row in
            var row = row
            row.unread = max(0, row.unread)
            row.preview = showsPreviews ? Self.clean(row.preview) : nil
            return row
        }
        if var nests = copy.nests {
            nests.items = Array(nests.items.prefix(Self.nestLimit))
            copy.nests = nests
        }
        if var agenda = copy.agenda {
            agenda.events = Array(agenda.events.prefix(Self.eventLimit))
            copy.agenda = agenda
        }
        return copy
    }

    /// One line, at most `previewLimit` characters, nil when nothing is left.
    static func clean(_ text: String?) -> String? {
        guard let text else { return nil }
        let line = text.components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        guard !line.isEmpty else { return nil }
        guard line.count > previewLimit else { return line }
        return String(line.prefix(previewLimit - 1)) + "…"
    }

    /// Everything but the moment it was written: what decides whether a write is worth a reload.
    func sameContent(as other: WidgetSnapshot) -> Bool {
        var a = self, b = other
        a.generatedAt = .distantPast
        b.generatedAt = .distantPast
        return a == b
    }
}

struct WidgetMailbox: Codable, Equatable, Sendable {
    /// The mailbox's address, so a tapped row opens the conversation in the mailbox it belongs to.
    var address: String
    /// The handle (`/bekir/main`) or the operator's label, for the widget header.
    var title: String
}

struct WidgetConversation: Codable, Equatable, Identifiable, Sendable {
    enum Kind: String, Codable, Sendable { case person, agent }

    var peer: String
    var name: String
    var kind: Kind = .person
    var lastAt: Date
    var unread: Int
    /// Present only when previews are switched on. See `WidgetSnapshot.redacted()`.
    var preview: String?

    var id: String { peer }
}

struct WidgetNests: Codable, Equatable, Sendable {
    /// Whether this account can have Nests at all. False shows no "Get a Nest" link.
    var available: Bool
    var items: [WidgetNest]
}

struct WidgetNest: Codable, Equatable, Identifiable, Sendable {
    enum Tone: String, Codable, Sendable { case ready, working, attention, paused, setup, failed, unknown }

    var id: String
    var name: String
    var tone: Tone
    /// The app's own words for the status ("Ready", "Needs you"…).
    var status: String
    /// Hours this Nest has run this period, when the app has read its usage.
    var runningHours: Double?
    /// The package's running hours for the period (pooled across the account's Nests).
    var hoursLimit: Int?

    /// The meter's fill, 0…1, or nil when there is nothing to measure against.
    var meterFraction: Double? {
        guard let used = runningHours, let limit = hoursLimit, limit > 0 else { return nil }
        return min(1, max(0, used / Double(limit)))
    }
}

struct WidgetAgenda: Codable, Equatable, Sendable {
    var events: [WidgetEvent]

    /// The events still to come at `date`: not yet ended, soonest first.
    func upcoming(at date: Date) -> [WidgetEvent] {
        events.filter { $0.endsAt > date }.sorted { ($0.startsAt, $0.title) < ($1.startsAt, $1.title) }
    }

    /// Of `upcoming(at:)`, the ones on the same calendar day as `date`, and the first one after it.
    func todayAndNext(at date: Date, calendar: Calendar = .current) -> (today: [WidgetEvent], next: WidgetEvent?) {
        let coming = upcoming(at: date)
        let today = coming.filter { calendar.isDate($0.startsAt, inSameDayAs: date) || $0.startsAt <= date }
        let next = coming.first { !today.contains($0) }
        return (today, next)
    }

    /// When the widget should next redraw: the moment the next event ends or starts.
    func nextChange(after date: Date) -> Date? {
        events.flatMap { [$0.startsAt, $0.endsAt] }.filter { $0 > date }.min()
    }
}

struct WidgetEvent: Codable, Equatable, Identifiable, Sendable {
    var eventId: String
    var occurrenceId: String?
    var title: String
    var startsAt: Date
    var endsAt: Date
    var allDay: Bool
    var location: String?
    /// The event has a Pigeonpost meeting room.
    var hasMeeting: Bool

    var id: String { eventId + ":" + (occurrenceId ?? "") }
}

/// The one way a snapshot becomes bytes and back. Both directions redact.
enum WidgetCodec {
    static func encode(_ snapshot: WidgetSnapshot) throws -> Data {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .secondsSince1970
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(snapshot.redacted())
    }

    /// Nil for anything unreadable or written by a newer app.
    static func decode(_ data: Data) -> WidgetSnapshot? {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .secondsSince1970
        guard let snapshot = try? decoder.decode(WidgetSnapshot.self, from: data),
              snapshot.version >= 1, snapshot.version <= WidgetSnapshot.currentVersion else { return nil }
        return snapshot.redacted()
    }
}
