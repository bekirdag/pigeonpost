//  Groups (Phase 3), as the postbox describes them and as this device remembers them.
//
//  Wire shapes are docs/groups.md §2–§3, decoded with the client's snake-case strategy and as
//  optional as the rest of `Models.swift`, for the same reason: a field added to the postbox later
//  must not turn a whole list into a decoding failure. What this device keeps of a group — the
//  decrypted history — is `GroupItem`, which is the web client's history record
//  (site-inbox/groups-core.js `remember`) in Swift.

import CryptoKit
import Foundation

// ---- the postbox's groups -----------------------------------------------------------------------

struct GroupSummary: Decodable, Identifiable, Equatable, Hashable {
    let id: String
    var name: String
    var description: String?
    var joinPolicy: String?
    var postPolicy: String?
    var visibility: String?
    var membersCanInvite: Bool?
    var membersCanChangeTimer: Bool?
    var ephemeralSecs: Int?
    var revision: Int?
    var epoch: Int?
    var activationState: String?
    var orgId: String?
    var memberCount: Int?
    var maxMembers: Int?
    var createdBy: String?
    var createdAt: Int?
    var lastAt: Int?
    var lastSeq: Int?
    var deletedAt: Int?
    var hasLink: Bool?
    var my: GroupMembership?

    var role: String { my?.role ?? "member" }
    var state: String { my?.state ?? "active" }
    var isAdmin: Bool { role == "owner" || role == "admin" }
    var isOwner: Bool { role == "owner" }
    var isActive: Bool { state == "active" }
    var isInvited: Bool { state == "invited" }
    var isKeying: Bool { state == "keying" }
    var isPublic: Bool { joinPolicy == "public" }
    var unread: Int { my?.unread ?? 0 }
    var muted: Bool { (my?.mutedUntil ?? 0) > Int(Date().timeIntervalSince1970) }
    var members: Int { max(memberCount ?? 1, 1) }

    /// Whether this mailbox may send here: active, not restricted, and allowed by the posting rule.
    var canPost: Bool {
        guard isActive, role != "restricted" else { return false }
        return postPolicy == "admins" ? isAdmin : true
    }
}

struct GroupMembership: Decodable, Equatable, Hashable {
    var role: String?
    var state: String?
    var readCursor: Int?
    var unread: Int?
    var notify: String?
    var mutedUntil: Int?
    var invitedBy: String?
    var joinedAt: Int?
    var filtered: Bool?
    var revision: Int?
}

struct GroupsResponse: Decodable { let groups: [GroupSummary]? }
struct GroupEnvelope: Decodable { let group: GroupSummary }

struct CreatedGroup: Decodable {
    let group: GroupSummary
    let invitations: [InvitationResult]?
}

struct InvitationResult: Decodable, Equatable {
    let address: String
    let status: String
}

struct InviteResults: Decodable { let results: [InvitationResult]? }

struct GroupMember: Decodable, Identifiable, Equatable, Hashable {
    let address: String
    var handle: String?
    var role: String?
    var state: String?
    var invitedBy: String?
    var joinedAt: Int?
    var revision: Int?

    var id: String { address }
    /// What a person recognises, and what blocking and avatars key on.
    var key: String { handle ?? address }
}

struct MembersResponse: Decodable { let members: [GroupMember]? }

struct GroupMemberReceipt: Decodable, Equatable, Identifiable {
    let address: String
    var handle: String?
    var deliveredSeq: Int?
    var seenSeq: Int?
    var id: String { address }
}

struct GroupReceipts: Decodable, Equatable {
    var members: [GroupMemberReceipt]?
    var readReceipts: Bool?

    enum State: String { case sent, delivered, seen }

    func state(at seq: Int) -> State {
        guard seq > 0, let members, !members.isEmpty else { return .sent }
        let visible = members.filter { $0.seenSeq != nil }
        if readReceipts != false, !visible.isEmpty, visible.allSatisfy({ ($0.seenSeq ?? 0) >= seq }) { return .seen }
        if members.allSatisfy({ ($0.deliveredSeq ?? 0) >= seq }) { return .delivered }
        return .sent
    }
}

struct GroupLink: Decodable {
    let linkToken: String?
    let url: String?
}

struct GroupPreview: Decodable, Equatable {
    let id: String
    let name: String
    let description: String?
    let memberCount: Int?
    let joinPolicy: String?
    let visibility: String?
    let myState: String?
    let canJoin: Bool?
    let needs: String?
}

