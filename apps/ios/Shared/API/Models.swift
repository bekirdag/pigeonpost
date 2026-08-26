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
    /// Files that came with it. Absent on messages that carry none, which is most of them.
    let attachments: [MessageAttachment]?

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

/// The reply to `POST /v1/identities`. It also carries a one-time capability token, which this app
/// has no use for — see `PostboxClient.createIdentity`.
struct CreatedIdentity: Decodable {
    let address: String
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

/// How full a mailbox is.
///
/// The warning threshold is the server's, deliberately: it can move without shipping an app, and
/// both clients agree on when to start saying something.
struct Quota: Decodable {
    let usedBytes: Int
    let limitBytes: Int
    let warnAtBytes: Int
    let tier: String

    var fraction: Double {
        guard limitBytes > 0 else { return 0 }
        return min(1, Double(usedBytes) / Double(limitBytes))
    }

    var shouldWarn: Bool { usedBytes >= warnAtBytes }
    var isFull: Bool { usedBytes >= limitBytes }
    var canBuyMoreRoom: Bool { tier != "paid" }

    private static let formatter: ByteCountFormatter = {
        let f = ByteCountFormatter()
        // Binary, to match how the quota is computed. `.file` counts a megabyte as 1,000,000
        // bytes, so a 20 MB limit set as 20 × 1024 × 1024 renders as "21 MB" — a number nobody
        // configured, in the one place somebody would quote it back to you.
        f.countStyle = .binary
        f.allowedUnits = [.useKB, .useMB, .useGB]
        return f
    }()

    var used: String { Self.formatter.string(fromByteCount: Int64(usedBytes)) }
    var limit: String { Self.formatter.string(fromByteCount: Int64(limitBytes)) }
}

/// A file on a message.
struct MessageAttachment: Decodable, Equatable, Identifiable {
    let id: String
    let filename: String
    let mediaType: String
    let bytes: Int

    /// What to show beside the name. Decimal units, matching how a phone reports file sizes
    /// everywhere else it shows one.
    var readableSize: String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        formatter.allowedUnits = [.useKB, .useMB, .useGB]
        return formatter.string(fromByteCount: Int64(bytes))
    }

    /// Which SF Symbol reads as this kind of file. Deliberately coarse — the point is to tell a
    /// picture from a document at a glance, not to name every format.
    var symbol: String {
        if mediaType.hasPrefix("image/") { return "photo" }
        if mediaType.hasPrefix("video/") { return "film" }
        if mediaType.hasPrefix("audio/") { return "waveform" }
        if mediaType == "application/pdf" { return "doc.richtext" }
        return "doc"
    }
}

/// What `POST /v1/attachments` answers with.
struct UploadedAttachment: Decodable {
    let id: String
    let filename: String
    let mediaType: String
    let bytes: Int
}

/// What the postbox will sell, and what this account already holds.
///
/// `namespace` is `nil` until something has been bought. Both the POST and the GET answer in this
/// shape, so a fresh purchase and a restored one are read by the same code.
struct HandleOffer: Decodable {
    let productId: String?
    let namespace: String?
    let expiresAt: Int?

    var owned: Bool { namespace != nil }

    var renewsOn: Date? { expiresAt.map { Date(timeIntervalSince1970: TimeInterval($0)) } }
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

    /// What to call this verb to a person.
    ///
    /// The wire name is a protocol token; showing it raw made a message somebody typed look like an
    /// envelope they had not asked for. Unknown verbs keep their wire name rather than being hidden
    /// — a verb this build does not know is still worth seeing exactly as it arrived.
    var title: String {
        switch verb {
        case "full_access": return "Full permissions"
        case "make_change": return "Do this work"
        case "report_status": return "Report status"
        case "answer_question": return "Answer a question"
        case "run_tests": return "Run the tests"
        case "read_file": return "Read a file"
        case "git_push": return "Push"
        case "deploy": return "Deploy"
        default: return verb
        }
    }

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
    ///
    /// `full_access` rather than `make_change`, because a person messaging their own fleet is not
    /// asking for a scoped subset of the job — they are asking for it to be finished. Under
    /// `make_change` an agent would do the work, commit it, and stop short of publishing, which
    /// read as "I am not allowed" when the truth was that nobody had said it was allowed.
    static func work(_ text: String) -> String {
        let envelope: [String: Any] = [
            "v": 1,
            "verb": "full_access",
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
