import Foundation

enum AuthError: Error { case sessionExpired }
@MainActor final class Session {
    enum Status { case signedIn, signedOut }
    var status = Status.signedIn
    var subject: String? = "member"
    var authorizationId = UUID()
    var renewals = 0
    var verifications = 0
    var delayToken = false
    var tokenWaiter: CheckedContinuation<String, Never>?
    func token() async throws -> String {
        if delayToken { return await withCheckedContinuation { tokenWaiter = $0 } }
        return "fixture-member"
    }
    func renew() async throws -> String { renewals += 1; return "fixture-renewed" }
    func verifyStrongSignIn(_ requirement: StrongSignInRequirement) async throws -> UUID {
        verifications += 1; authorizationId = UUID(); await Task.yield(); return authorizationId
    }
}

private final class ApprovalTransport: URLProtocol, @unchecked Sendable {
    @MainActor static var pending: [ApprovalTransport] = []
    @MainActor static var requests: [URLRequest] = []
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "approval-fixture.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() { Task { @MainActor in Self.requests.append(request); Self.pending.append(self) } }
    @MainActor func complete(_ status: Int, _ object: [String: Any], headers: [String: String] = [:]) throws {
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: headers)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: try JSONSerialization.data(withJSONObject: object))
        client?.urlProtocolDidFinishLoading(self)
    }
}

@main enum DeviceApprovalStoreTests {
    @MainActor static func main() async throws {
        URLProtocol.registerClass(ApprovalTransport.self)
        defer { URLProtocol.unregisterClass(ApprovalTransport.self) }
        var checks = 0
        func check(_ value: Bool, _ name: String) { precondition(value, name); checks += 1 }
        func request() async throws -> ApprovalTransport {
            for _ in 0..<400 {
                if !ApprovalTransport.pending.isEmpty { return ApprovalTransport.pending.removeFirst() }
                try await Task.sleep(for: .milliseconds(5))
            }
            preconditionFailure("Expected synthetic device request")
        }
        func response(_ name: String, state: String = "pending", allowed: Bool = true) -> [String: Any] {
            let now = Int(Date().timeIntervalSince1970)
            return ["pairing_id": "pair_"+name, "org": ["id": "org_fixture", "name": "Example"],
                "handle": "/fixture/"+name, "address": "/k/"+name, "creates_agent": false,
                "confirmation": "123 456", "capabilities": ["mailbox"], "requested_at": now,
                "expires_at": now+600, "state": state, "can_approve": state == "pending" && allowed]
        }
        let a = DevicePairingCode("AAAA1111")!, b = DevicePairingCode("BBBB2222")!
        let session = Session()
        let store = DeviceApprovalStore(session: session, base: URL(string: "https://approval-fixture.invalid")!)
        var task = Task { await store.resolve(a) }
        let first = try await request(); try first.complete(200, response("a")); await task.value
        check(store.phase == .review && store.canApprove && store.canDeny, "resolved explicit review")
        let proof = store.proofId
        task = Task { await store.approve(proof: proof) }
        let needsAuth = try await request()
        check(needsAuth.request.url?.path == "/v1/device-pairings/pair_a/approve", "frozen resolved target")
        try needsAuth.complete(401, ["error": "step_up_required", "acr_values": ["mfa"], "max_age": 600]); await task.value
        check(session.renewals == 0 && store.challenge != nil && !store.canApprove, "strong challenge never refreshes or replays")
        let challengedProof = store.proofId
        task = Task { await store.verifySignIn() }
        let refreshed = try await request()
        check(refreshed.request.url?.path == "/v1/device-pairings/resolve", "verification only resolves again")
        try refreshed.complete(200, response("a")); await task.value
        check(session.verifications == 1 && store.phase == .review && store.canApprove, "fresh preview needs another decision")
        let count = ApprovalTransport.requests.count
        await store.approve(proof: challengedProof)
        check(ApprovalTransport.requests.count == count, "pre-authentication hold cannot approve")
        task = Task { await store.approve(proof: store.proofId) }
        let approved = try await request()
        try approved.complete(200, ["approved": true, "address": "/k/a", "handle": "/fixture/a"]); await task.value
        check(store.phase == .approved && !store.canApprove, "explicit new decision completes")

        let old = Task { await store.resolve(a) }; let oldRead = try await request()
        let newer = Task { await store.resolve(b) }; let newRead = try await request()
        try newRead.complete(200, response("b")); await newer.value
        try oldRead.complete(200, response("a")); await old.value
        check(store.preview?.handle == "/fixture/b" && store.code == b, "late read cannot replace another code")

        task = Task { await store.resolve(a) }; let stale = try await request()
        session.subject = "other"; session.authorizationId = UUID(); store.synchronize()
        try stale.complete(401, ["error": "unauthorized"]); await task.value
        check(session.renewals == 0 && ApprovalTransport.pending.isEmpty, "retired 401 cannot renew")
        check(store.preview == nil && store.code == nil && store.phase == .idle, "new account sees no old request")

        session.delayToken = true
        let beforeToken = ApprovalTransport.requests.count
        task = Task { await store.resolve(a) }
        for _ in 0..<400 where session.tokenWaiter == nil { try await Task.sleep(for: .milliseconds(5)) }
        check(session.tokenWaiter != nil, "token acquisition suspended")
        session.authorizationId = UUID(); store.synchronize()
        session.tokenWaiter?.resume(returning: "fixture-after-change"); session.tokenWaiter = nil
        session.delayToken = false; await task.value
        check(ApprovalTransport.requests.count == beforeToken && store.preview == nil, "old operation cannot send after token context changes")

        task = Task { await store.resolve(a) }; let denialRead = try await request()
        try denialRead.complete(200, response("a")); await task.value
        task = Task { await store.deny(proof: store.proofId) }
        let denial = try await request(); try denial.complete(200, ["denied": true])
        let final = try await request()
        check(store.phase == .denying, "denial acknowledgment is not a final verdict")
        try final.complete(200, response("a", state: "approved")); await task.value
        check(store.phase == .approved, "concurrent approval cannot be reported as denied")

        task = Task { await store.resolve(a) }; let another = try await request()
        try another.complete(200, response("a")); await task.value
        task = Task { await store.deny(proof: store.proofId) }
        let denied = try await request(); try denied.complete(200, ["denied": true])
        let expired = try await request(); try expired.complete(410, ["error": "pairing_expired"]); await task.value
        check(store.phase == .unconfirmed, "denial followed by expiry cannot establish final state")

        task = Task { await store.resolve(b) }; let restricted = try await request()
        try restricted.complete(200, response("b", allowed: false)); await task.value
        let deniedCount = ApprovalTransport.requests.count
        await store.approve(proof: store.proofId)
        check(!store.canApprove && ApprovalTransport.requests.count == deniedCount, "permission denied has no approval request")
        task = Task { await store.resolve(b) }; let limited = try await request()
        try limited.complete(429, ["error": "rate_limited"], headers: ["retry-after": "60"]); await task.value
        let limitedCount = ApprovalTransport.requests.count
        await store.resolve(a)
        check(store.retryAt != nil && ApprovalTransport.requests.count == limitedCount, "Retry-After blocks another immediate lookup")
        store.changeCode()
        await store.resolve(b)
        check(store.retryAt != nil && ApprovalTransport.requests.count == limitedCount, "editing the code preserves the rate limit")
        store.retire()
        check(store.preview == nil && store.code == nil && store.challenge == nil, "closing erases request authority")
        print("Device approval store: \(checks) checks passed")
    }
}
