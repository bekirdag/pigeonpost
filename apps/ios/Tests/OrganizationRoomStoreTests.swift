import Foundation

enum AuthError: Error { case sessionExpired }
@MainActor final class Session {
    enum Status { case signedIn, signedOut }
    var status = Status.signedIn
    var subject: String? = "fixture-person"
    var authorizationId = UUID()
    var renewals = 0
    var waitForToken = false
    var tokenWaiter: CheckedContinuation<String, Never>?
    var waitForRenew = false
    var renewalWaiter: CheckedContinuation<String, Never>?
    func token() async throws -> String {
        if waitForToken { return await withCheckedContinuation { tokenWaiter = $0 } }
        return "fixture-token"
    }
    func renew() async throws -> String {
        renewals += 1
        if waitForRenew { return await withCheckedContinuation { renewalWaiter = $0 } }
        return "fixture-renewed"
    }
}

private final class DirectoryTransport: URLProtocol, @unchecked Sendable {
    @MainActor static var pending: [DirectoryTransport] = []
    @MainActor static var requests: [URLRequest] = []
    @MainActor static var bodies: [Data] = []
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "directory-fixture.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        Task { @MainActor in
            var body = request.httpBody ?? Data()
            if body.isEmpty, let stream = request.httpBodyStream {
                stream.open(); defer { stream.close() }
                var buffer = [UInt8](repeating: 0, count: 2048)
                while stream.hasBytesAvailable {
                    let count = stream.read(&buffer, maxLength: buffer.count)
                    if count <= 0 { break }; body.append(contentsOf: buffer.prefix(count))
                }
            }
            Self.requests.append(request); Self.bodies.append(body); Self.pending.append(self)
        }
    }
    @MainActor func complete(_ status: Int, _ value: [String: Any], headers: [String: String] = [:]) throws {
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil,
                                       headerFields: headers.merging(["content-type": "application/json"]) { old, _ in old })!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: try JSONSerialization.data(withJSONObject: value))
        client?.urlProtocolDidFinishLoading(self)
    }
    @MainActor func fail() { client?.urlProtocol(self, didFailWithError: URLError(.networkConnectionLost)) }
}

