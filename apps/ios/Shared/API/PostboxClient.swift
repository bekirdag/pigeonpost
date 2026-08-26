//  The postbox API. One place that knows about `identity=`, and the only place that retries a 401.

import Foundation

struct APIError: LocalizedError, Equatable {
    let status: Int
    /// The postbox's own machine-readable code — `not_admitted`, `recipient_unresolved`, and so on.
    let code: String?
    let detail: String?

    var errorDescription: String? { detail ?? code ?? "The postbox answered \(status)." }

    /// What to say to a person when a send does not go through. The codes are the postbox's; the
    /// sentences are the web app's, so both clients fail with the same words.
    var sendFailureMessage: String {
        switch code {
        case "not_admitted": return "They are not accepting mail from this mailbox."
        case "recipient_unresolved": return "No mailbox at that address."
        case "recipient_inbox_full": return "Their inbox is full."
        case "unauthorized": return "Your session expired. Sign in again."
        default: return errorDescription ?? "Could not send."
        }
    }
}

/// Anything that can hand out a live bearer token and spend a refresh token on demand.
@MainActor
protocol TokenProviding: AnyObject {
    func token() async throws -> String
    @discardableResult func renew() async throws -> String
}

extension Session: TokenProviding {}

struct PostboxClient {
    let base: URL
    private(set) weak var tokens: TokenProviding?

    @MainActor
    init(base: URL = Config.postbox, tokens: TokenProviding) {
        self.base = base
        self.tokens = tokens
    }

    private static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    // ---- reads ---------------------------------------------------------------------------------

    func identities() async throws -> [IdentityRow] {
        try await send("/v1/identities", as: IdentitiesResponse.self).identities ?? []
    }

    /// Mint a mailbox on the account: anonymous, or named when a handle is given.
    ///
    /// No proof-of-work and no label: an account holder has already authenticated, and the postbox
    /// creates under the account on the strength of this very token. The one-time capability token
    /// in the reply is deliberately not read — this app authenticates as the account, never as a
    /// single mailbox, so keeping it would be a second credential with nothing to do.
    ///
    /// A handle is accepted only for a namespace the account owns; the postbox decides that, and
    /// refuses with `namespace_not_yours` if it does not agree.
    @discardableResult
    func createIdentity(handle: String? = nil) async throws -> String {
        try await send(
            "/v1/identities",
            method: "POST",
            json: handle.map { ["handle": $0] } ?? [:],
            as: CreatedIdentity.self
        ).address
    }

    func whoami(identity: String) async throws -> WhoAmI {
        try await send("/v1/whoami", query: [.init(name: "identity", value: identity)], as: WhoAmI.self)
    }

    /// The conversation, both halves of it.
    ///
    /// `includeSent` is what turns a listing into a conversation, and is opt-in on the wire because
    /// every other caller of this endpoint reads it as mail addressed to them.
    ///
    /// `includeRead` is not optional here, on the first load or on the poll. The server drops
    /// acknowledged mail from a listing that did not ask for it — right for an agent draining what
    /// is new, wrong for a person reading a thread. Both calls must ask the same question, or the
    /// poll replaces a full conversation with its unread part.
    func inbox(identity: String, wait: Int? = nil) async throws -> InboxResponse {
        var query = [
            URLQueryItem(name: "identity", value: identity),
            URLQueryItem(name: "include_sent", value: "true"),
            URLQueryItem(name: "include_read", value: "true"),
        ]
        // `include_sent=1` is a 400 before the request is even authenticated: axum deserialises a
        // bool from `true`/`false` only.
        if let wait { query.append(URLQueryItem(name: "wait", value: String(wait))) }
        return try await send("/v1/inbox", query: query, as: InboxResponse.self)
    }

    func threads(identity: String) async throws -> [ServerThread] {
        try await send("/v1/threads", query: [.init(name: "identity", value: identity)], as: ThreadsResponse.self).threads ?? []
    }

    func contacts(identity: String) async throws -> ContactsResponse {
        try await send("/v1/contacts", query: [.init(name: "identity", value: identity)], as: ContactsResponse.self)
    }

    func archive(identity: String) async throws -> Set<String> {
        let body = try await send("/v1/archive", query: [.init(name: "identity", value: identity)], as: ArchiveResponse.self)
        return Set(body.archived ?? [])
    }

    // ---- writes --------------------------------------------------------------------------------

    func sendMessage(
        from identity: String,
        to peer: String,
        body text: String,
        threadId: String?,
        attachments: [String] = []
    ) async throws -> SendResponse {
        var payload: [String: Any] = ["to": peer, "body": text, "from": identity]
        if let threadId { payload["thread_id"] = threadId }
        if !attachments.isEmpty { payload["attachments"] = attachments }
        return try await send("/v1/send", method: "POST", json: payload, as: SendResponse.self)
    }

