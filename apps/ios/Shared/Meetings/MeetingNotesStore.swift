//  The Meeting notes reader's state: the list, one opened brief, registering this device's key.
//
//  Keys: a brief is wrapped by meeting-intel for each notes key the account registered. This app's
//  key is the call page's own (`calls-summary.js` NotesKeys, kept in that page's storage on this
//  device) — the key a member's Agree registers during a call — so notes from calls taken in this
//  app open here, and opening the reader registers it for calls taken elsewhere, as the web inbox
//  does. The private half never leaves the page: the page unwraps the brief and hands back its JSON.

import Foundation
import Observation

/// Where this device's notes key lives (the call page, in the app).
@MainActor
protocol MeetingNotesKeys: AnyObject {
    func meetingNotesKey() async throws -> MeetingNotesKey
    /// Unwrap `GET /v1/meetings/{id}`'s brief with this device's key; the plaintext JSON.
    func openMeetingBrief(_ meeting: [String: Any]) async throws -> Data
    /// The page's refusal code, when it refused.
    func refusal(_ error: Error) -> String?
}

@MainActor
@Observable
final class MeetingNotesStore {
    enum ListState: Equatable {
        case loading
        case loaded([MeetingSummary])
        case failed
    }
    enum Opened: Equatable {
        case loading
        case brief(MeetingSummary, MeetingBrief)
        /// Not ready yet, or failed: the meeting's own state.
        case pending(MeetingSummary)
        /// Wrapped only for the account's other devices.
        case otherDevices(MeetingSummary)
        case cannotOpen(MeetingSummary?)
        case gone
    }

    private(set) var list: ListState = .loading
    private(set) var opened: [String: Opened] = [:]
    private(set) var registered = false
    var toast: String?

    @ObservationIgnored private let client: () -> PostboxClient?
    @ObservationIgnored private weak var keys: MeetingNotesKeys?
    @ObservationIgnored private let fixtures: Bool
    @ObservationIgnored private var listRun = UUID()

    init(client: @escaping () -> PostboxClient?, keys: MeetingNotesKeys?, fixtures: Bool = false) {
        self.client = client
        self.keys = keys
        self.fixtures = fixtures
    }

    // ---- the list ----------------------------------------------------------------------------

    func load() async {
        let run = UUID()
        listRun = run
        if fixtures { list = .loaded(Self.fixtureList()); return }
        guard let client = client() else { list = .failed; return }
        do {
            let rows = try await client.send("/v1/meetings", as: MeetingList.self).meetings ?? []
            guard listRun == run else { return }
            list = .loaded(rows.sorted { $0.createdAt > $1.createdAt })
        } catch {
            guard listRun == run else { return }
            list = .failed
        }
    }

    /// Register this device's notes key for the account (once per reader), so notes made later in
    /// calls taken on other devices are wrapped for it too. Quiet on failure; tried again next time.
    func register() async {
        guard !registered, !fixtures, let keys, let client = client() else { return }
        do {
            let key = try await keys.meetingNotesKey()
            _ = try await client.sendRaw("/v1/meetings/keys/" + PostboxClient.segment(key.id), method: "PUT",
                                         json: ["public_key": key.publicKey, "label": key.label])
            registered = true
        } catch {
            registered = false
        }
    }

    // ---- one meeting -------------------------------------------------------------------------

