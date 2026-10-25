//  Blocking, the writing half: the contact row, the optional report, and lifting it again.

import Foundation

extension Blocking {
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
