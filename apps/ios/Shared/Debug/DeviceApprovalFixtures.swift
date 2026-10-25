#if DEBUG
import Foundation

/// Explicit fixtures only: no real credential, Keychain, browser or production origin is used.
@MainActor enum DeviceApprovalFixtures {
    private static let session = DeviceApprovalFixtureSession()
    static var enabled: Bool { Fixtures.enabled && CommandLine.arguments.contains("-device-approval-fixtures") }
    static func store(session realSession: Session) -> DeviceApprovalStore {
        guard enabled else { return DeviceApprovalStore(session: realSession) }
        URLProtocol.registerClass(DeviceApprovalFixtureTransport.self)
        return DeviceApprovalStore(session: session, base: URL(string: "https://device-approval-fixture.invalid")!)
    }
    static func stage(router: DeviceApprovalRouter) {
        guard enabled, CommandLine.arguments.contains("-device-approval-link-fixture") else { return }
        router.receive(DevicePairingCode("ABCD1234")!)
    }
    fileprivate static var verified: Bool { session.verified }
}

@MainActor private final class DeviceApprovalFixtureSession: DeviceApprovalSession {
    let subject: String? = "fixture-member"
    var authorizationId = UUID()
    let deviceSignedIn = true
    var verified = false
    func token() async throws -> String { "synthetic-device-member" }
    func renew() async throws -> String { "synthetic-device-member" }
    func verifyStrongSignIn(_ requirement: StrongSignInRequirement) async throws -> UUID {
        await Task.yield(); verified = true; authorizationId = UUID(); return authorizationId
    }
}

private final class DeviceApprovalFixtureTransport: URLProtocol {
    @MainActor private static var state = "pending"
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "device-approval-fixture.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        var data = request.httpBody ?? Data()
        if data.isEmpty, let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var bytes = [UInt8](repeating: 0, count: 2048)
            while stream.hasBytesAvailable {
                let count = stream.read(&bytes, maxLength: bytes.count)
                if count <= 0 { break }; data.append(contentsOf: bytes.prefix(count))
            }
        }
        Task { @MainActor in
            guard DeviceApprovalFixtures.enabled else { client?.urlProtocol(self, didFailWithError: URLError(.unsupportedURL)); return }
            let body = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
            let path = request.url!.path
            var status = 200
            var answer: [String: Any]
            if path == "/v1/device-pairings/resolve" {
                if DevicePairingCode(body["user_code"] as? String ?? "")?.rawValue != "ABCD1234" {
                    status = 404; answer = ["error": "pairing_not_found"]
                } else {
                    let now = Int(Date().timeIntervalSince1970)
                    let expired = CommandLine.arguments.contains("-device-approval-expired")
                    let forbidden = CommandLine.arguments.contains("-device-approval-forbidden")
                    answer = ["pairing_id": "pair_fixture", "org": ["id": "org_fixture", "name": "Example Studio"],
                        "handle": "/example/assistant", "creates_agent": true, "label": "My workstation",
                        "confirmation": "123 456", "capabilities": ["mailbox"], "ip_country": "TR",
                        "requested_at": now-600, "expires_at": expired ? now-1 : now+600,
                        "state": Self.state, "can_approve": !forbidden && Self.state == "pending"]
                    if forbidden { answer["reason"] = "delegation_required" }
                }
            } else if path == "/v1/device-pairings/pair_fixture/approve" {
                if !DeviceApprovalFixtures.verified {
                    status = 401; answer = ["error": "step_up_required", "acr_values": ["mfa"], "max_age": 600]
                } else {
                    Self.state = "approved"
                    answer = ["approved": true, "handle": "/example/assistant", "address": "/k/fixture-agent"]
                }
            } else if path == "/v1/device-pairings/pair_fixture/deny" {
                Self.state = CommandLine.arguments.contains("-device-approval-deny-race") ? "approved" : "denied"
                answer = ["denied": true]
            } else { status = 404; answer = ["error": "not_found"] }
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: ["Content-Type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: (try? JSONSerialization.data(withJSONObject: answer)) ?? Data())
            client?.urlProtocolDidFinishLoading(self)
        }
    }
}
#endif
