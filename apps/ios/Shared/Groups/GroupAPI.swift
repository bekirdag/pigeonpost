//  The group routes (docs/groups.md §1–§6), on the client's own wire.
//
//  Every call names the acting mailbox in `x-pigeonpost-identity` — a group member is a mailbox,
//  never an account, and an account with several mailboxes must say which one is speaking — and the
//  device-scoped ones (marked D in the contract) add `x-pigeonpost-device`.

import Foundation

/// The acting mailbox and, for device-scoped routes, this device.
struct GroupCaller: Sendable, Equatable {
    let mailbox: String
    var device: String?

    func headers(device needed: Bool = false) -> [String: String] {
        var out = ["x-pigeonpost-identity": mailbox]
        if needed, let device { out["x-pigeonpost-device"] = device }
        return out
    }
}

enum GroupIDs {
    private static let alphabet = Array("abcdefghijklmnopqrstuvwxyz234567")

    static func random(_ count: Int) -> String {
        var bytes = [UInt8](repeating: 0, count: count)
        _ = SecRandomCopyBytes(kSecRandomDefault, count, &bytes)
        return String(bytes.map { alphabet[Int($0 & 31)] })
    }

    /// `ios-` or `mac-` and twelve base-32 characters, like the web's `web-…`, so a device list says
    /// which kind of device each one is.
    static func newDevice() -> String {
        #if os(macOS)
        return "mac-" + random(12)
        #else
        return "ios-" + random(12)
        #endif
    }

    static func requestKey() -> String { "rk_" + random(20) }
    static func clientMessage() -> String { "cm_" + random(24) }
}

extension PostboxClient {
    private static func path(_ group: String, _ rest: String = "") -> String {
        "/v1/groups/" + (group.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? group) + rest
    }

