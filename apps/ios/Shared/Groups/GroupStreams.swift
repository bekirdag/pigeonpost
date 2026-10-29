//  Live streams in groups (Phase 6, docs/streaming.md), as the group screens see them.
//
//  The stream itself — going live, watching, the stage, reactions and the end-to-end encryption of
//  every frame — is the web client's (`site-inbox/live.js`), run in the call web view through
//  `PigeonpostNative.watchStream` / `goLive` (`CallHost`). What lives here is the group's side: is
//  somebody live in this group, what the banner says, and how the bridge's answers read.

import Foundation

/// `GET /v1/groups/{id}/streams` → `{streams: [stream]}`; the live stream, if any.
struct GroupStream: Decodable, Equatable, Identifiable {
    struct StageEntry: Decodable, Equatable {
        let address: String
        let state: String?
    }

    let id: String
    var groupId: String?
    var title: String?
    var sources: [String]?
    var by: String?
    var byHandle: String?
    var state: String?
    var startedAt: Int?
    var endsBy: Int?
    var viewers: Int?
    var stage: [StageEntry]?
    var revision: Int?

    var isLive: Bool { (state ?? "live") == "live" }
}

struct GroupStreamsResponse: Decodable { let streams: [GroupStream]? }

extension PostboxClient {
    /// The group's live stream, or nil. Active members only; `feature_disabled` when streaming is off.
    func liveStream(_ caller: GroupCaller, group: String) async throws -> GroupStream? {
        let path = "/v1/groups/" + PostboxClient.segment(group) + "/streams"
        let streams = try await send(path, headers: caller.headers(), as: GroupStreamsResponse.self).streams ?? []
        return streams.first(where: \.isLive)
    }
}

enum LiveStreams {
    /// The bridge validates ids before anything is sent; the app does the same so a fixture or an
    /// odd server answer never reaches the page.
    static func validGroup(_ id: String) -> Bool { matches(id, prefix: "grp_") }
    static func validStream(_ id: String) -> Bool { matches(id, prefix: "str_") }

    private static func matches(_ id: String, prefix: String) -> Bool {
        guard id.hasPrefix(prefix) else { return false }
        let rest = id.dropFirst(prefix.count)
        return rest.count == 26 && rest.allSatisfy { ("a"..."z").contains($0) || ("2"..."7").contains($0) }
    }

    /// "Jane is live: Sprint demo", as `groups.js` words it.
    static func bannerText(_ stream: GroupStream, me: String?, name: (String) -> String) -> String {
        let title = (stream.title ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let who: String
        if let by = stream.by, by == me { who = "You are" }
        else { who = (stream.by.map(name) ?? "Someone") + " is" }
        return title.isEmpty ? "\(who) live" : "\(who) live: \(title)"
    }

    /// The banner's button: Watch, Open (your own stream, e.g. after reopening the app), Return
    /// (this device is already in it).
    static func bannerAction(_ stream: GroupStream, me: String?, current: String?) -> String {
        if current == stream.id { return "Return" }
        if let by = stream.by, by == me { return "Open" }
        return "Watch"
    }

    /// Titles go to the postbox trimmed and at most 100 characters (docs/streaming.md §2).
    static func cleanTitle(_ raw: String) -> String {
        String(raw.trimmingCharacters(in: .whitespacesAndNewlines).prefix(100))
    }

    /// What a refusal from the bridge means to a person. `nil`: nothing to say (it worked, or the
    /// page already explained itself in the stream view or a toast of its own).
    static func explain(_ code: String?) -> String? {
        switch code {
        case nil, "cancelled": return nil
        case "signed_out": return "Sign in again to use live streams."
        case "bridge_unavailable", "not_ready": return "Live streams aren't ready yet. Try again in a moment, or close and reopen the app."
        case "needs_e2ee": return "Live streams need a newer version of this device's system: it can't encrypt the stream end to end."
        case "invalid_stream", "invalid_group": return "That stream can't be opened."
        case "busy": return "Finish the current call or stream first."
        case "camera_denied": return "Pigeonpost can't use your camera or microphone. Allow access in Settings to go live."
        default:
            // live.js has already shown its own message in the stream view for these.
            return nil
        }
    }
}
