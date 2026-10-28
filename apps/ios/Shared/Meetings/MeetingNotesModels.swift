//  Meeting notes (Phase 7, docs/meeting-summaries.md): what the postbox lists, and the brief a
//  device opens with its own notes key. The web reader is `site-inbox/meeting-notes.js`; the words
//  here are its words.

import Foundation

/// `GET /v1/meetings` → `{meetings: [...]}`: the meetings whose notes this account agreed to.
struct MeetingSummary: Decodable, Identifiable, Equatable {
    let id: String
    var kind: String?
    /// active, processing, ready or failed.
    var state: String
    var createdAt: Int
    var endedAt: Int?
    var readyAt: Int?
    var expiresAt: Int
    var host: String?
    /// The mailbox this account attended as.
    var `as`: String?
    var failure: String?
}

struct MeetingList: Decodable { let meetings: [MeetingSummary]? }

/// The decrypted brief (meeting-intel `service.js`). Every field optional: a later brief version may
/// add or drop one, and the reader must still show what it can.
struct MeetingBrief: Decodable, Equatable {
    struct ActionItem: Decodable, Equatable {
        var owner: String?
        var task: String?
        var due: String?
    }
    struct Line: Decodable, Equatable {
        var at: String?
        var speaker: String?
        var text: String?
    }
    struct GeneratedBy: Decodable, Equatable {
        var stt: String?
        var llm: String?
    }

    var title: String?
    var language: String?
    var startedAt: String?
    var durationS: Int?
    var participants: [String]?
    var summary: String?
    var decisions: [String]?
    var actionItems: [ActionItem]?
    var openQuestions: [String]?
    var draft: Bool?
    var transcript: [Line]?
    var generatedBy: GeneratedBy?

    static func decode(_ data: Data) throws -> MeetingBrief {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(MeetingBrief.self, from: data)
    }
}

/// This device's notes key, as the call page holds it (public half only).
struct MeetingNotesKey: Equatable {
    let id: String
    let publicKey: String
    let label: String
}

enum MeetingNotesText {
    static let intro = "Notes from calls where you agreed to them. They're encrypted for your devices that were in the call or set up for notes; Pigeonpost's servers keep only the encrypted copy, and delete it after the retention date."
    static let otherDevices = "These notes were encrypted for your other devices. Open them on a device that was in the call, or one that opened Meeting notes before the call ended."
    static let cannotOpen = "This device couldn't open these notes."
    static let preparing = "The notes are still being prepared. Check back in a few minutes."
    static let gone = "These notes are no longer available."
    static let draft = "A summary couldn't be generated for this meeting. The transcript is below."
    static let footer = "Transcribed and summarized automatically on Pigeonpost's server. Speaker names come from each person's own microphone; a microphone can pick up other voices."

    static func state(_ state: String) -> String {
        switch state {
        case "active": return "Notes in progress"
        case "processing": return "Being prepared…"
        case "ready": return "Ready"
        case "failed": return "Couldn't be made"
        default: return state
        }
    }

    static func failure(_ code: String?) -> String {
        switch code {
        case "no_speech": return "No speech was captured."
        case "no_recipients": return "No device could receive the notes."
        case "stt_unavailable": return "Transcription was unavailable."
        case "timed_out": return "The notes service didn't finish in time."
        default: return "The notes couldn't be made."
        }
    }

    static func when(_ seconds: Int) -> String {
        Date(timeIntervalSince1970: TimeInterval(seconds)).formatted(date: .abbreviated, time: .shortened)
    }

    /// "Ready · host /alp/main · kept until 25 Jan 2027".
    static func meta(_ m: MeetingSummary) -> String {
        var out = state(m.state)
        if let host = m.host { out += " · host \(host)" }
        if m.state == "ready" {
            out += " · kept until " + Date(timeIntervalSince1970: TimeInterval(m.expiresAt)).formatted(date: .abbreviated, time: .omitted)
        }
        return out
    }

    /// When it started, how long, who spoke.
    static func header(_ b: MeetingBrief) -> String {
        var bits: [String] = []
        if let started = b.startedAt, let date = ISO8601DateFormatter.fractional.date(from: started) ?? ISO8601DateFormatter().date(from: started) {
            bits.append(date.formatted(date: .abbreviated, time: .shortened))
        }
        if let d = b.durationS, d > 0 { bits.append("\(max(1, Int((Double(d) / 60).rounded()))) min") }
        if let p = b.participants, !p.isEmpty { bits.append(p.joined(separator: ", ")) }
        return bits.joined(separator: " · ")
    }

    static func action(_ a: MeetingBrief.ActionItem) -> String {
        [a.owner.map { $0 + ":" }, a.task, a.due.map { "(due \($0))" }].compactMap { $0 }.joined(separator: " ")
    }

    static func generated(_ b: MeetingBrief) -> String {
        guard let stt = b.generatedBy?.stt else { return footer }
        let engines = stt + (b.generatedBy?.llm.map { ", " + $0 } ?? "")
        return footer.replacingOccurrences(of: "server.", with: "server (\(engines)).")
    }

    /// "Copy as text", line for line as the web's `asText`.
    static func plain(_ b: MeetingBrief) -> String {
        var out = [b.title ?? "Meeting notes", ""]
        if let s = b.summary { out += [s, ""] }
        func list(_ title: String, _ items: [String]) {
            guard !items.isEmpty else { return }
            out.append(title + ":")
            out += items.map { "- " + $0 }
            out.append("")
        }
        list("Decisions", b.decisions ?? [])
        list("Action items", (b.actionItems ?? []).map(action))
        list("Open questions", b.openQuestions ?? [])
        list("Transcript", (b.transcript ?? []).map { "[\($0.at ?? "")] \($0.speaker ?? ""): \($0.text ?? "")" })
        return out.joined(separator: "\n")
    }
}

extension ISO8601DateFormatter {
    static let fractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()
}