/// `GET /v1/groups/{id}/crypto`.
struct GroupCryptoState: Decodable {
    let epoch: Int?
    let revision: Int?
    let activationState: String?
    let inGroup: Bool?
    let pending: [PendingChange]?

    struct PendingChange: Decodable, Equatable {
        let kind: String
        let mailbox: String
        let deviceId: String?
    }
}

struct KeyPackageCount: Decodable {
    let unclaimed: Int?
    let lastResort: Bool?
}

struct KeyPackageClaims: Decodable {
    let claims: [Claim]?
    struct Claim: Decodable {
        let mailbox: String
        let deviceId: String
        let keyPackage: String
        let keyPackageRef: String
    }
}

struct Welcomes: Decodable {
    let welcomes: [Welcome]?
    struct Welcome: Decodable {
        let welcomeId: String
        let groupId: String
        let epoch: Int?
        let seq: Int?
        let welcome: String
    }
}

struct RegisteredDevice: Decodable {
    let deviceId: String
    let fingerprint: String?
    let fingerprintDigits: String?
    let attestation: String?
    let mailboxPublicKey: String?
}

struct MailboxDevices: Decodable {
    let mailbox: String?
    let devices: [Device]?
    struct Device: Decodable, Identifiable, Equatable {
        let deviceId: String
        let fingerprint: String?
        let fingerprintDigits: String?
        let createdAt: Int?
        var id: String { deviceId }
    }
}

struct PostedGroupMessage: Decodable {
    let seq: Int
    let id: String?
    let expiresAt: Int?
}

struct DeletedGroupMessage: Decodable, Equatable {
    var deleted: Bool?
    var seqs: [Int]?
}

struct PostedCommit: Decodable {
    let seq: Int
    let epoch: Int?
    let revision: Int?
}

struct UploadedGroupAttachment: Decodable {
    let id: String
    let bytes: Int?
}

/// One item of `GET /v1/groups/{id}/messages`: an application message, a commit or a notice.
struct GroupWireItem: Decodable {
    let seq: Int
    let type: String
    var id: String?
    var epoch: Int?
    var sender: String?
    var senderDevice: String?
    var clientMessageId: String?
    var ciphertext: String?
    var replyTo: Int?
    var editOf: Int?
    var createdAt: Int?
    var expiresAt: Int?
    var deletedAt: Int?
    var replacedBy: Int?
    var commit: String?
    var adds: [DeviceAddress]?
    var removes: [DeviceAddress]?
    var kind: String?
    var actor: String?
    var metadata: NoticeMeta?
}

struct GroupMessagesPage: Decodable {
    let items: [GroupWireItem]?
    let lastSeq: Int?
    let more: Bool?
}

struct DeviceAddress: Codable, Equatable, Hashable {
    let mailbox: String
    let deviceId: String
}

/// The fields notices carry in `metadata`. Everything optional; unknown fields are ignored.
struct NoticeMeta: Codable, Equatable, Hashable {
    var address: String?
    var role: String?
    var changed: [String]?
    var postPolicy: String?
    var joinPolicy: String?
    var name: String?
    var ephemeralSecs: Int?
    var kind: String?
    var duration: Int?
    var deviceId: String?
}

/// `GET /calls/v1/groups/{id}/call`.
struct GroupCallState: Decodable, Equatable {
    let active: Bool?
    let kind: String?
}

// ---- what this device remembers --------------------------------------------------------------------

/// A member device as a verified commit names it.
struct GroupDeviceInfo: Codable, Equatable, Hashable {
    let mailbox: String
    let deviceId: String
    /// Hex, as the web client stores it.
    let fingerprint: String
}

/// A file inside an encrypted message: its name and type travel only inside the ciphertext, and
/// `ref` holds the key that opens the sealed bytes the postbox stores.
struct GroupFile: Codable, Equatable, Hashable, Identifiable {
    let id: String
    var name: String
    var mime: String
    var size: Int
    var ref: FileKey?

    struct FileKey: Codable, Equatable, Hashable {
        /// Base64, as in the web client: `key`, `sha` (sealed SHA-256) and `len` (plaintext length).
        let key: String
        let sha: String
        let len: UInt64
    }
}

/// One line of a group's history on this device. `kind` decides which fields mean anything.
struct GroupItem: Codable, Equatable, Identifiable {
    enum Kind: String, Codable { case message, notice, commit, warning }

