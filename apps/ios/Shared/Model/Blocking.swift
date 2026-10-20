//  Blocking (SDS §15): one decision, reachable from every place a person meets somebody — a
//  conversation's header, the sender panel, a message, a call in the log, a group's member list and
//  the Filtered list. Each of those builds a `BlockTarget` and presents `BlockSheet`; the writing
//  happens here, once.
//
//  A block is an ordinary contact row with `admission: block`. The postbox applies it to mail and
//  to calls alike, under the sender's handle and their `/k/` address both, and never tells them.

import Foundation

/// Who is about to be blocked, and where the request came from.
struct BlockTarget: Identifiable, Equatable {
    enum Origin: String { case thread, info, message, call, filtered, group }

    /// The peer key: a handle when there is one, otherwise the `/k/` address.
    let peer: String
    /// What to call them in the question.
    var name: String
    var origin: Origin
    /// The message to report with "Also report as spam", when there is a particular one.
    var messageId: String? = nil
    /// The incoming call to report instead, from the call log.
    var callId: String? = nil

    var id: String { origin.rawValue + ":" + peer }
    /// Reporting is the point of blocking from Filtered — that mail already looked unwanted to the
    /// postbox. Elsewhere it is an extra step somebody chooses.
    var reportByDefault: Bool { origin == .filtered }
}

enum Blocking {
    /// The rule covering a whole namespace, or nil where a wildcard would block strangers who merely
    /// share it: `/k/` addresses, the providers' namespaces (`/github`, `/pp`) and wildcards
    /// themselves. The web's `wildcardFor`, rule for rule.
    static func wildcard(for peer: String) -> String? {
        guard peer.hasPrefix("/"), !peer.hasPrefix("/k/"), !peer.hasSuffix("/*") else { return nil }
        let parts = peer.split(separator: "/").map(String.init).filter { !$0.isEmpty }
        guard !parts.isEmpty else { return nil }
        let parent = parts.count == 1 ? parts[0] : parts.dropLast().joined(separator: "/")
        if ["github", "pp", "k"].contains(parent) { return nil }
        return "/" + parent + "/*"
    }

    /// What the Blocked page's own field accepts: a `/k/` address, a handle, or `/ns/*`.
    static func validAddress(_ value: String) -> Bool {
        let pattern = #"^/(?:k/[a-z0-9]+|[^\s/*]+(?:/[^\s/*]+)*(?:/\*)?)$"#
        return value.range(of: pattern, options: [.regularExpression, .caseInsensitive]) != nil
    }

    /// Write the block. Returns an error to show, or nil when it was saved.
    @MainActor
    static func block(_ peer: String, inbox: Inbox) async -> String? {
        let existing = inbox.exactContact(peer)
        do {
            try await inbox.saveContact(peer: peer, alias: existing?.alias, admission: "block",
                                        autonomy: "review", allowedVerbs: [])
            return nil
        } catch let error as APIError {
            return "Could not block. " + (error.errorDescription ?? "Please try again.")
        } catch {
            return "Could not block. Please try again."
        }
    }

    /// The report that may follow a block: the call when it came from the log, otherwise the named
    /// message or the newest one this sender sent. Nothing to report is not an error.
    @MainActor
    static func report(_ target: BlockTarget, inbox: Inbox, filtered: [Message] = []) async -> String {
        guard let address = inbox.account.me?.address else { return "Blocked." }
        let client = inbox.account.client
        do {
            if let callId = target.callId {
                if !Fixtures.enabled { try await client.reportCall(identity: address, callId: callId) }
            } else if let id = target.messageId ?? (inbox.messages + filtered).last(where: {
                !$0.isOutgoing && $0.peerKey == target.peer
            })?.messageId {
                if !Fixtures.enabled { try await client.reportSpam(identity: address, messageId: id) }
            } else {
                return "Blocked. They won’t be able to message or call you."
            }
            return "Blocked and reported as spam."
        } catch {
            return "Blocked, but the spam report could not be sent."
        }
    }

    /// Lift a block: the row goes, and the sender is a stranger again under the mailbox's policy.
    @MainActor
    static func unblock(_ peer: String, inbox: Inbox) async -> Bool {
        do {
            if Fixtures.enabled {
                inbox.forgetFixtureContact(peer)
            } else {
                try await inbox.removeContact(peer: peer)
            }
            return true
        } catch {
            return false
        }
    }
}
