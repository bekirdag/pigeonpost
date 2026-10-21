//  Call history for the mailbox being read (SDS §12): the Calls page, its missed badge, and the
//  missed-call lines a 1:1 conversation shows.
//
//  The server keeps the log; this holds one page of it per filter and nothing else. Like `Inbox`, a
//  change of mailbox empties it before anything else can be drawn — another mailbox's missed calls
//  are not this one's.

import Foundation
import Observation

@MainActor
@Observable
final class CallLog {
    enum Filter: String, CaseIterable, Identifiable {
        case all, missed, filtered
        var id: String { rawValue }
        var title: String {
            switch self {
            case .all: return "All"
            case .missed: return "Missed"
            case .filtered: return "Filtered"
            }
        }
    }

    let account: Account

    var filter: Filter = .all {
        didSet { if filter != oldValue { entries = []; next = nil; loaded = false } }
    }
    private(set) var entries: [CallLogEntry] = []
    /// What the badge counts. The server's number, cleared by looking at the page.
    private(set) var unseen = 0
    private(set) var next: Int?
    private(set) var loaded = false
    private(set) var loading = false
    private(set) var error: String?
    /// Every incoming missed call in the newest page of everything, kept apart from `entries` so a
    /// conversation can show its missed-call lines whichever filter the Calls page is on.
    private(set) var recent: [CallLogEntry] = []

    var toast: String?

    @ObservationIgnored private var mailbox: String?

    init(account: Account) {
        self.account = account
    }

    private var client: PostboxClient { account.client }
    var enabled: Bool { account.features.callHistory }

    /// Empty for a mailbox it was not loaded for.
    private func follow() -> String? {
        let current = account.me?.address
        if current != mailbox {
            mailbox = current
            entries = []
            recent = []
            unseen = 0
            next = nil
            loaded = false
            error = nil
            filter = .all
        }
        return current
    }

    /// The badge and the conversation lines: the newest page of everything. Cheap, and run on every
    /// return to the foreground.
    func refresh() async {
        guard let address = follow(), enabled, !Fixtures.enabled else { return }
        do {
            let page = try await client.callLog(identity: address, filter: Filter.all.rawValue)
            guard address == account.me?.address else { return }
            recent = page.entries ?? []
            unseen = max(0, page.unseenMissed ?? 0)
            if filter == .all, !loading {
                entries = recent
                next = page.nextBefore
                loaded = true
            }
        } catch {
            // The badge simply stays what it was.
        }
    }

    /// The page itself: the current filter, from the top or the next page down.
    func load(more: Bool = false) async {
        guard let address = follow(), enabled else { return }
        guard !Fixtures.enabled else { applyFixture(); return }
        let wanted = filter
        loading = true
        defer { if address == account.me?.address { loading = false } }
        do {
            let page = try await client.callLog(identity: address, filter: wanted.rawValue,
                                                before: more ? next : nil)
            guard address == account.me?.address, wanted == filter else { return }
            let got = (page.entries ?? []).filter { !$0.id.isEmpty }
            if more {
                let seen = Set(entries.map(\.id))
                entries += got.filter { !seen.contains($0.id) }
            } else {
                entries = got
            }
            if wanted == .all, !more { recent = got }
            next = page.nextBefore
            unseen = max(0, page.unseenMissed ?? unseen)
            loaded = true
            error = nil
        } catch {
            guard address == account.me?.address else { return }
            self.error = "Could not load your calls."
        }
    }

    /// Looking at the page is seeing the missed calls on it.
    func markSeen() async {
        let newest = (entries + recent).map(\.startedAt).max() ?? 0
        guard unseen > 0, newest > 0, let address = account.me?.address else { return }
        unseen = 0
        guard !Fixtures.enabled else { return }
        if let left = try? await client.markCallsSeen(identity: address, upTo: newest),
           address == account.me?.address {
            unseen = left
        }
    }