@main enum OrganizationRoomStoreTests {
    @MainActor static func main() async throws {
        URLProtocol.registerClass(DirectoryTransport.self)
        defer { URLProtocol.unregisterClass(DirectoryTransport.self) }
        var checks = 0
        func check(_ value: Bool, _ message: String) { precondition(value, message); checks += 1 }
        func request(_ path: String? = nil) async throws -> DirectoryTransport {
            for _ in 0..<400 {
                if !DirectoryTransport.pending.isEmpty {
                    let next = DirectoryTransport.pending.removeFirst()
                    if let path { check(next.request.url?.path == path, "exact scoped request path") }
                    return next
                }
                try await Task.sleep(for: .milliseconds(5))
            }
            preconditionFailure("Expected isolated room request")
        }
        func org(_ mailbox: String, status: String = "active") -> [String: Any] {
            ["id": "org_fixture", "name": "Example", "status": status, "namespace": "example",
             "my_roles": [], "settings": ["rooms_creation": "everyone"], "my_mailbox": ["address": mailbox]]
        }
        func room(_ id: String = "g_fixture") -> [String: Any] {
            ["id": id, "name": "Design", "member_count": 2, "my_state": NSNull(), "join_policy": "public"]
        }
        func preview(_ state: String? = nil, policy: String = "public", canJoin: Bool = true) -> [String: Any] {
            ["id": "g_fixture", "name": "Design", "my_state": state as Any? ?? NSNull(), "join_policy": policy,
             "can_join": canJoin, "member_count": 2, "needs": state == "invited" ? "invitation" : "join"]
        }
        func group(_ id: String = "g_fixture", org: String = "org_fixture", member: Bool = true, creator: String = "/k/member") -> [String: Any] {
            var result: [String: Any] = ["id": id, "name": "Design", "org_id": org, "created_by": creator,
                                       "activation_state": "active", "my": ["role": "owner", "state": "keying"]]
            if !member { result["my"] = NSNull() }
            return result
        }
        let session = Session()
        var mailbox = "/k/member", enabled = true
        let store = OrganizationRoomStore(session: session, mailbox: { mailbox }, enabled: { enabled }, base: URL(string: "https://directory-fixture.invalid")!)
        func load(_ address: String = "/k/member", roomId: String = "g_fixture") async throws {
            let task = Task { await store.loadOrganizations() }
            let organizations = try await request("/v1/orgs")
            try organizations.complete(200, ["orgs": [org(address)], "operated": []])
            let rooms = try await request("/v1/rooms")
            try rooms.complete(200, ["rooms": [room(roomId)]])
            await task.value
        }
        func inspect(_ state: String? = nil, policy: String = "public", canJoin: Bool = true) async throws {
            let task = Task { await store.inspect("g_fixture") }
            let resolving = try await request("/v1/groups/resolve")
            try resolving.complete(200, preview(state, policy: policy, canJoin: canJoin))
            await task.value
        }
        try await load()
        check(store.rooms.count == 1 && store.canCreate && !store.loading, "directory completes without creating a room")
        check(store.selectedOrganizationId == "org_fixture", "only matching canonical context is selected")
        try await inspect()
        check(store.reviewAction == .join, "a fresh public preview offers an explicit join")
        let proof = store.reviewProof
        let joining = Task { await store.openReviewedRoom(proof: proof) }
        let join = try await request("/v1/groups/g_fixture/join")
        check(join.request.value(forHTTPHeaderField: "x-pigeonpost-identity") == mailbox, "join names the captured acting inbox")
        try join.complete(200, [:])
        let joined = try await request("/v1/groups/g_fixture")
        try joined.complete(200, group())
        let opening = await joining.value
        check(opening != nil && store.allows(opening!), "live organization and keying membership confirm navigation")
        try await inspect("invited", policy: "private", canJoin: false)
        check(store.reviewAction == .acceptInvitation, "private membership is accepted rather than public-joined")
        let count = DirectoryTransport.requests.count
        check(await store.openReviewedRoom(proof: proof) == nil && DirectoryTransport.requests.count == count, "a stale preview choice sends nothing")
        let accepting = Task { await store.openReviewedRoom(proof: store.reviewProof) }
        let accept = try await request("/v1/groups/g_fixture/invitation")
        check((try JSONSerialization.jsonObject(with: DirectoryTransport.bodies.last!) as? [String: Any])?["accept"] as? Bool == true, "explicit invitation acceptance")
        try accept.complete(200, [:])
        let accepted = try await request("/v1/groups/g_fixture"); try accepted.complete(200, group())
        check(await accepting.value != nil, "an accepted invitation still needs live group confirmation")
        for wrong in [group(org: "org_other"), group(member: false)] {
            try await inspect("active", canJoin: false)
            let task = Task { await store.openReviewedRoom(proof: store.reviewProof) }
            let current = try await request("/v1/groups/g_fixture"); try current.complete(200, wrong)
            check(await task.value == nil && store.mustReviewAgain && store.reviewAction == .unavailable,
                  "foreign organization or absent membership cannot open cached history")
        }

        let old = Task { await store.loadOrganizations() }; let oldRead = try await request("/v1/orgs")
        session.subject = "second-person"; session.authorizationId = UUID()
        check(store.organizations.isEmpty && store.rooms.isEmpty && store.preview == nil, "identity change hides old data before lifecycle callbacks")
        store.synchronize()
        try oldRead.complete(401, ["error": "unauthorized"]); await old.value
        check(session.renewals == 0 && DirectoryTransport.pending.isEmpty, "retired 401 cannot renew another person's token")
        check(!store.loading && store.pendingCreation == nil, "new identity inherits no loading state or create intent")

        session.waitForToken = true
        let tokenCount = DirectoryTransport.requests.count
        let waiting = Task { await store.loadOrganizations() }
        for _ in 0..<400 where session.tokenWaiter == nil { try await Task.sleep(for: .milliseconds(5)) }
        check(session.tokenWaiter != nil, "token acquisition can be suspended")
        session.authorizationId = UUID(); store.synchronize()
        session.tokenWaiter?.resume(returning: "fixture-after-reauthorization"); session.tokenWaiter = nil
        session.waitForToken = false; await waiting.value
        check(DirectoryTransport.requests.count == tokenCount && store.organizations.isEmpty, "same-person reauthorization retires the old request before transport")
        try await load()
        let first = Task { await store.loadRooms(search: "old") }; let firstRead = try await request("/v1/rooms")
        let second = Task { await store.loadRooms(search: "new") }; let secondRead = try await request("/v1/rooms")
        try secondRead.complete(200, ["rooms": [room("g_new")]]); await second.value
        try firstRead.complete(200, ["rooms": [room("g_old")]]); await first.value
        check(store.rooms.map(\.id) == ["g_new"] && !store.loading, "late search cannot replace the current directory")
        let formerMailbox = Task { await store.loadRooms() }; let formerRead = try await request("/v1/rooms")
        mailbox = "/k/other"
        check(store.rooms.isEmpty && store.organizations.isEmpty, "mailbox switch immediately hides prior room data")
        try await load(mailbox, roomId: "g_other")
        try formerRead.complete(401, ["error": "unauthorized"]); await formerMailbox.value
        check(session.renewals == 0 && store.rooms.map(\.id) == ["g_other"], "old-mailbox expiry cannot renew or alter the new directory")
        let denied = Task { await store.loadRooms() }; let deniedRead = try await request("/v1/rooms")
        try deniedRead.complete(403, ["error": "not_org_member"]); await denied.value
        check(store.organizations.isEmpty && store.rooms.isEmpty && store.preview == nil && !store.canCreate, "denied membership erases directory and decisions")
        let refusedCount = DirectoryTransport.requests.count
        await store.loadRooms()
        check(DirectoryTransport.requests.count == refusedCount, "denial never falls back to a personal directory")

        mailbox = "/k/member"; try await load()
        func create(_ name: String = "Design") async -> OrganizationRoomOpening? {
            await store.createRoom(in: store.context!, organizationId: "org_fixture", name: name, description: "Notes", joinPolicy: .members, postPolicy: .everyone, listed: true)
        }
        let lost = Task { await create() }; let lostRequest = try await request("/v1/groups")
        let originalBody = DirectoryTransport.bodies.last!
        lostRequest.fail(); check(await lost.value == nil, "lost create response is uncertain")
        check(store.pendingCreation?.data == originalBody && !store.busy, "uncertain create retains the exact intent")
        let alteredCount = DirectoryTransport.requests.count
        check(await create("Changed") == nil && DirectoryTransport.requests.count == alteredCount, "an uncertain attempt cannot silently become a different creation")
        let retry = Task { await create() }; let retryRequest = try await request("/v1/groups")
        check(DirectoryTransport.bodies.last == originalBody, "explicit create retry keeps bytes and idempotency key")
        try retryRequest.complete(201, ["group": group("g_created")])
        let confirmation = try await request("/v1/groups/g_created"); try confirmation.complete(200, group("g_created"))
        check(await retry.value != nil && store.pendingCreation == nil, "confirmed create clears the pending intent")
        let limited = Task { await create() }; let limitedRequest = try await request("/v1/groups")
        try limitedRequest.complete(429, ["error": "rate_limited"], headers: ["Retry-After": "120"]); _ = await limited.value
        let deadline = store.retryAt
        store.discardCreation(in: store.context!, organizationId: "org_fixture")
        let limitedCount = DirectoryTransport.requests.count
        check(await create("Another") == nil && DirectoryTransport.requests.count == limitedCount && store.retryAt == deadline,
              "discarding a draft cannot clear server backoff")
        mailbox = "/k/other"; store.synchronize()
        check(store.retryAt == deadline, "mailbox switches preserve account-level backoff")
        mailbox = "/k/member"; store.synchronize(); try await load()
        let cancelled = Task { await store.loadRooms() }; _ = try await request("/v1/rooms")
        cancelled.cancel(); await cancelled.value
        check(!store.loading && store.error == nil, "cancellation finishes its own spinner without displaying an error")

        let oldScope = store.context!
        session.authorizationId = UUID(); store.synchronize(); try await load()
        let staleCreateCount = DirectoryTransport.requests.count
        let staleCreate = await store.createRoom(in: oldScope, organizationId: "org_fixture", name: "Old form", description: "", joinPolicy: .members, postPolicy: .everyone, listed: true)
        check(staleCreate == nil && DirectoryTransport.requests.count == staleCreateCount, "an old form cannot create under a new authorization")
        let wrongOrganization = await store.createRoom(in: store.context!, organizationId: "org_other", name: "Old organization", description: "", joinPolicy: .members, postPolicy: .everyone, listed: true)
        check(wrongOrganization == nil && DirectoryTransport.requests.count == staleCreateCount, "an old organization choice cannot be redirected into the current organization")
        let lateCreation = Task { await create() }; let lateCreationRequest = try await request("/v1/groups")
        let beforeFollowup = DirectoryTransport.requests.count
        session.subject = "third-person"; session.authorizationId = UUID(); store.synchronize()
        try lateCreationRequest.complete(201, ["group": group("g_old_created")])
        check(await lateCreation.value == nil && DirectoryTransport.requests.count == beforeFollowup && store.pendingCreation == nil,
              "late creation cannot query or open a room under a different account")
        try await load()
        session.waitForRenew = true
        let renewing = Task { await store.loadRooms() }; let expired = try await request("/v1/rooms")
        try expired.complete(401, ["error": "unauthorized"])
        for _ in 0..<400 where session.renewalWaiter == nil { try await Task.sleep(for: .milliseconds(5)) }
        check(session.renewalWaiter != nil, "ordinary renewal suspended")
        let beforeRenewedSend = DirectoryTransport.requests.count
        session.authorizationId = UUID(); store.synchronize()
        session.renewalWaiter?.resume(returning: "fixture-after-renewal"); session.renewalWaiter = nil
        session.waitForRenew = false; await renewing.value
        check(DirectoryTransport.requests.count == beforeRenewedSend && store.rooms.isEmpty, "renewal completion cannot resend after authorization changes")
        try await load(); try await inspect()
        let oldJoin = Task { await store.openReviewedRoom(proof: store.reviewProof) }
        let oldJoinRequest = try await request("/v1/groups/g_fixture/join")
        mailbox = "/k/other"; store.synchronize(); try await load(mailbox)
        let newCreation = Task { await create() }; let newCreationRequest = try await request("/v1/groups")
        let beforeOldJoin = DirectoryTransport.requests.count
        try oldJoinRequest.complete(200, [:])
        check(await oldJoin.value == nil && DirectoryTransport.requests.count == beforeOldJoin && store.busy,
              "old join cannot make a follow-up request or clear a new mailbox's pending mutation")
        try newCreationRequest.complete(201, ["group": group("g_new_created", creator: mailbox)])
        let newConfirmation = try await request("/v1/groups/g_new_created")
        try newConfirmation.complete(200, group("g_new_created", creator: mailbox))
        check(await newCreation.value != nil && !store.busy && store.pendingCreation == nil, "new mailbox owns its confirmed creation independently")
        let renewals = session.renewals
        let disabled = Task { await store.loadOrganizations() }; let disabledRead = try await request("/v1/orgs")
        enabled = false
        check(store.rooms.isEmpty && store.organizations.isEmpty && store.preview == nil, "disabled feature immediately hides enterprise data")
        store.synchronize(); try disabledRead.complete(401, ["error": "unauthorized"]); await disabled.value
        check(session.renewals == renewals && !store.loading, "disabled-feature work cannot refresh credentials")
        check(DirectoryTransport.requests.allSatisfy { $0.url?.host == "directory-fixture.invalid" }, "race tests never leave the isolated transport")
        print("Organization room store: \(checks) checks passed")
    }
}
