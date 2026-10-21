//  Phase 1b shapes and the pure rules over them — kept free of the app's live objects so the
//  hand-run tests (Tests/run.sh) can compile them alongside the thread model.

import Foundation

/// One row of `GET /v1/calls/log`.
struct CallLogEntry: Decodable, Identifiable, Equatable {
    let id: String
    /// `incoming` or `outgoing`.
    let direction: String
    /// `answered`, `missed`, `declined`, `cancelled`, `busy`, `filtered`, `failed`, or absent while
    /// the call is still going.
    let outcome: String?
    /// `audio` or `video`.
    let kind: String
    let peer: String?
    let peerHandle: String?
    let participants: [String]?
    let groupId: String?
    let startedAt: Int
    let answeredAt: Int?
    let endedAt: Int?
    let duration: Int?
    let seen: Bool?

    var isIncoming: Bool { direction == "incoming" }
    var isVideo: Bool { kind == "video" }
    /// Who to call back and whose conversation to open: the handle when there is one.
    var peerKey: String? { peerHandle ?? peer }
    /// An incoming call nobody picked up. A caller who gave up before it was answered is a missed
    /// call to the person it rang, whatever the caller's side calls it.
    var isMissed: Bool { isIncoming && (outcome == "missed" || outcome == "cancelled") }

    /// The words under the name, as the web says them.
    var outcomeLabel: String {
        switch outcome {
        case "answered": return isIncoming ? "Incoming" : "Outgoing"
        case "missed": return isIncoming ? "Missed" : "No answer"
        case "declined": return "Declined"
        case "cancelled": return isIncoming ? "Missed" : "Cancelled"
        case "busy": return "Busy"
        case "filtered": return "Filtered"
        case "failed": return "Failed"
        default: return "In progress"
        }
    }

    var talkedFor: Int? {
        if let duration { return duration }
        if let answeredAt, let endedAt { return max(0, endedAt - answeredAt) }
        return nil
    }

    /// `1:05`, `1:02:09`.
    static func formatDuration(_ seconds: Int) -> String {
        let total = max(0, seconds)
        let h = total / 3600, m = (total % 3600) / 60, s = total % 60
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }

    /// "↙ Missed · Video · 2:14 · 3 people".
    var summary: String {
        var bits = [outcomeLabel, isVideo ? "Video" : "Voice"]
        if outcome == "answered", let talkedFor { bits.append(Self.formatDuration(talkedFor)) }
        let people = participants?.count ?? 0
        if groupId != nil || people > 2 { bits.append("\(people) people") }
        return (isIncoming ? "↙ " : "↗ ") + bits.joined(separator: " · ")
    }
}

struct CallLogPage: Decodable {
    let entries: [CallLogEntry]?
    let unseenMissed: Int?
    let nextBefore: Int?
}


/// `GET /v1/policy`: the stranger defaults and the Phase 1b filters. Only the six fields Settings
/// shows are read; the rest of the object is the server's and is never written back.
struct InboxFilters: Decodable, Equatable {
    var unknownCalls: String?
    var unnamedCalls: String?
    var unnamedMessages: String?
    var unknownMessages: String?
    var lowReputation: String?
    var callHistoryDays: Int?

    subscript(field: String) -> String? {
        switch field {
        case "unknown_calls": return unknownCalls
        case "unnamed_calls": return unnamedCalls
        case "unnamed_messages": return unnamedMessages
        case "unknown_messages": return unknownMessages
        case "low_reputation": return lowReputation
        case "call_history_days": return callHistoryDays.map(String.init)
        default: return nil
        }
    }

    mutating func set(_ field: String, _ value: String) {
        switch field {
        case "unknown_calls": unknownCalls = value
        case "unnamed_calls": unnamedCalls = value
        case "unnamed_messages": unnamedMessages = value
        case "unknown_messages": unknownMessages = value
        case "low_reputation": lowReputation = value
        case "call_history_days": callHistoryDays = Int(value)
        default: break
        }
    }
}

/// A picture as `GET /v1/avatars` and `GET /v1/profile/avatar` describe it.
struct AvatarInfo: Decodable, Equatable {
    let id: String?
    let version: Int?
    let hash: String?
    let urls: [String: String]?
    /// Only on your own: `everyone`, `known` or `nobody`.
    let visibility: String?

    /// What identifies these bytes: changes whenever the picture or its visibility does.
    var key: String { hash ?? "\(id ?? "?"):\(version ?? 0)" }
    /// A small size for lists, a bigger one for Settings. WebP, which ImageIO reads.
    var small: String? { urls?["128"] ?? urls?["256"] ?? urls?["jpeg"] }
    var large: String? { urls?["256"] ?? urls?["512"] ?? urls?["128"] ?? urls?["jpeg"] }
}

extension CallLogEntry {
    static func line(_ entry: CallLogEntry) -> ThreadMessage {
        ThreadMessage(id: "call:" + entry.id, kind: .system, at: entry.startedAt,
                      body: "Missed \(entry.isVideo ? "video" : "voice") call · \(Time.clockTime(entry.startedAt))",
                      threadId: nil)
    }

    /// `messages` with `lines` in their places by time.
    static func merge(_ messages: [ThreadMessage], _ lines: [ThreadMessage]) -> [ThreadMessage] {
        guard !lines.isEmpty else { return messages }
        return (messages + lines).sorted { $0.at < $1.at }
    }

    /// Pure, for the tests: which entries become a "Missed call" line in a conversation with
    /// `keys` (its handle, plus the addresses its mail came from).
    static func missed(in entries: [CallLogEntry], keys: Set<String>) -> [CallLogEntry] {
        entries.filter { entry in
            entry.isIncoming && entry.outcome == "missed" && entry.groupId == nil
                && (entry.peerHandle.map(keys.contains) ?? false || entry.peer.map(keys.contains) ?? false)
        }
    }
}

/// One sender's filtered mail.
struct FilteredSender: Identifiable, Equatable {
    let peer: String
    var messages: [Message]
    var last: Int
    var id: String { peer }
    /// The address their latest message came from, which is what `accept` names.
    var from: String { messages.last?.from ?? peer }
}

extension FilteredSender {
    static func group(_ messages: [Message], contacts: [Contact]) -> [FilteredSender] {
        var order: [String] = []
        var byPeer: [String: FilteredSender] = [:]
        for message in messages where !message.isOutgoing {
            let key = message.peerKey
            if ConversationBuilder.contact(for: key, in: contacts)?.admission == "block" { continue }
            if byPeer[key] == nil {
                byPeer[key] = FilteredSender(peer: key, messages: [], last: 0)
                order.append(key)
            }
            byPeer[key]?.messages.append(message)
            let last = max(byPeer[key]?.last ?? 0, message.at)
            byPeer[key]?.last = last
        }
        return order.compactMap { key in
            guard var sender = byPeer[key] else { return nil }
            sender.messages.sort { $0.at < $1.at }
            return sender
        }.sorted { $0.last > $1.last }
    }
}
