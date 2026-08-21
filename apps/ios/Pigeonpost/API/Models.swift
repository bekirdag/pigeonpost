//  The postbox's response shapes.
//
//  Copied from `do_inbox`, `do_list_identities`, `whoami` and `do_list_contacts`, and kept honest by
//  the same fixtures the web app's tests use (site-inbox/test/app.test.mjs).
//
//  Almost everything is optional. That is not defensiveness for its own sake: an app that has been
//  on a phone for six months talks to whatever the postbox was last deployed as, and a field added
//  since — `thread_id` was, `peer` was — must not turn a whole mailbox into a decoding failure.

import Foundation

struct Message: Decodable, Identifiable, Equatable {
    let messageId: String
    let from: String?
    let to: String?
    let body: String
    /// `in` or `out`. Absent from a postbox older than sent copies, which only ever returned
    /// received mail — so absence reads as `in`.
    let direction: String?
    /// The other end of the conversation, whichever way it went. The server says so directly; the
    /// handle is what trust matches on and what a person recognises.
    let peer: String?
    let peerHandle: String?
    let senderHandle: String?
    let threadId: String?
    let receivedAt: Int?
    let sentAt: Int?
    let read: Bool?
    /// `auto` or `review` on received mail. Always absent on a sent copy: your own words were never
    /// subject to an admission decision, and a plausible-looking `review` on them would be a lie.
    let autonomy: String?
    let verb: String?
    let heldBecause: String?
    let alias: String?
    let matchedContact: String?
    let senderStanding: String?
    let senderTier: String?
    let senderKnown: Bool?
    let untrusted: Bool?

    var id: String { messageId }
    var isOutgoing: Bool { direction == "out" }
    var at: Int { isOutgoing ? (sentAt ?? receivedAt ?? 0) : (receivedAt ?? sentAt ?? 0) }
    var isRead: Bool { read ?? false }

    /// Who this message is a conversation *with*, keyed the way trust is: on the handle when there
    /// is one, on the key address when there is not.
    var peerKey: String { peerHandle ?? peer ?? senderHandle ?? from ?? "unknown" }
}

struct InboxPolicy: Decodable, Equatable {
    let acceptAll: Bool?
    let autoAcceptKnown: Bool?
}

struct InboxResponse: Decodable {
    let messages: [Message]?
    let policy: InboxPolicy?
}

struct IdentityRow: Decodable, Equatable {
    let address: String
    /// The operator's own label for the mailbox. Local; trust never matches on it.
    let label: String?
}

struct IdentitiesResponse: Decodable {
    let identities: [IdentityRow]?
}

struct WhoAmI: Decodable {
    let address: String?
    let handle: String?
}

/// One mailbox of the account, once its handle has been resolved.
struct Mailbox: Identifiable, Equatable, Hashable {
    let address: String
    let handle: String?
    let label: String?

    var id: String { address }
    /// What trust matches on, and what the app keys conversations by.
    var key: String { handle ?? address }
}

struct ServerThread: Decodable, Equatable {
    let threadId: String
    let peer: String
    let title: String?
    let isDefault: Bool?
    let createdAt: Int?
    let lastAt: Int?
    let archived: Bool?
}

struct ThreadsResponse: Decodable {
    let threads: [ServerThread]?
}

struct Contact: Decodable, Equatable {
    /// An address, or a whole fleet as `/namespace/*`.
    let peer: String
    let alias: String?
    /// `allow` or `block`.
    let admission: String
    /// `review` or `auto`.
    let autonomy: String
    let allowedVerbs: [String]?

    var isWildcard: Bool { peer.hasSuffix("/*") }
}

/// Which verbs this postbox will let a mailbox grant, and which it refuses to whoever asks. The
/// server settles this; the app only shows what it said.
struct Vocabulary: Decodable, Equatable {
    let grantable: [String]?
    let neverAuto: [String]?
}

struct ContactsResponse: Decodable {
    let contacts: [Contact]?
    let policy: InboxPolicy?
    let vocabulary: Vocabulary?
}

struct ArchiveResponse: Decodable {
    let archived: [String]?
}

struct SendResponse: Decodable {
    let messageId: String?
    /// The id of the server's own copy of what was just sent. Holding it is what lets the
    /// optimistic row retire the moment that copy comes back, instead of the message appearing
    /// twice for one poll.
    let sentCopyId: String?
}

struct OpenedThread: Decodable {
    let threadId: String
}

/// An answer a mailbox's agent sent without anyone reading it first.
///
/// These arrive as plain text with two machine-readable lines on the front. Shown verbatim they
/// bury the answer under its own envelope, which is what a whole fleet's replies looked like before
/// this existed.
struct AutoReply: Equatable {
    let answered: String?
    let failed: Bool
    /// The answer itself, with the header lines removed.
    let body: String

    init?(body: String) {
        guard body.hasPrefix("pigeonpost-auto-reply v1") else { return nil }
        var lines = body.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        let header = lines.removeFirst()
        // The second line is the standing disclaimer, and it says the same thing every time.
        if lines.first?.hasPrefix("Generated unattended") == true { lines.removeFirst() }
        while lines.first?.trimmingCharacters(in: .whitespaces).isEmpty == true { lines.removeFirst() }

        answered = header
            .split(separator: " ")
            .first { $0.hasPrefix("answered=") }
            .map { String($0.dropFirst("answered=".count)) }
        failed = header.contains("outcome=failed")
        self.body = lines.joined(separator: "\n")
    }
}

/// A scoped request, as it travels: JSON in the body of an ordinary message. Rendered as what it
/// asks for rather than as the envelope it is.
struct RequestEnvelope: Equatable {
    let verb: String
    let args: [String: String]
    let note: String?

    /// What this app sends: the most it can ask for.
    ///
    /// There is no picker, deliberately, and the web inbox made the same call. Choosing a verb is a
    /// decision about somebody else's machine taken by the person with the least information — the
    /// sender cannot see what the recipient granted, what permission tier its route runs at, or
    /// which branches it allows. Guessing low only guarantees the message sits in review.
    ///
    /// So ask for the most and let the recipient decide. Its grant, its tier, its branch allowlist
    /// and its daily ceiling all still apply, and anything it will not do is held for a human
    /// exactly as prose used to be. Agent-to-agent traffic still uses the narrower verbs; they are
    /// a protocol, not a interface.
    static func work(_ text: String) -> String {
        let envelope: [String: Any] = [
            "v": 1,
            "verb": "make_change",
            "args": ["task": text],
            // The typed words survive whatever happens to the verb, and tell the agent why it was
            // asked rather than only what.
            "note": text,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: envelope),
              let json = String(data: data, encoding: .utf8)
        else { return text }
        return json
    }

    /// Parsed only when it really is one. Prose that happens to start with a brace is prose.
    init?(body: String) {
        guard body.first == "{", let data = body.data(using: .utf8),
              let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              (root["v"] as? Int) == 1,
              let verb = root["verb"] as? String
        else { return nil }
        self.verb = verb
        self.note = root["note"] as? String
        var args: [String: String] = [:]
        for (key, value) in (root["args"] as? [String: Any]) ?? [:] {
            args[key] = String(describing: value)
        }
        self.args = args
    }
}
