//  Groups (Phase 3), as the postbox describes them and as this device remembers them.
//
//  Wire shapes are docs/groups.md §2–§3, decoded with the client's snake-case strategy and as
//  optional as the rest of `Models.swift`, for the same reason: a field added to the postbox later
//  must not turn a whole list into a decoding failure. What this device keeps of a group — the
//  decrypted history — is `GroupItem`, which is the web client's history record
//  (site-inbox/groups-core.js `remember`) in Swift.

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
    var code: String?

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
