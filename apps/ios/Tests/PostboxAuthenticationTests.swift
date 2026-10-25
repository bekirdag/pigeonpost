import Foundation

enum AuthError: Error { case sessionExpired }
@MainActor final class Session {
    var renewals = 0
    func token() async throws -> String { "fixture-original" }
    func renew() async throws -> String { renewals += 1; return "fixture-renewed" }
}

private final class AuthTransport: URLProtocol, @unchecked Sendable {
    struct Reply { let status: Int; let body: Data; var challenge: String? = nil }
    @MainActor static var replies: [Reply] = []
    @MainActor static var requests: [URLRequest] = []
    @MainActor static var bodies: [[String: Any]] = []
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "postbox-auth.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        Task { @MainActor in
            Self.requests.append(request)
            var bytes = request.httpBody ?? Data()
            if bytes.isEmpty, let stream = request.httpBodyStream {
                stream.open(); defer { stream.close() }
                var buffer = [UInt8](repeating: 0, count: 1024)
                while stream.hasBytesAvailable {
                    let count = stream.read(&buffer, maxLength: buffer.count)
                    if count <= 0 { break }; bytes.append(contentsOf: buffer.prefix(count))
                }
            }
            Self.bodies.append((try? JSONSerialization.jsonObject(with: bytes)) as? [String: Any] ?? [:])
            guard !Self.replies.isEmpty else {
                client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse)); return
            }
            let reply = Self.replies.removeFirst()
            var headers = ["content-type": "application/json"]
            if let challenge = reply.challenge { headers["www-authenticate"] = challenge }
            let response = HTTPURLResponse(url: request.url!, statusCode: reply.status, httpVersion: nil, headerFields: headers)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: reply.body)
            client?.urlProtocolDidFinishLoading(self)
        }
    }
}