    var kind: Kind
    /// The postbox's sequence number; `nil` while one of our own messages is still sending.
    var seq: Int?
    /// Orders lines that share a seq (a warning sits just after the item it is about).
    var order: Double = 0
    var createdAt: Int?
    var expiresAt: Int?

    // message
    var sender: String?
    var senderDevice: String?
    var cmid: String?
    var text: String?
    var files: [GroupFile]?
    var replyTo: String?
    var editOf: String?
    var replacedBy: Int?
    var deleted: Bool?
    var undecryptable: Bool?
    var pending: Bool?
    var sendFailed: Bool?
    var sendFingerprint: String?
    var code: String?
    /// This device checked the encrypted action fields against the sequenced envelope.
    /// Older vault records have no such proof and cannot hide another message as an edit.
    var actionsVerified: Bool?

    // notice
    var notice: String?
    var actor: String?
    var metadata: NoticeMeta?
    var local: Bool?

    // commit
    var epoch: Int?
    var committer: GroupDeviceInfo?
    var added: [GroupDeviceInfo]?
    var removed: [GroupDeviceInfo]?
    var updated: [GroupDeviceInfo]?

    // warning
    var warning: String?

    var id: String {
        switch kind {
        case .message: return "m:" + (seq.map(String.init) ?? (cmid ?? "?"))
        case .notice: return "n:\(seq ?? 0):\(notice ?? "")"
        case .commit: return "c:\(seq ?? 0)"
        case .warning: return "w:\(seq ?? 0):\(warning ?? "")"
        }
    }

    var sortKey: Double { Double(seq ?? Int.max / 2) + order }
}

/// A prepared send is sealed in the same vault as its MLS state. `request` is the exact JSON
/// sent on the wire, retained across an ambiguous response loss instead of encrypting again.
struct GroupPendingSend: Codable, Equatable {
    let group: String
    let cmid: String
    let fingerprint: String
    let createdAt: Int
    var request: Data?
    let body: Data
    let files: [GroupFile]
    let mentions: [String]

    func depends(on seqs: Set<Int>, cmids: Set<String>) -> Bool {
        cmids.contains(cmid) || GroupActions.sequence(GroupBody.decode(body).editOf).map(seqs.contains) == true
    }
}

struct OutgoingGroupFile: Sendable {
    let name: String
    let mime: String
    let data: Data
}

enum GroupSendIntent {
    static func fingerprint(_ group: String, text: String, files: [OutgoingGroupFile], replyTo: Int?, editOf: Int?, mentions: [String]) throws -> String {
        func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
        let payload: [String: Any] = ["group": group, "text": text, "reply": replyTo.map { $0 as Any } ?? NSNull(),
            "edit": editOf.map { $0 as Any } ?? NSNull(), "mentions": mentions.sorted(),
            "files": files.map { ["name": $0.name, "mime": $0.mime, "sha": digest($0.data)] }]
        return digest(try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys]))
    }
}

/// Presentation uses authenticated action references, never `replaced_by` alone. Group edits
/// name the preceding version's seq (unlike direct-message root refs); retain that chain locally.
enum GroupActions {
    static func sequence(_ reference: String?) -> Int? {
        guard let reference, let seq = Int(reference), seq > 0, String(seq) == reference else { return nil }
        return seq
    }

    static func parent(of item: GroupItem, in history: [GroupItem]) -> GroupItem? {
        parent(of: item, in: indexed(history))
    }

    private static func indexed(_ history: [GroupItem]) -> [Int: GroupItem] {
        var out: [Int: GroupItem] = [:]
        for item in history where item.kind == .message {
            if let seq = item.seq, out[seq] == nil { out[seq] = item }
        }
        return out
    }

    private static func parent(of item: GroupItem, in bySeq: [Int: GroupItem]) -> GroupItem? {
        guard item.kind == .message, item.actionsVerified == true, item.pending != true,
              let seq = item.seq, let parentSeq = sequence(item.editOf), parentSeq < seq,
              let sender = item.sender, let parent = bySeq[parentSeq], parent.sender == sender,
              parent.deleted != true, parent.undecryptable != true, parent.text != nil else { return nil }
        return parent
    }