    func open(_ id: String) async {
        opened[id] = .loading
        if fixtures { opened[id] = Self.fixtureOpen(id); return }
        guard let client = client() else { opened[id] = .cannotOpen(nil); return }
        let data: Data
        do {
            data = try await client.sendRaw("/v1/meetings/" + PostboxClient.segment(id))
        } catch {
            opened[id] = .gone
            return
        }
        guard let raw = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let summary = try? PostboxClient.decode(MeetingSummary.self, from: data) else {
            opened[id] = .cannotOpen(nil)
            return
        }
        guard summary.state == "ready" else { opened[id] = .pending(summary); return }
        guard let keys else { opened[id] = .cannotOpen(summary); return }
        // Only what the page needs to open it.
        var meeting: [String: Any] = ["id": summary.id]
        for field in ["iv", "ciphertext", "alg"] { if let value = raw[field] as? String { meeting[field] = value } }
        meeting["keys"] = (raw["keys"] as? [[String: Any]] ?? []).map { wrap in
            wrap.filter { ["key_id", "epk", "iv", "wrapped"].contains($0.key) && $0.value is String }
        }
        do {
            let plain = try await keys.openMeetingBrief(meeting)
            opened[id] = .brief(summary, try MeetingBrief.decode(plain))
        } catch {
            opened[id] = keys.refusal(error) == "no_key_for_device" ? .otherDevices(summary) : .cannotOpen(summary)
        }
    }

    /// "Delete my copy": other attendees keep theirs.
    func delete(_ id: String) async -> Bool {
        if fixtures {
            if case let .loaded(rows) = list { list = .loaded(rows.filter { $0.id != id }) }
            opened[id] = nil
            toast = "Deleted."
            return true
        }
        guard let client = client() else { return false }
        do {
            _ = try await client.sendRaw("/v1/meetings/" + PostboxClient.segment(id), method: "DELETE")
            if case let .loaded(rows) = list { list = .loaded(rows.filter { $0.id != id }) }
            opened[id] = nil
            toast = "Deleted."
            return true
        } catch {
            toast = "Couldn't delete."
            return false
        }
    }

    // ---- fixtures (DEBUG screens and UI tests; synthetic, no network) ------------------------

    static let fixtureReady = "mtg_fixtureready00000000000000"
    static let fixtureOther = "mtg_fixtureother00000000000000"
    static let fixturePending = "mtg_fixturepending000000000000"

    static func fixtureList() -> [MeetingSummary] {
        let now = Int(Date().timeIntervalSince1970)
        return [
            MeetingSummary(id: fixtureReady, kind: "group", state: "ready", createdAt: now - 3600, endedAt: now - 1800,
                           readyAt: now - 1700, expiresAt: now + 89 * 86400, host: "/alp/main", as: nil, failure: nil),
            MeetingSummary(id: fixturePending, kind: "direct", state: "processing", createdAt: now - 600, endedAt: now - 60,
                           readyAt: nil, expiresAt: now + 90 * 86400, host: "/ozgur/main", as: nil, failure: nil),
            MeetingSummary(id: fixtureOther, kind: "direct", state: "ready", createdAt: now - 86400 * 3, endedAt: now - 86000 * 3,
                           readyAt: now - 85000 * 3, expiresAt: now + 87 * 86400, host: "/alp/main", as: nil, failure: nil),
        ]
    }

    static func fixtureOpen(_ id: String) -> Opened {
        guard let summary = fixtureList().first(where: { $0.id == id }) else { return .gone }
        switch id {
        case fixtureReady:
            let brief = MeetingBrief(
                title: "Release planning", language: "en", startedAt: "2026-10-27T09:00:00.000Z", durationS: 1520,
                participants: ["/alp/main", "/bekir/main"],
                summary: "The team agreed to ship 0.9.1 on Thursday after the calendar fix lands.",
                decisions: ["Ship 0.9.1 on Thursday", "Keep the old export for one more release"],
                actionItems: [.init(owner: "/alp/main", task: "Land the calendar fix", due: "Wednesday"),
                              .init(owner: "/bekir/main", task: "Write the release notes", due: nil)],
                openQuestions: ["Do we need a migration note for Windows?"],
                draft: false,
                transcript: [.init(at: "00:04", speaker: "/alp/main", text: "Let's go through the release."),
                             .init(at: "00:19", speaker: "/bekir/main", text: "Thursday works if the fix is in.")],
                generatedBy: .init(stt: "whisper.cpp small", llm: "qwen"))
            return .brief(summary, brief)
        case fixtureOther: return .otherDevices(summary)
        default: return .pending(summary)
        }
    }
}
