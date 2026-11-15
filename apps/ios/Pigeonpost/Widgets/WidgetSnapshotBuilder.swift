//  From what the app already holds to the little a widget may know.
//
//  Pure functions over the app's own models, so what goes to the widgets is testable without a
//  session, a network or a widget (Tests/WidgetSnapshotTests.swift). `WidgetSync` decides when to
//  call them; this decides what comes out — and above all that no message text leaves the app
//  unless the person switched previews on.

import Foundation
import CryptoKit

enum WidgetSnapshotBuilder {
    /// A one-way fingerprint of the OIDC subject. The subject itself is not stored.
    static func owner(subject: String) -> String {
        SHA256.hash(data: Data(("pigeonpost-widget-owner:" + subject).utf8))
            .prefix(12).map { String(format: "%02x", $0) }.joined()
    }

    static func mailbox(_ mailbox: Mailbox) -> WidgetMailbox {
        WidgetMailbox(address: mailbox.address,
                      title: mailbox.handle ?? mailbox.label ?? PeerFace.displayName(mailbox.address))
    }

    /// The conversations a badge would count: not archived, newest first.
    static func conversations(_ all: [Conversation], archived: Set<String>, previews: Bool) -> [WidgetConversation] {
        all.filter { !archived.contains($0.peer) && !$0.isBlocked }
            .sorted { $0.last > $1.last }
            .prefix(WidgetSnapshot.conversationLimit)
            .map { conversation in
                WidgetConversation(
                    peer: conversation.peer,
                    name: conversation.name,
                    kind: conversation.mine ? .agent : .person,
                    lastAt: Date(timeIntervalSince1970: TimeInterval(conversation.last)),
                    unread: conversation.unread,
                    preview: previews ? preview(of: conversation) : nil
                )
            }
    }

    /// The last thing said, as prose. Only ever called with previews switched on.
    static func preview(of conversation: Conversation) -> String? {
        guard let last = conversation.messages.last(where: { $0.kind != .system }) else { return nil }
        let words = last.copyText.trimmingCharacters(in: .whitespacesAndNewlines)
        let text = words.isEmpty ? (last.attachments.isEmpty ? "" : "Attachment") : words
        guard !text.isEmpty else { return nil }
        return WidgetSnapshot.clean(last.kind == .outgoing ? "You: " + text : text)
    }

    /// The account's own Nests and the ones still being set up. Nil until the snapshot has loaded.
    static func nests(_ snapshot: NestSnapshot?, available: Bool, usage: [String: NestUsage]) -> WidgetNests? {
        guard available else { return WidgetNests(available: false, items: []) }
        guard let snapshot else { return nil }
        let allowance = snapshot.package?.activeHours
        let running = snapshot.nests.map { nest -> WidgetNest in
            let used = usage[nest.nestId]
            return WidgetNest(
                id: nest.nestId,
                name: nest.label,
                tone: tone(nest),
                status: NestWords.status(nest),
                runningHours: used?.runningSecs.map { Double($0) / 3600 },
                hoursLimit: used?.package?.activeHoursPooled ?? allowance
            )
        }
        let setting = snapshot.setups.map { setup in
            WidgetNest(id: setup.setupId, name: setup.label, tone: setup.status == "failed" ? .failed : .setup,
                       status: setup.status == "failed" ? "Setup failed" : "Setting up…", runningHours: nil, hoursLimit: allowance)
        }
        return WidgetNests(available: true, items: Array((running + setting).prefix(WidgetSnapshot.nestLimit)))
    }

    static func tone(_ nest: NestView) -> WidgetNest.Tone {
        if nest.state == "paused" { return .paused }
        switch nest.status {
        case "ready": return .ready
        case "working": return .working
        case "needs_you": return .attention
        case "paused": return .paused
        case nil: return .unknown
        default: return nest.statusReason == "setting_up" ? .setup : .unknown
        }
    }

    /// Events that have not ended yet, soonest first. Cancelled ones are left out.
    static func agenda(_ occurrences: [EventOccurrence], now: Date, zone: TimeZone) -> WidgetAgenda {
        let events = occurrences.compactMap { item -> WidgetEvent? in
            guard item.status != "cancelled", let span = interval(item, zone: zone), span.1 > now else { return nil }
            let (start, end) = span
            let title = item.title?.trimmingCharacters(in: .whitespacesAndNewlines)
            return WidgetEvent(eventId: item.eventId, occurrenceId: item.occurrenceId,
                               title: (title?.isEmpty == false ? title! : "Untitled event"),
                               startsAt: start, endsAt: end, allDay: item.allDay == true,
                               location: item.location.flatMap { $0.isEmpty ? nil : $0 },
                               hasMeeting: item.meeting?.enabled == true)
        }
        .sorted { ($0.startsAt, $0.id) < ($1.startsAt, $1.id) }
        return WidgetAgenda(events: Array(events.prefix(WidgetSnapshot.eventLimit)))
    }

    /// An occurrence's span. All-day dates are `yyyy-MM-dd` with an exclusive end, in the calendar's zone.
    static func interval(_ item: EventOccurrence, zone: TimeZone) -> (Date, Date)? {
        if item.allDay == true {
            var calendar = Calendar(identifier: .gregorian)
            calendar.timeZone = zone
            func day(_ text: String?) -> Date? {
                guard let text else { return nil }
                let parts = text.split(separator: "-").compactMap { Int($0) }
                guard parts.count == 3 else { return nil }
                return calendar.date(from: DateComponents(year: parts[0], month: parts[1], day: parts[2]))
            }
            guard let start = day(item.startDate) else { return nil }
            let end = day(item.endDate) ?? calendar.date(byAdding: .day, value: 1, to: start) ?? start
            return (start, max(end, start.addingTimeInterval(1)))
        }
        guard let first = item.startsAt else { return nil }
        let start = Date(timeIntervalSince1970: TimeInterval(first))
        let end = item.endsAt.map { Date(timeIntervalSince1970: TimeInterval($0)) } ?? start.addingTimeInterval(3600)
        return (start, max(end, start.addingTimeInterval(1)))
    }
}