    static func versions(of item: GroupItem, in history: [GroupItem]) -> [GroupItem] {
        var versions = [item]
        var current = item
        let bySeq = indexed(history)
        while let parent = parent(of: current, in: bySeq) {
            versions.append(parent)
            current = parent
        }
        return versions.reversed()
    }

    static func visible(_ history: [GroupItem], now: Int) -> [GroupItem] {
        let bySeq = indexed(history)
        var replaced: Set<Int> = []
        var deadlines: [Int: Int] = [:]
        // One pass in sequence order: parents must have a smaller seq, so even a long edit
        // chain cannot cause recursive or quadratic work on every history refresh.
        for item in history.sorted(by: { $0.sortKey < $1.sortKey }) where item.kind == .message {
            guard let seq = item.seq else { continue }
            var deadline = item.expiresAt ?? .max
            if let parent = parent(of: item, in: bySeq), let parentSeq = parent.seq {
                replaced.insert(parentSeq)
                deadline = min(deadline, deadlines[parentSeq] ?? parent.expiresAt ?? .max)
            }
            deadlines[seq] = deadline
        }
        return history.filter { item in
            if let seq = item.seq, item.kind == .message, replaced.contains(seq) { return false }
            // An edit cannot extend a locally known disappearing message's life.
            let deadline = item.kind == .message ? item.seq.flatMap { deadlines[$0] } : nil
            return (deadline ?? item.expiresAt ?? .max) > now
        }
    }

    static func editable(_ item: GroupItem, in history: [GroupItem], me: String, now: Int) -> Bool {
        guard item.kind == .message, item.sender == me, item.seq != nil, item.pending != true,
              item.deleted != true, item.undecryptable != true, item.text != nil,
              visible(history, now: now).contains(where: { $0.id == item.id }) else { return false }
        let chain = versions(of: item, in: history)
        // A device joining later may have only the last edit. Its timestamp cannot stand in
        // for the original's deadline, and an unverified legacy edit cannot acquire that right.
        guard chain.first?.editOf == nil, let at = chain.first?.createdAt, now >= at else { return false }
        return now - at < 86_400
    }

    /// Connected versions on this device. Only authenticated same-author, backward links may
    /// extend a deletion/expiry beyond the seq the server named. Server-only `replacedBy` is
    /// deliberately ignored, just as it is when folding the visible conversation.
    static func linkedSequences(_ history: [GroupItem], seeds: Set<Int>) -> Set<Int> {
        let bySeq = indexed(history)
        var edges: [Int: [Int]] = [:]
        for item in history where item.kind == .message && item.actionsVerified == true {
            guard let seq = item.seq, let parentSeq = sequence(item.editOf), parentSeq < seq,
                  let sender = item.sender, bySeq[parentSeq]?.sender == sender else { continue }
            edges[seq, default: []].append(parentSeq)
            edges[parentSeq, default: []].append(seq)
        }
        var found = Set(seeds.filter { $0 > 0 })
        var queue = Array(found)
        var at = 0
        while at < queue.count {
            let seq = queue[at]
            at += 1
            for next in edges[seq] ?? [] where found.insert(next).inserted { queue.append(next) }
        }
        return found
    }

    static func erasing(_ history: [GroupItem], seqs: Set<Int>, tombstones: Bool = true) -> [GroupItem] {
        let erased = linkedSequences(history, seeds: seqs)
        return history.compactMap { item in
            let affected = item.seq.map(erased.contains) == true
                || (item.pending == true && sequence(item.editOf).map(erased.contains) == true)
            guard affected else { return item }
            guard tombstones, item.kind == .message, let seq = item.seq else { return nil }
            // Rebuild rather than clearing selected properties: no text, attachment key, quote,
            // pending send or future optional plaintext field survives as a deleted record.
            return GroupItem(kind: .message, seq: seq, createdAt: item.createdAt,
                             expiresAt: item.expiresAt, sender: item.sender, deleted: true)
        }
    }

    static func pruning(_ history: [GroupItem], now: Int) -> [GroupItem] {
        let expired = Set(history.filter { ($0.expiresAt ?? .max) <= now }.compactMap(\.seq))
        return erasing(history, seqs: expired, tombstones: false)
            .filter { ($0.expiresAt ?? .max) > now }
    }

