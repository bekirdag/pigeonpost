//  One incoming ring, as the phone and the Mac hear about it.
//
//  A ring reaches a device by one of three routes: a VoIP push (iPhone, iPad), an alert push with
//  the `CALL_RING` category (Mac), or the call page's own event stream while the app is open. All
//  three carry the same facts — which room, audio or video, who is calling, and (from a push) the
//  ring token that lets this device decline or look up *this* ring without a member token. This file
//  turns each of them into the same value, and holds the small rules around it that are worth
//  testing on their own: the room id is the identity of a ring, a ring has an end time, and the
//  calls service's reason for ending a ring has to be said in the system's own vocabulary.
//
//  Contract: docs/call-alerting.md. Nothing here touches CallKit, the network or a view, so
//  `Tests/run.sh` compiles it with plain `swiftc`.

import CryptoKit
import Foundation

struct CallRing: Equatable {
    /// The room being rung. The dedupe key: a VoIP push and an in-app ring for the same room are the
    /// same call and must never show twice.
    let room: String
    /// "audio" or "video".
    let kind: String
    /// The six-character room code, for the record; nothing here dials it.
    let code: String?
    /// Lets this device decline or read the state of this ring and nothing else. Absent on a ring
    /// that came over the page's event stream, which acts through its own session instead.
    var ringToken: String?
    let fromAddress: String?
    let fromHandle: String?
    /// The caller's own display name, as the postbox cleaned it (60 characters, no control or bidi
    /// characters). Display text only.
    let fromDisplay: String?
    /// The mailbox that was called, as the caller wrote it (a handle or an address).
    let to: String?
    /// The called mailbox's address, so a device signed in to another account can tell the push was
    /// not meant for it.
    let identity: String?
    /// When the calls service gives up ringing.
    let expiresAt: Date?

    var isVideo: Bool { kind == "video" }
    var uuid: UUID { Self.uuid(forRoom: room) }

    /// What the system call screen and Recents keep as "who called": the handle when there is one
    /// (that is what a person calls back), the address otherwise.
    var handleValue: String { fromHandle ?? fromAddress ?? "unknown" }

    /// The name on the call screen: the caller's own display name, else whatever readable form of
    /// their handle or address `fallback` makes (the app passes `PeerFace.displayName`).
    func callerName(known: String? = nil, fallback: (String?) -> String) -> String {
        if let known, !known.isEmpty { return known }
        if let fromDisplay, !fromDisplay.isEmpty { return fromDisplay }
        return fallback(fromHandle ?? fromAddress)
    }

    /// Past the time the calls service stops ringing. A ring with no end time never expires here;
    /// the calls service still ends it.
    func isExpired(at now: Date = Date()) -> Bool {
        guard let expiresAt else { return false }
        return now >= expiresAt
    }

    /// Whether this ring was addressed to a mailbox in `mailboxes` (addresses and handles both). A
    /// ring that names no mailbox at all is given the benefit of the doubt: the postbox only pushes
    /// to devices registered for the account, so it is far more likely ours than not.
    func isFor(mailboxes: [(address: String, handle: String?)]) -> Bool {
        let named = [identity, to].compactMap { $0 }.filter { !$0.isEmpty }
        guard !named.isEmpty else { return true }
        return mailboxes.contains { box in
            named.contains { $0 == box.address || (box.handle != nil && $0 == box.handle) }
        }
    }

    init(room: String, kind: String, code: String? = nil, ringToken: String? = nil,
         fromAddress: String? = nil, fromHandle: String? = nil, fromDisplay: String? = nil,
         to: String? = nil, identity: String? = nil, expiresAt: Date? = nil) {
        self.room = room
        self.kind = kind == "video" ? "video" : "audio"
        self.code = code
        self.ringToken = ringToken
        self.fromAddress = fromAddress
        self.fromHandle = fromHandle
        self.fromDisplay = fromDisplay
        self.to = to
        self.identity = identity
        self.expiresAt = expiresAt
    }