    /// Opening a conversation is reading it, and `ack` is not only a read receipt: it is also how an
    /// agent sharing this mailbox learns a message has been dealt with.
    func ack(identity: String, messageId: String) async throws {
        _ = try await sendRaw("/v1/ack", method: "POST", json: ["message_id": messageId, "identity": identity])
    }

    /// Store a file, and get back the id a send names it by.
    ///
    /// The bytes are the whole body and the metadata rides in headers, which is why this does not
    /// go through `send` — that one encodes JSON, and a file is neither JSON nor small.
    func uploadAttachment(
        identity: String,
        data: Data,
        filename: String,
        mediaType: String
    ) async throws -> UploadedAttachment {
        guard let tokens else { throw AuthError.sessionExpired }
        let url = base.appendingPathComponent("/v1/attachments")

        func attempt(_ bearer: String) async throws -> (Data, HTTPURLResponse) {
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("Bearer \(bearer)", forHTTPHeaderField: "authorization")
            request.setValue("application/json", forHTTPHeaderField: "accept")
            request.setValue("application/octet-stream", forHTTPHeaderField: "content-type")
            request.setValue(identity, forHTTPHeaderField: "x-pigeonpost-identity")
            // A filename is somebody's text and a header is a line-based protocol.
            request.setValue(Self.headerSafe(filename), forHTTPHeaderField: "x-pigeonpost-filename")
            request.setValue(Self.headerSafe(mediaType), forHTTPHeaderField: "x-pigeonpost-media-type")
            // Long enough for a large file on a phone's uplink, which is the slow case this has.
            request.timeoutInterval = 300
            let (body, response) = try await URLSession.shared.upload(for: request, from: data)
            return (body, response as? HTTPURLResponse ?? HTTPURLResponse())
        }

        var (body, response) = try await attempt(try await tokens.token())
        if response.statusCode == 401 {
            (body, response) = try await attempt(try await tokens.renew())
        }
        guard (200..<300).contains(response.statusCode) else {
            let problem = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
            throw APIError(
                status: response.statusCode,
                code: problem?["error"] as? String,
                detail: problem?["detail"] as? String
            )
        }
        do {
            return try Self.decoder.decode(UploadedAttachment.self, from: body)
        } catch {
            throw APIError(status: 200, code: "bad_response", detail: "The postbox answered in a shape this app does not understand.")
        }
    }

    /// The bytes of a file on a message this mailbox holds.
    func downloadAttachment(identity: String, id: String) async throws -> Data {
        guard let tokens else { throw AuthError.sessionExpired }
        let url = base.appendingPathComponent("/v1/attachments/\(id)")

        func attempt(_ bearer: String) async throws -> (Data, HTTPURLResponse) {
            var request = URLRequest(url: url)
            request.setValue("Bearer \(bearer)", forHTTPHeaderField: "authorization")
            request.setValue(identity, forHTTPHeaderField: "x-pigeonpost-identity")
            request.timeoutInterval = 300
            let (body, response) = try await URLSession.shared.data(for: request)
            return (body, response as? HTTPURLResponse ?? HTTPURLResponse())
        }
        var (body, response) = try await attempt(try await tokens.token())
        if response.statusCode == 401 {
            (body, response) = try await attempt(try await tokens.renew())
        }
        guard (200..<300).contains(response.statusCode) else {
            let problem = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
            throw APIError(
                status: response.statusCode,
                code: problem?["error"] as? String,
                detail: problem?["detail"] as? String
            )
        }
        return body
    }

    /// Everything a header cannot carry, removed rather than escaped.
    private static func headerSafe(_ text: String) -> String {
        String(
            text.unicodeScalars
                .filter { $0.value >= 0x20 && $0.value < 0x7f && $0 != "\"" && $0 != "\\" }
                .prefix(120)
                .map(Character.init)
        )
    }

    /// Remove one message from this mailbox, for good.
    ///
    /// Not archiving. Archiving hides a conversation and keeps every byte of it; this is what a
    /// bounded mailbox needs in order to stay usable, and it is local — the other side keeps their
    /// copy and is never told.
    func deleteMessage(identity: String, messageId: String) async throws {
        _ = try await sendRaw("/v1/messages/delete", method: "POST", json: ["message_id": messageId, "identity": identity])
    }

    /// How full this mailbox is. See `Quota` — the thresholds are the server's, not ours.
    func quota(identity: String) async throws -> Quota {
        try await send("/v1/quota", query: [.init(name: "identity", value: identity)], as: Quota.self)
    }

    /// Report the sender of one message. Same shape as `ack`, and a real act: it is what tells the
    /// postbox that an address is sending mail nobody asked for, and it counts against that
    /// sender's standing.
    func reportSpam(identity: String, messageId: String) async throws {
        _ = try await sendRaw("/v1/report-spam", method: "POST", json: ["message_id": messageId, "identity": identity])
    }