    static func replyTarget(of item: GroupItem, in history: [GroupItem], now: Int) -> GroupItem? {
        guard item.actionsVerified == true, let seq = sequence(item.replyTo) else { return nil }
        let linked = linkedSequences(history, seeds: [seq])
        return visible(history, now: now).last {
            $0.kind == .message && $0.deleted != true && $0.undecryptable != true
                && $0.pending != true && $0.seq.map(linked.contains) == true
        }
    }
}

/// What travels inside the encryption: the web client's `encodeBody`/`decodeBody`, byte for byte
/// in shape, so a message written on the phone reads the same in the browser and back.
struct GroupBody: Codable, Equatable {
    var v: Int? = 1
    var type: String? = "text"
    var text: String?
    var files: [WireFile]?
    var replyTo: String?
    var editOf: String?

    struct WireFile: Codable, Equatable {
        var id: String
        var name: String
        var mime: String
        var size: Int

        init(id: String, name: String, mime: String, size: Int) {
            self.id = id
            self.name = name
            self.mime = mime
            self.size = size
        }

        /// Lenient, like the web client's `decodeBody`: a file entry with a missing field is still
        /// a file, called "file".
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = (try? c.decode(String.self, forKey: .id)) ?? ""
            name = (try? c.decode(String.self, forKey: .name)) ?? "file"
            mime = (try? c.decode(String.self, forKey: .mime)) ?? "application/octet-stream"
            size = (try? c.decode(Int.self, forKey: .size)) ?? Int((try? c.decode(Double.self, forKey: .size)) ?? 0)
        }
    }

    enum CodingKeys: String, CodingKey {
        case v, type, text, files
        case replyTo = "reply_to"
        case editOf = "edit_of"
    }

    func encoded() -> Data {
        var copy = self
        if let text = copy.text { copy.text = String(text.prefix(64_000)) }
        copy.files = files.map { list in
            list.map { WireFile(id: $0.id, name: String($0.name.prefix(200)), mime: String($0.mime.prefix(120)), size: $0.size) }
        }
        if copy.files?.isEmpty == true { copy.files = nil }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return (try? encoder.encode(copy)) ?? Data("{}".utf8)
    }

    /// Anything that is not our JSON is shown as the text it is, as the web client does.
    static func decode(_ data: Data) -> GroupBody {
        if let body = try? JSONDecoder().decode(GroupBody.self, from: data), body.v != nil || body.text != nil || body.files != nil {
            var clean = body
            clean.files = body.files.map { Array($0.prefix(32)) }
            return clean
        }
        return GroupBody(text: String(decoding: data, as: UTF8.self))
    }
}

enum GroupTimers {
    /// The only values the postbox accepts (docs/groups.md §5); `nil` is off.
    static let all: [Int?] = [nil, 300, 3600, 28800, 86400, 604800, 2419200]

    static func label(_ seconds: Int?) -> String {
        guard let seconds, seconds > 0 else { return "Off" }
        switch seconds {
        case 300: return "5 minutes"
        case 3600: return "1 hour"
        case 28800: return "8 hours"
        case 86400: return "1 day"
        case 604800: return "1 week"
        case 2419200: return "4 weeks"
        default: return "\(Int((Double(seconds) / 60).rounded())) minutes"
        }
    }

    /// Said wherever a timer can be chosen (SDS §17).
    static let limits = "Recipients can still take screenshots or copy text, notifications already shown may stay on screen, and copies made outside Pigeonpost are not affected."
}

/// A group join link: `https://inbox.pigeonpost.dev/g/<id>#<token>`. The token is a fragment, so it
/// never reaches a server log; the app sends it only in a request body.
struct GroupJoinLink: Equatable, Identifiable {
    let groupId: String
    let token: String?
    var id: String { groupId + (token ?? "") }

    init(groupId: String, token: String?) {
        self.groupId = groupId
        self.token = token
    }

    /// `https://inbox.pigeonpost.dev/g/<id>#<token>` (or `/g/<id>` with no token, for a listed
    /// public group). Anything else is not a join link.
    init?(url: URL) {
        let parts = url.path.split(separator: "/")
        guard url.scheme == "https", url.host == "inbox.pigeonpost.dev" || url.host == "pigeonpost.dev",
              parts.count == 2, parts[0] == "g", parts[1].hasPrefix("grp_"),
              parts[1].allSatisfy({ $0.isLetter || $0.isNumber || $0 == "_" }) else { return nil }
        self.init(groupId: String(parts[1]), token: url.fragment.flatMap { $0.isEmpty ? nil : $0 })
    }
}
