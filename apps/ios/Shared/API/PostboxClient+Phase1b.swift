//  Phase 1b routes: call history, filtered mail, the inbox policy, blocking and profile pictures.
//
//  The shapes are the postbox's (`calllog.rs`, `filtering.rs`, `avatars.rs`) and the calls are the
//  web inbox's (site-inbox/app.js, "Phase 1b"), so both clients ask the same questions in the same
//  words. Every route is per mailbox, named by `identity` exactly as the older routes are.

import Foundation

private struct SeenAnswer: Decodable { let unseenMissed: Int? }
private struct AvatarsAnswer: Decodable { let avatars: [String: AvatarInfo?]? }
private struct OwnAvatarAnswer: Decodable { let avatar: AvatarInfo? }
private struct UploadAnswer: Decodable { let avatar: AvatarInfo?; let avatars: [AvatarInfo]? }

extension PostboxClient {
    private func identity(_ address: String) -> [URLQueryItem] { [URLQueryItem(name: "identity", value: address)] }

    // ---- call history --------------------------------------------------------------------------

    func callLog(identity address: String, filter: String, before: Int? = nil, limit: Int = 50) async throws -> CallLogPage {
        var query = identity(address) + [
            URLQueryItem(name: "filter", value: filter),
            URLQueryItem(name: "limit", value: String(limit)),
        ]
        if let before { query.append(URLQueryItem(name: "before", value: String(before))) }
        return try await send("/v1/calls/log", query: query, as: CallLogPage.self)
    }

    /// Clears the missed badge up to a point. Returns what the server still counts as unseen.
    func markCallsSeen(identity address: String, upTo: Int) async throws -> Int {
        try await send("/v1/calls/log/seen", method: "POST", json: ["up_to": upTo, "identity": address],
                       as: SeenAnswer.self).unseenMissed ?? 0
    }

    func deleteCall(identity address: String, id: String) async throws {
        let escaped = id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? id
        try await sendRaw("/v1/calls/log/\(escaped)", method: "DELETE", query: identity(address))
    }

    func clearCallLog(identity address: String) async throws {
        try await sendRaw("/v1/calls/log", method: "DELETE", query: identity(address))
    }

    /// Report the caller of one incoming call. Keyed by the call, feeding the same standing as a
    /// reported message.
    func reportCall(identity address: String, callId: String) async throws {
        try await sendRaw("/v1/report-spam", method: "POST", json: ["call_id": callId, "identity": address])
    }

    // ---- filtered mail -------------------------------------------------------------------------

    /// Mail the postbox filed under Filtered: no push, not unread, not in the ordinary listing.
    func filteredInbox(identity address: String) async throws -> [Message] {
        let query = identity(address) + [
            URLQueryItem(name: "filtered", value: "1"),
            URLQueryItem(name: "include_read", value: "true"),
        ]
        return try await send("/v1/inbox", query: query, as: InboxResponse.self).messages ?? []
    }

    /// Admit a filtered sender: a contact allow row by name, and their mail moved to the inbox.
    func acceptFiltered(identity address: String, peer: String) async throws {
        try await sendRaw("/v1/filtered/accept", method: "POST", json: ["peer": peer, "identity": address])
    }

    // ---- the inbox policy ------------------------------------------------------------------------

    func policy(identity address: String) async throws -> InboxFilters {
        try await send("/v1/policy", query: identity(address), as: InboxFilters.self)
    }

    /// One field at a time, as the web does; the answer is the whole policy as saved.
    func setPolicy(identity address: String, field: String, value: Any) async throws -> InboxFilters {
        try await send("/v1/policy", method: "PUT", json: [field: value, "identity": address], as: InboxFilters.self)
    }

    // ---- blocking --------------------------------------------------------------------------------

    /// Exact and wildcard blocks, fresh from the server.
    func blockedContacts(identity address: String) async throws -> [Contact] {
        let query = identity(address) + [URLQueryItem(name: "admission", value: "block")]
        return (try await send("/v1/contacts", query: query, as: ContactsResponse.self).contacts ?? [])
            .filter { $0.admission == "block" }
    }

    /// Unblocking forgets the row; the sender is a stranger again, subject to the policy.
    func deleteContact(identity address: String, peer: String) async throws {
        try await sendRaw("/v1/contacts", method: "DELETE",
                          query: identity(address) + [URLQueryItem(name: "peer", value: peer)],
                          json: ["peer": peer, "identity": address])
    }

    // ---- profile pictures ------------------------------------------------------------------------

    /// Who has a picture this account may see. Keyed by exactly the strings sent; `nil` for none.
    func avatars(for keys: [String]) async throws -> [String: AvatarInfo?] {
        // One comma-separated value, as the web sends it. URLComponents leaves the commas and
        // slashes alone (both are legal in a query) and escapes anything that is not.
        let query = [URLQueryItem(name: "addresses", value: keys.joined(separator: ","))]
        return try await send("/v1/avatars", query: query, as: AvatarsAnswer.self).avatars ?? [:]
    }

    /// The bytes behind one of the URLs above. Sent with the bearer token: a "people I know"
    /// picture is not public.
    func avatarBlob(path: String) async throws -> Data {
        try await sendRaw(path)
    }

    func ownAvatar(identity address: String) async throws -> AvatarInfo? {
        try await send("/v1/profile/avatar", query: identity(address), as: OwnAvatarAnswer.self).avatar
    }

    /// Upload a picture. `crop` is x,y,w,h in the uploaded image's pixels.
    func uploadAvatar(identity address: String, jpeg: Data, crop: (Int, Int, Int, Int),
                      visibility: String, allMailboxes: Bool) async throws -> AvatarInfo? {
        var query = identity(address) + [
            URLQueryItem(name: "crop", value: "\(crop.0),\(crop.1),\(crop.2),\(crop.3)"),
            URLQueryItem(name: "visibility", value: visibility),
        ]
        if allMailboxes { query.append(URLQueryItem(name: "all", value: "1")) }
        let data = try await sendRaw("/v1/profile/avatar", method: "PUT", query: query, body: jpeg,
                                     contentType: "image/jpeg", timeout: 120)
        let answer = try? PostboxClient.decode(UploadAnswer.self, from: data)
        return answer?.avatar ?? answer?.avatars?.first
    }

    func setAvatarVisibility(identity address: String, visibility: String) async throws -> AvatarInfo? {
        try await send("/v1/profile/avatar", method: "PATCH", json: ["identity": address, "visibility": visibility],
                       as: OwnAvatarAnswer.self).avatar
    }

    func removeAvatar(identity address: String) async throws {
        try await sendRaw("/v1/profile/avatar", method: "DELETE", query: identity(address))
    }
}