    /// A push payload: the VoIP push on a phone, or the userInfo of a `CALL_RING` alert on a Mac.
    /// Nil when it is not a ring or names no usable room — the caller must still report something to
    /// CallKit for a VoIP push, and does.
    init?(push payload: [AnyHashable: Any]) {
        guard Self.string(payload["type"]) == "call_ring",
              let room = Self.string(payload["room"]), Self.isRoomId(room) else { return nil }
        let from = payload["from"] as? [String: Any] ?? [:]
        self.init(
            room: room,
            kind: Self.string(payload["kind"]) ?? "audio",
            code: Self.string(payload["code"]),
            ringToken: Self.string(payload["ring_token"]).flatMap { Self.isRingToken($0) ? $0 : nil },
            // The FCM shape flattens `from`; the APNs one nests it. Both are read so one parser
            // serves every push this app could be handed.
            fromAddress: Self.string(from["address"]) ?? Self.string(payload["from_address"]),
            fromHandle: Self.string(from["handle"]) ?? Self.string(payload["from_handle"]),
            fromDisplay: Self.display(Self.string(from["display"]) ?? Self.string(payload["from_display"])),
            to: Self.string(payload["to"]),
            identity: Self.string(payload["identity"]),
            expiresAt: Self.seconds(payload["expires_at"]).map { Date(timeIntervalSince1970: $0) }
        )
    }

    /// The page's `{type: "ring", room, code, kind, from: {address, handle}, to}`.
    init?(page message: [String: Any]) {
        guard Self.string(message["type"]) == "ring",
              let room = Self.string(message["room"]), Self.isRoomId(room) else { return nil }
        let from = message["from"] as? [String: Any] ?? [:]
        self.init(
            room: room,
            kind: Self.string(message["kind"]) ?? "audio",
            code: Self.string(message["code"]),
            fromAddress: Self.string(from["address"]),
            fromHandle: Self.string(from["handle"]),
            fromDisplay: Self.display(Self.string(from["display"])),
            to: Self.string(message["to"]),
            identity: Self.string(message["identity"]),
            expiresAt: Self.seconds(message["expires_at"]).map { Date(timeIntervalSince1970: $0) }
        )
    }

    /// One room, one UUID, whichever route the ring took. CallKit keys a call by UUID; deriving it
    /// from the room id (rather than minting one per report) is what makes the second report of the
    /// same ring a duplicate CallKit refuses instead of a second call on the screen.
    static func uuid(forRoom room: String) -> UUID {
        var bytes = Array(SHA256.hash(data: Data("pigeonpost-call:\(room)".utf8)).prefix(16))
        // Shaped as a name-based UUID (version 5 bits, RFC 4122 variant) so nothing that inspects
        // it mistakes it for a random one.
        bytes[6] = (bytes[6] & 0x0F) | 0x50
        bytes[8] = (bytes[8] & 0x3F) | 0x80
        return UUID(uuid: (bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                           bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]))
    }

    /// Room ids go into a URL path, so only what the calls service mints is accepted: base64url
    /// characters, a sensible length.
    static func isRoomId(_ value: String) -> Bool {
        (4...64).contains(value.count) && value.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }
    }

    /// 32 random bytes, base64url: 43 characters. Anything else is not a token this device will send.
    static func isRingToken(_ value: String) -> Bool {
        (16...128).contains(value.count) && value.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }
    }

    private static func string(_ value: Any?) -> String? {
        guard let text = value as? String, !text.isEmpty else { return nil }
        return text
    }

    /// Seconds since 1970, as a JSON number (APNs) or a string (FCM).
    private static func seconds(_ value: Any?) -> TimeInterval? {
        if let number = value as? NSNumber { return number.doubleValue }
        if let text = value as? String { return TimeInterval(text) }
        return nil
    }

    /// The postbox already cleans the display name; this is the belt to its braces, since it ends up
    /// on the lock screen.
    private static func display(_ value: String?) -> String? {
        guard let value else { return nil }
        let cleaned = String(String.UnicodeScalarView(value.unicodeScalars.filter {
            !CharacterSet.controlCharacters.contains($0) && !(0x202A...0x202E).contains($0.value)
                && !(0x2066...0x2069).contains($0.value) && $0.value != 0x200E && $0.value != 0x200F
        })).trimmingCharacters(in: .whitespacesAndNewlines)
        return cleaned.isEmpty ? nil : String(cleaned.prefix(60))
    }
}