@main enum PostboxAuthenticationTests {
    @MainActor static func main() async throws {
        var checks = 0
        func check(_ value: Bool, _ name: String) { precondition(value, name); checks += 1 }
        func data(_ object: [String: Any]) throws -> Data { try JSONSerialization.data(withJSONObject: object) }
        let body = try data(["error": "step_up_required", "acr_values": ["mfa"], "max_age": 600])
        let good = APIError(status: 401, body: body)
        check(good.needsStrongSignIn && good.signInRequirement?.requestedACR == "mfa", "body challenge")
        check(good.signInRequirement?.maxAge == 600, "bounded freshness")
        let header = "Bearer error=\"insufficient_user_authentication\", acr_values=\"mfa urn:example:strong\", max_age=600"
        let fromHeader = APIError(status: 401, body: Data(), authenticationChallenge: header)
        check(fromHeader.needsStrongSignIn, "RFC challenge")
        check(fromHeader.signInRequirement?.acrValues == ["mfa", "urn:example:strong"], "ordered alternatives")
        check(fromHeader.signInRequirement?.maxAge == 600, "unquoted integer")
        let quoted = header.replacingOccurrences(of: "max_age=600", with: "max_age=\"0\"")
        check(APIError(status: 401, body: Data(), authenticationChallenge: quoted).signInRequirement?.maxAge == 0, "fresh authentication")
        check(try APIError(status: 401, body: data(["error": "step_up_required", "acr_values": "mfa", "max_age": 600])).signInRequirement != nil, "legacy string ACR")
        check(!APIError(status: 403, body: body).needsStrongSignIn, "non-401 never starts authorization")
        for bad: [String: Any] in [
            ["acr_values": [], "max_age": 600], ["acr_values": ["mfa", "mfa"], "max_age": 600],
            ["acr_values": ["mfa\nanything"], "max_age": 600], ["acr_values": ["mfa"], "max_age": true],
            ["acr_values": ["mfa"], "max_age": -1], ["acr_values": ["mfa"], "max_age": 601],
            ["acr_values": ["mfa"], "max_age": 0.5], ["acr_values": [String(repeating: "a", count: 129)], "max_age": 600]
        ] {
            let error = APIError(status: 401, body: try data(bad.merging(["error": "step_up_required"]) { _, new in new }))
            check(error.needsStrongSignIn && error.signInRequirement == nil, "malformed challenge stops without assumptions")
        }
        for bad in [
            "Bearer error=\"insufficient_user_authentication\", acr_values=\"mfa\", acr_values=\"other\", max_age=600",
            "Bearer error=\"insufficient_user_authentication\", acr_values=\"mfa\", max_age=600, max_age=0",
            "Bearer error=\"insufficient_user_authentication\", acr_values=\"mfa\nother\", max_age=600",
            "Bearer error=\"insufficient_user_authentication\", acr_values=\"mfa\", max_age=-1"
        ] {
            let error = APIError(status: 401, body: Data(), authenticationChallenge: bad)
            check(error.needsStrongSignIn && error.signInRequirement == nil, "malformed header cannot cause an automatic retry")
        }

        URLProtocol.registerClass(AuthTransport.self)
        defer { URLProtocol.unregisterClass(AuthTransport.self) }
        let ok = Data("{\"ok\":true}".utf8)
        let ordinary = Data("{\"error\":\"unauthorized\"}".utf8)
        for variant in 0..<4 {
            AuthTransport.requests = []
            let tokens = Session()
            let client = PostboxClient(base: URL(string: "https://postbox-auth.invalid")!, tokens: tokens)
            if variant == 0 { AuthTransport.replies = [.init(status: 401, body: body)] }
            if variant == 1 { AuthTransport.replies = [.init(status: 401, body: Data(), challenge: header)] }
            if variant == 2 { AuthTransport.replies = [.init(status: 401, body: ordinary), .init(status: 200, body: ok)] }
            if variant == 3 { AuthTransport.replies = [.init(status: 403, body: try data(["error": "step_up_unavailable"]))] }
            var refusal: APIError?
            do { _ = try await client.sendRaw("/v1/device-pairings/pair_fixture/approve", method: "POST", json: ["user_code": "ABCD-1234"], timeout: 5) }
            catch let error as APIError { refusal = error }
            check(AuthTransport.requests.count == (variant == 2 ? 2 : 1), "challenge cannot replay the approval")
            check(tokens.renewals == (variant == 2 ? 1 : 0), "only ordinary expiration renews")
            check(AuthTransport.requests.allSatisfy { $0.url?.host == "postbox-auth.invalid" && $0.url?.query == nil }, "fixed endpoint without token query")
            if variant < 2 { check(refusal?.needsStrongSignIn == true && refusal?.signInRequirement != nil, "challenge returned to caller") }
            if variant == 2 {
                check(refusal == nil, "ordinary expiry still recovers")
                check(AuthTransport.requests.last?.value(forHTTPHeaderField: "authorization") == "Bearer fixture-renewed", "renewed token used once")
            }
            if variant == 3 { check(refusal?.code == "step_up_unavailable", "unconfigured strong sign-in is terminal") }
        }
        let tokens = Session()
        let client = PostboxClient(base: URL(string: "https://postbox-auth.invalid")!, tokens: tokens)
        let code = DevicePairingCode("ABCD1234")!
        let now = Int(Date().timeIntervalSince1970)
        let pending: [String: Any] = ["pairing_id": "pair_fixture", "org": ["id": "org_fixture", "name": "Example"],
            "handle": "/fixture/build", "address": "/k/fixture", "state": "pending", "creates_agent": false,
            "confirmation": "123 456", "capabilities": ["mailbox"], "requested_at": now, "expires_at": now + 600, "can_approve": true]
        AuthTransport.requests = []; AuthTransport.bodies = []
        AuthTransport.replies = [.init(status: 200, body: try data(pending))]
        let preview = try await client.resolveDevicePairing(code)
        check(preview.isValid && preview.address == "/k/fixture", "actual preview decoding")
        check(AuthTransport.requests.last?.url?.path == "/v1/device-pairings/resolve", "resolve path")
        check(AuthTransport.bodies.last?["user_code"] as? String == "ABCD-1234", "code only in POST body")
        AuthTransport.replies = [.init(status: 200, body: try data(["approved": true, "address": "/k/fixture", "handle": "/fixture/build"]))]
        try await client.approveDevicePairing(preview, code: code)
        check(AuthTransport.requests.last?.url?.path == "/v1/device-pairings/pair_fixture/approve", "exact resolved request approval")
        AuthTransport.replies = [.init(status: 200, body: try data(["denied": true]))]
        try await client.denyDevicePairing(preview, code: code)
        check(AuthTransport.requests.last?.url?.path == "/v1/device-pairings/pair_fixture/deny", "deny path")
        check(AuthTransport.requests.allSatisfy { $0.httpMethod == "POST" && $0.url?.query == nil && $0.value(forHTTPHeaderField: "x-pigeonpost-identity") == nil }, "human JWT without mailbox impersonation")
        for malformed: [String: Any] in [pending.merging(["pairing_id": "../different"]) { _, new in new }, ["can_approve": true]] {
            AuthTransport.replies = [.init(status: 200, body: try data(malformed))]
            do { _ = try await client.resolveDevicePairing(code); preconditionFailure("malformed preview accepted") }
            catch let error as APIError { check(error.code == "bad_response", "malformed preview refused") }
        }
        AuthTransport.replies = [.init(status: 200, body: try data(["approved": true, "address": "/k/another", "handle": "/fixture/build"]))]
        do { try await client.approveDevicePairing(preview, code: code); preconditionFailure("wrong target acknowledged") }
        catch let error as APIError { check(error.code == "bad_response", "response cannot silently change target") }
        check(tokens.renewals == 0, "pairing success requires no credential exchange")
        print("Postbox authentication: \(checks) checks passed")
    }
}