    /// One path segment, every reserved character encoded — an address's slashes included.
    static func segment(_ value: String) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
    }

    // ---- devices and key packages (§1) -----------------------------------------------------------

    func registerGroupDevice(_ caller: GroupCaller, deviceId: String, signatureKey: Data) async throws -> RegisteredDevice {
        try await send("/v1/mls/devices", method: "POST",
                       json: ["device_id": deviceId, "signature_key": signatureKey.base64EncodedString()],
                       headers: caller.headers(), as: RegisteredDevice.self)
    }

    func groupDevices(_ caller: GroupCaller, of mailbox: String) async throws -> [MailboxDevices.Device] {
        try await send("/v1/mls/devices", query: [.init(name: "mailbox", value: mailbox)],
                       headers: caller.headers(), as: MailboxDevices.self).devices ?? []
    }

    func revokeGroupDevice(_ caller: GroupCaller, deviceId: String) async throws {
        try await sendRaw("/v1/mls/devices/" + Self.segment(deviceId), method: "DELETE", headers: caller.headers(device: true))
    }

    func keyPackageCount(_ caller: GroupCaller) async throws -> KeyPackageCount {
        try await send("/v1/mls/key-packages/count", headers: caller.headers(device: true), as: KeyPackageCount.self)
    }

    func uploadKeyPackages(_ caller: GroupCaller, packages: [Data], lastResort: Data?) async throws {
        var body: [String: Any] = [
            "request_key": GroupIDs.requestKey(),
            "key_packages": packages.map { $0.base64EncodedString() },
        ]
        if let lastResort { body["last_resort"] = lastResort.base64EncodedString() }
        try await sendRaw("/v1/mls/key-packages", method: "POST", json: body, headers: caller.headers(device: true))
    }

    func groupWelcomes(_ caller: GroupCaller) async throws -> [Welcomes.Welcome] {
        try await send("/v1/mls/welcomes", headers: caller.headers(device: true), as: Welcomes.self).welcomes ?? []
    }

    func ackWelcome(_ caller: GroupCaller, id: String, joined: Bool, error: String?) async throws {
        var body: [String: Any] = ["request_key": GroupIDs.requestKey(), "joined": joined]
        if let error { body["error_code"] = error }
        try await sendRaw("/v1/mls/welcomes/" + Self.segment(id) + "/ack", method: "POST", json: body,
                          headers: caller.headers(device: true))
    }

    // ---- groups (§2) -------------------------------------------------------------------------------

    func groups(_ caller: GroupCaller) async throws -> [GroupSummary] {
        try await send("/v1/groups", query: [.init(name: "state", value: "active,invited,keying")],
                       headers: caller.headers(), as: GroupsResponse.self).groups ?? []
    }

    func group(_ caller: GroupCaller, id: String) async throws -> GroupSummary {
        let data = try await sendRaw(Self.path(id), headers: caller.headers())
        // The single-group route answers with the object itself; tolerate `{group: …}` too.
        if let wrapped = try? PostboxClient.decode(GroupEnvelope.self, from: data) { return wrapped.group }
        return try PostboxClient.decode(GroupSummary.self, from: data)
    }

    func createGroup(_ caller: GroupCaller, settings: [String: Any]) async throws -> CreatedGroup {
        var body = settings
        body["request_key"] = GroupIDs.requestKey()
        return try await send("/v1/groups", method: "POST", json: body, headers: caller.headers(), as: CreatedGroup.self)
    }

    @discardableResult
    func updateGroup(_ caller: GroupCaller, id: String, fields: [String: Any]) async throws -> GroupSummary? {
        let data = try await sendRaw(Self.path(id), method: "PATCH", json: fields, headers: caller.headers())
        if let wrapped = try? PostboxClient.decode(GroupEnvelope.self, from: data) { return wrapped.group }
        return try? PostboxClient.decode(GroupSummary.self, from: data)
    }

    func deleteGroup(_ caller: GroupCaller, id: String) async throws {
        try await sendRaw(Self.path(id), method: "DELETE", headers: caller.headers())
    }

    func groupMembers(_ caller: GroupCaller, id: String) async throws -> [GroupMember] {
        try await send(Self.path(id, "/members"), headers: caller.headers(), as: MembersResponse.self).members ?? []
    }

    func invite(_ caller: GroupCaller, id: String, people: [String]) async throws -> [InvitationResult] {
        try await send(Self.path(id, "/members"), method: "POST",
                       json: ["request_key": GroupIDs.requestKey(), "invite": people],
                       headers: caller.headers(), as: InviteResults.self).results ?? []
    }

    func changeMember(_ caller: GroupCaller, id: String, address: String, revision: Int?, role: String? = nil, state: String? = nil) async throws {
        var body: [String: Any] = [:]
        if let revision { body["expected_revision"] = revision }
        if let role { body["role"] = role }
        if let state { body["state"] = state }
        try await sendRaw(Self.path(id, "/members/" + Self.segment(address)), method: "PATCH", json: body, headers: caller.headers())
    }

    func liftBan(_ caller: GroupCaller, id: String, address: String) async throws {
        try await sendRaw(Self.path(id, "/bans/" + Self.segment(address)), method: "DELETE", headers: caller.headers())
    }

    func answerInvitation(_ caller: GroupCaller, id: String, accept: Bool) async throws {
        try await sendRaw(Self.path(id, "/invitation"), method: "POST", json: ["accept": accept], headers: caller.headers())
    }

    func joinGroup(_ caller: GroupCaller, id: String, linkToken: String?) async throws {
        try await sendRaw(Self.path(id, "/join"), method: "POST",
                          json: linkToken.map { ["link_token": $0] } ?? [:], headers: caller.headers())
    }

    func leaveGroup(_ caller: GroupCaller, id: String) async throws {
        try await sendRaw(Self.path(id, "/leave"), method: "POST", json: [:], headers: caller.headers())
    }

    func transferGroup(_ caller: GroupCaller, id: String, to: String, revision: Int?) async throws {
        var body: [String: Any] = ["to": to]
        if let revision { body["expected_revision"] = revision }
        try await sendRaw(Self.path(id, "/transfer"), method: "POST", json: body, headers: caller.headers())
    }

    func groupLink(_ caller: GroupCaller, id: String, enabled: Bool) async throws -> GroupLink {
        try await send(Self.path(id, "/link"), method: "POST", json: ["enabled": enabled], headers: caller.headers(), as: GroupLink.self)
    }

    /// The token travels in the body, never in a URL (docs/groups.md §2).
    func resolveGroup(_ caller: GroupCaller, id: String, linkToken: String?) async throws -> GroupPreview {
        var body: [String: Any] = ["group_id": id]
        if let linkToken { body["link_token"] = linkToken }
        return try await send("/v1/groups/resolve", method: "POST", json: body, headers: caller.headers(), as: GroupPreview.self)
    }

    func markGroupRead(_ caller: GroupCaller, id: String, seq: Int) async throws {
        try await sendRaw(Self.path(id, "/read"), method: "POST", json: ["seq": seq], headers: caller.headers())
    }

    func groupReceipts(_ caller: GroupCaller, id: String) async throws -> GroupReceipts {
        try await send(Self.path(id, "/receipts"), headers: caller.headers(), as: GroupReceipts.self)
    }

    func setGroupNotifications(_ caller: GroupCaller, id: String, notify: String, mutedUntil: Int?) async throws {
        try await sendRaw(Self.path(id, "/notifications"), method: "PUT",
                          json: ["notify": notify, "muted_until": mutedUntil.map { $0 as Any } ?? NSNull()],
                          headers: caller.headers())
    }

    // ---- messages and handshakes (§3) -------------------------------------------------------------

    func groupMessages(_ caller: GroupCaller, id: String, after: Int, limit: Int = 100) async throws -> GroupMessagesPage {
        try await send(Self.path(id, "/messages"),
                       query: [.init(name: "after_seq", value: String(after)), .init(name: "limit", value: String(limit))],
                       headers: caller.headers(device: true), as: GroupMessagesPage.self)
    }

    func postGroupMessage(_ caller: GroupCaller, id: String, body: [String: Any]) async throws -> PostedGroupMessage {
        try await send(Self.path(id, "/messages"), method: "POST", json: body, headers: caller.headers(device: true),
                       as: PostedGroupMessage.self)
    }

    func deleteGroupMessage(_ caller: GroupCaller, id: String, seq: Int) async throws {
        try await sendRaw(Self.path(id, "/messages/\(seq)"), method: "DELETE", headers: caller.headers())
    }

    func uploadGroupAttachment(_ caller: GroupCaller, id: String, sealed: Data) async throws -> UploadedGroupAttachment {
        let data = try await sendRaw(Self.path(id, "/attachments"), method: "POST", body: sealed,
                                     contentType: "application/octet-stream", headers: caller.headers(device: true), timeout: 300)
        return try PostboxClient.decode(UploadedGroupAttachment.self, from: data)
    }

    func downloadGroupAttachment(_ caller: GroupCaller, id: String, attachment: String) async throws -> Data {
        try await sendRaw(Self.path(id, "/attachments/" + Self.segment(attachment)),
                          headers: caller.headers(device: true).merging(["accept": "application/octet-stream"]) { $1 },
                          timeout: 300)
    }

    func claimKeyPackages(_ caller: GroupCaller, id: String, mailboxes: [String]) async throws -> [KeyPackageClaims.Claim] {
        try await send(Self.path(id, "/key-packages/claim"), method: "POST",
                       json: ["request_key": GroupIDs.requestKey(), "mailboxes": mailboxes],
                       headers: caller.headers(device: true), as: KeyPackageClaims.self).claims ?? []
    }

    func postCommit(_ caller: GroupCaller, id: String, body: [String: Any]) async throws -> PostedCommit {
        try await send(Self.path(id, "/commits"), method: "POST", json: body, headers: caller.headers(device: true), as: PostedCommit.self)
    }

    func ackEpoch(_ caller: GroupCaller, id: String, epoch: UInt64) async throws {
        try await sendRaw(Self.path(id, "/epoch-ack"), method: "POST", json: ["epoch": epoch], headers: caller.headers(device: true))
    }

    func groupCrypto(_ caller: GroupCaller, id: String) async throws -> GroupCryptoState {
        try await send(Self.path(id, "/crypto"), headers: caller.headers(device: true), as: GroupCryptoState.self)
    }

    func resyncGroup(_ caller: GroupCaller, id: String) async throws {
        try await sendRaw(Self.path(id, "/resync"), method: "POST", json: ["request_key": GroupIDs.requestKey()],
                          headers: caller.headers(device: true))
    }

    // ---- disappearing direct messages (§5) -------------------------------------------------------------

    func setThreadTimer(identity: String, thread: String, seconds: Int?) async throws {
        try await sendRaw("/v1/threads/" + Self.segment(thread), method: "PATCH",
                          query: [.init(name: "identity", value: identity)],
                          json: ["ephemeral_secs": seconds.map { $0 as Any } ?? NSNull()])
    }
}