    func delete(_ entry: CallLogEntry) async {
        guard let address = account.me?.address else { return }
        do {
            if !Fixtures.enabled { try await client.deleteCall(identity: address, id: entry.id) }
            entries.removeAll { $0.id == entry.id }
            recent.removeAll { $0.id == entry.id }
            toast = "Removed from call history."
        } catch {
            toast = "Could not remove that call."
        }
    }

    func clear() async {
        guard let address = account.me?.address else { return }
        do {
            if !Fixtures.enabled { try await client.clearCallLog(identity: address) }
            entries = []
            recent = []
            next = nil
            unseen = 0
            toast = "Call history cleared."
        } catch {
            toast = "Could not clear your call history."
        }
    }

    func report(_ entry: CallLogEntry) async {
        guard let address = account.me?.address else { return }
        do {
            if !Fixtures.enabled { try await client.reportCall(identity: address, callId: entry.id) }
            toast = "Reported as spam."
        } catch {
            toast = "Could not send the report."
        }
    }

    /// Missed calls from this peer, as lines for its conversation. Incoming, missed, one to one.
    func missedLines(for keys: Set<String>) -> [CallLogEntry] {
        guard enabled else { return [] }
        return CallLogEntry.missed(in: recent, keys: keys)
    }

    /// The missed-call lines for one conversation, ready to sort in among its messages.
    func lines(for conversation: Conversation?) -> [ThreadMessage] {
        guard let conversation else { return [] }
        var keys: Set<String> = [conversation.peer]
        for message in conversation.messages where message.kind == .incoming {
            if let address = message.address { keys.insert(address) }
        }
        return missedLines(for: keys).map(CallLogEntry.line)
    }

    // ---- fixtures ---------------------------------------------------------------------------------

    @ObservationIgnored private var fixture: [CallLogEntry]?

    private func applyFixture() {
        #if DEBUG
        if fixture == nil { fixture = Self.fixtureEntries() }
        let all = fixture ?? []
        switch filter {
        case .all: entries = all.filter { $0.outcome != "filtered" }
        case .missed: entries = all.filter(\.isMissed)
        case .filtered: entries = all.filter { $0.outcome == "filtered" }
        }
        loaded = true
        #endif
    }

    #if DEBUG
    func installFixture() {
        fixture = Self.fixtureEntries()
        mailbox = account.me?.address
        recent = (fixture ?? []).filter { $0.outcome != "filtered" }
        entries = recent
        unseen = 2
        loaded = true
    }

    static func fixtureEntries(now: Int = Int(Date().timeIntervalSince1970)) -> [CallLogEntry] {
        func entry(_ id: String, _ direction: String, _ outcome: String?, _ kind: String, _ peer: String?,
                   _ handle: String?, ago: Int, duration: Int? = nil, people: [String] = []) -> CallLogEntry {
            CallLogEntry(id: id, direction: direction, outcome: outcome, kind: kind, peer: peer, peerHandle: handle,
                         participants: people, groupId: nil, startedAt: now - ago, answeredAt: nil, endedAt: nil,
                         duration: duration, seen: false)
        }
        return [
            entry("c1", "incoming", "missed", "video", "/k/aaaa1111bbbb2222cccc3333dd", "/bekir/agent1", ago: 300),
            entry("c2", "outgoing", "answered", "audio", "/k/zz1111v2h90vnwefj7g7ezvbh9", "/bekir/docdex", ago: 5400, duration: 134),
            entry("c3", "incoming", "answered", "video", "/k/aaaa1111bbbb2222cccc3333dd", "/bekir/agent1", ago: 90000, duration: 3725),
            entry("c4", "incoming", "missed", "audio", "/k/eeee5555ffff6666gggg7777hh", nil, ago: 200000),
            entry("c5", "outgoing", "missed", "audio", "/k/zz1111v2h90vnwefj7g7ezvbh9", "/bekir/docdex", ago: 260000),
            entry("c6", "incoming", "filtered", "audio", "/k/spam0000spam0000spam0000sp", nil, ago: 1200),
        ]
    }
    #endif
}