    /// Register this device against the acting mailbox, so the postbox can wake it.
    func registerDevice(identity: String, token: String, environment: String) async throws {
        _ = try await sendRaw("/v1/devices", method: "POST", json: [
            "token": token,
            "platform": "apns",
            "environment": environment,
            "identity": identity,
        ])
    }

    /// Stop waking this device — on sign-out, so a phone handed on does not ring for somebody
    /// else's mail.
    func unregisterDevice(token: String) async throws {
        _ = try await sendRaw("/v1/devices/\(token)", method: "DELETE")
    }

    func openThread(identity: String, peer: String, title: String) async throws -> String {
        try await send("/v1/threads", method: "POST", json: ["peer": peer, "title": title, "identity": identity], as: OpenedThread.self).threadId
    }

    func setArchived(identity: String, peer: String, archived: Bool) async throws {
        _ = try await sendRaw("/v1/archive", method: "PUT", json: ["peer": peer, "archived": archived, "identity": identity])
    }

    func putContact(identity: String, peer: String, alias: String?, admission: String, autonomy: String, allowedVerbs: [String]) async throws {
        var payload: [String: Any] = [
            "peer": peer,
            "admission": admission,
            "autonomy": autonomy,
            "allowed_verbs": allowedVerbs,
            "identity": identity,
        ]
        if let alias, !alias.isEmpty { payload["alias"] = alias }
        _ = try await sendRaw("/v1/contacts", method: "PUT", json: payload)
    }

    func removeContact(identity: String, peer: String) async throws {
        _ = try await sendRaw("/v1/contacts", method: "PUT", json: ["peer": peer, "remove": true, "identity": identity])
    }

    // ---- buying a handle -----------------------------------------------------------------------

    /// What this account already owns, and which product sells one. The product id comes from the
    /// postbox rather than from `Config`, so the identifier a receipt is checked against and the
    /// identifier StoreKit is asked for cannot drift apart.
    func handleOffer() async throws -> HandleOffer {
        try await send("/v1/claims/apple", as: HandleOffer.self)
    }

    /// Hand Apple's transaction id to the postbox, which asks Apple what it means. Deliberately not
    /// a receipt or an entitlement flag: this app is in no position to assert what was bought.
    @discardableResult
    func claimHandle(transactionId: String, namespace: String) async throws -> HandleOffer {
        try await send(
            "/v1/claims/apple",
            method: "POST",
            json: ["transaction_id": transactionId, "namespace": namespace],
            as: HandleOffer.self
        )
    }

    // ---- the wire ------------------------------------------------------------------------------

    private func send<T: Decodable>(
        _ path: String,
        method: String = "GET",
        query: [URLQueryItem] = [],
        json: [String: Any]? = nil,
        as type: T.Type
    ) async throws -> T {
        let data = try await sendRaw(path, method: method, query: query, json: json)
        do {
            return try Self.decoder.decode(T.self, from: data)
        } catch {
            throw APIError(status: 200, code: "bad_response", detail: "The postbox answered in a shape this app does not understand.")
        }
    }

    @discardableResult
    private func sendRaw(
        _ path: String,
        method: String = "GET",
        query: [URLQueryItem] = [],
        json: [String: Any]? = nil
    ) async throws -> Data {
        guard let tokens else { throw AuthError.sessionExpired }

        var components = URLComponents(url: base.appendingPathComponent(path), resolvingAgainstBaseURL: false)!
        if !query.isEmpty { components.queryItems = query }
        let url = components.url!

        func attempt(_ bearer: String) async throws -> (Data, HTTPURLResponse) {
            var request = URLRequest(url: url)
            request.httpMethod = method
            request.setValue("Bearer \(bearer)", forHTTPHeaderField: "authorization")
            request.setValue("application/json", forHTTPHeaderField: "accept")
            if let json {
                request.setValue("application/json", forHTTPHeaderField: "content-type")
                request.httpBody = try JSONSerialization.data(withJSONObject: json)
            }
            // A long poll is held open for as long as the postbox will hold it; the default 60s
            // timeout would cut a `wait=25` call short only sometimes, which is the worst kind of
            // bug to look for.
            request.timeoutInterval = 90
            let (data, response) = try await URLSession.shared.data(for: request)
            return (data, response as? HTTPURLResponse ?? HTTPURLResponse())
        }

        var (data, response) = try await attempt(await tokens.token())
        if response.statusCode == 401 {
            // The token was live by its own `exp` and the realm disagreed. Spend the refresh token
            // once and try again; a second 401 is a real one.
            let renewed = try await tokens.renew()
            (data, response) = try await attempt(renewed)
        }
        guard (200..<300).contains(response.statusCode) else {
            let body = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            throw APIError(
                status: response.statusCode,
                code: body?["error"] as? String,
                detail: body?["detail"] as? String
            )
        }
        return data
    }
}