/// Why a ring stopped, in the terms a system call screen uses. The calls service says
/// `answered_elsewhere`, `declined`, `busy`, `no_answer`, `cancelled` or `ended`; CallKit has a
/// fixed, smaller list (`CXCallEndedReason`), and so does this.
enum RingEnd: Equatable {
    case answeredElsewhere
    case declinedElsewhere
    case remoteEnded
    case unanswered
    case failed

    init(reason: String?) {
        switch reason {
        case "answered_elsewhere", "answered": self = .answeredElsewhere
        case "declined", "busy": self = .declinedElsewhere
        case "no_answer", "timeout", "expired": self = .unanswered
        case "cancelled", "ended", "hangup": self = .remoteEnded
        default: self = .remoteEnded
        }
    }
}

/// `GET /calls/v1/rings/{room}`: `{"state":"ringing","kind","expires_at"}` or
/// `{"state":"ended","reason"}`. A 404 (`no_ring`) means the ring is gone — ended long enough ago to
/// be forgotten, or never ours — and is read as ended.
enum RingState: Equatable {
    case ringing(expiresAt: Date?)
    case ended(RingEnd)

    init?(status: Int, body: Data) {
        if status == 404 { self = .ended(.remoteEnded); return }
        guard (200..<300).contains(status),
              let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let state = json["state"] as? String else { return nil }
        switch state {
        case "ringing":
            let expires = (json["expires_at"] as? NSNumber).map { Date(timeIntervalSince1970: $0.doubleValue) }
            self = .ringing(expiresAt: expires)
        case "ended":
            self = .ended(RingEnd(reason: json["reason"] as? String))
        default:
            return nil
        }
    }
}

/// The two ring-token endpoints, on the call page's own origin — the calls service sits under
/// `/calls/` there in production and in the local test harness alike, so following the page means a
/// DEBUG build pointed at the harness declines against the harness.
enum RingEndpoint {
    static func state(page: URL, room: String) -> URL? { url(page: page, path: "/calls/v1/rings/\(room)") }
    static func decline(page: URL, room: String) -> URL? { url(page: page, path: "/calls/v1/rings/\(room)/decline") }

    private static func url(page: URL, path: String) -> URL? {
        guard var parts = URLComponents(url: page, resolvingAgainstBaseURL: false) else { return nil }
        parts.path = path
        parts.query = nil
        parts.fragment = nil
        return parts.url
    }

    /// Decline one ring with its token. Works with no member token at all — from a lock screen, with
    /// a session that has long expired. Best effort: a decline that fails leaves the caller ringing
    /// until the ring times out, which is what not answering would have done anyway.
    static func decline(page: URL, room: String, ringToken: String, session: URLSession = .shared) async -> Bool {
        guard CallRing.isRoomId(room), let url = decline(page: page, room: room) else { return false }
        var request = URLRequest(url: url, timeoutInterval: 10)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: ["ring_token": ringToken, "reason": "declined"])
        guard let (_, response) = try? await session.data(for: request) else { return false }
        return ((response as? HTTPURLResponse)?.statusCode ?? 0) / 100 == 2
    }

    /// The ring's state now, or nil when it could not be read (offline; try again next time).
    static func fetchState(page: URL, room: String, ringToken: String, session: URLSession = .shared) async -> RingState? {
        guard CallRing.isRoomId(room), let url = state(page: page, room: room) else { return nil }
        var request = URLRequest(url: url, timeoutInterval: 5)
        request.setValue(ringToken, forHTTPHeaderField: "x-ring-token")
        request.cachePolicy = .reloadIgnoringLocalCacheData
        guard let (data, response) = try? await session.data(for: request),
              let status = (response as? HTTPURLResponse)?.statusCode else { return nil }
        return RingState(status: status, body: data)
    }
}
