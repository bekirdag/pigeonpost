import Foundation
import AuthenticationServices

enum Fixtures { static let enabled = false }
@MainActor enum Keychain {
    static var values: [String: String] = [:]
    static func read(_ key: String) -> String? { values[key] }
    static func write(_ key: String, _ value: String) { values[key] = value }
    static func delete(_ key: String) { values.removeValue(forKey: key) }
}

private final class RealmTransport: URLProtocol, @unchecked Sendable {
    @MainActor static var pending: [RealmTransport] = []
    @MainActor static var count = 0
    private let lock = NSLock()
    private var finished = false
    override class func canInit(with request: URLRequest) -> Bool { true } // Never fall through to a real realm.
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() { lock.lock(); finished = true; lock.unlock() }
    override func startLoading() {
        Task { @MainActor in
            precondition(request.url?.host == "auth.pigeonpost.dev", "Only the fixed realm may be requested")
            Self.count += 1; Self.pending.append(self)
        }
    }
    @MainActor func complete(_ status: Int, _ body: Data) {
        lock.lock(); let deliver = !finished; finished = true; lock.unlock()
        guard deliver else { return }
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: ["content-type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }
}

@main enum SessionLifetimeTests {
    @MainActor static func main() async throws {
        var checks = 0
        func check(_ value: Bool, _ name: String) {
            if !value { FileHandle.standardError.write(Data(("FAIL: " + name + "\n").utf8)); exit(1) }
            checks += 1
        }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [RealmTransport.self]
        configuration.timeoutIntervalForRequest = 5
        let network = URLSession(configuration: configuration)
        defer { network.invalidateAndCancel() }
        func callback(_ url: URL) async throws -> URL {
            let query = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems!
            let state = query.first { $0.name == "state" }!.value!
            var result = URLComponents(string: Config.OIDC.redirectURI)!
            result.queryItems = [.init(name: "state", value: state), .init(name: "code", value: "synthetic-code")]
            return result.url!
        }
        func token(_ subject: String, expired: Bool = false) throws -> String {
            let claims: [String: Any] = ["sub": subject, "preferred_username": subject,
                                       "exp": expired ? 1 : Int(Date().timeIntervalSince1970) + 3600]
            let data = try JSONSerialization.data(withJSONObject: claims)
            return "fixture." + data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") + ".fixture"
        }
        func reply(_ subject: String) throws -> Data {
            try JSONSerialization.data(withJSONObject: ["access_token": token(subject), "refresh_token": "fixture-rt-" + subject])
        }
        func request() async throws -> RealmTransport {
            for _ in 0..<400 {
                if !RealmTransport.pending.isEmpty { return RealmTransport.pending.removeFirst() }
                try await Task.sleep(for: .milliseconds(5))
            }
            preconditionFailure("Expected isolated realm request")
        }

        Keychain.values = [:]
        let retired = Session(network: network, browserAuthorization: callback)
        let login = Task { await retired.signIn() }
        let delayed = try await request()
        check(delayed.request.url?.path.hasSuffix("/token") == true, "authorization code exchange")
        retired.forget()
        delayed.complete(200, try reply("retired")); await login.value
        check(retired.status == .signedOut && retired.subject == nil, "late authorization must not revive sign-out")
        check(Keychain.values.isEmpty, "retired authorization cannot write the Keychain")

        Keychain.values = ["access_token": try token("shared", expired: true), "refresh_token": "fixture-rt-shared"]
        let shared = Session(network: network, browserAuthorization: callback)
        let generation = shared.authorizationId
        let before = RealmTransport.count
        let refreshA = Task { try await shared.renew() }
        let refreshB = Task { try await shared.renew() }
        let refresh = try await request()
        check(RealmTransport.count == before + 1, "rotating refresh is shared")
        refresh.complete(200, try reply("shared"))
        let a = try await refreshA.value, b = try await refreshB.value
        check(a == b && Keychain.values["access_token"] == a, "all waiters receive the accepted refresh")
        check(shared.authorizationId == generation && shared.subject == "shared", "ordinary refresh preserves authorization identity")

        Keychain.values = [:]
        let competing = Session(network: network, browserAuthorization: callback)
        let oldLogin = Task { await competing.signIn() }
        let oldExchange = try await request()
        let freshLogin = Task { await competing.signIn(otherAccount: true) }
        let freshExchange = try await request()
        freshExchange.complete(200, try reply("newest")); await freshLogin.value
        let freshGeneration = competing.authorizationId
        oldExchange.complete(200, try reply("older")); await oldLogin.value
        check(competing.subject == "newest" && competing.authorizationId == freshGeneration, "older browser success cannot replace newer account")
        check(Keychain.values["refresh_token"] == "fixture-rt-newest", "newer refresh credential survives")

        let failedLogin = Task { await competing.signIn() }
        let failedExchange = try await request()
        let replacement = Task { await competing.signIn(otherAccount: true) }
        let replacementExchange = try await request()
        replacementExchange.complete(200, try reply("replacement")); await replacement.value
        failedExchange.complete(400, Data("{\"error\":\"invalid_grant\"}".utf8)); await failedLogin.value
        check(competing.subject == "replacement" && competing.lastError == nil, "retired authorization failure cannot disturb newer account")

        let cancelled = Session(network: network, browserAuthorization: { _ in throw AuthError.cancelled })
        let cancelledGeneration = cancelled.authorizationId
        await cancelled.signIn()
        check(cancelled.subject == "replacement" && cancelled.lastError == nil && cancelled.authorizationId == cancelledGeneration, "cancelled authorization preserves current account")

        Keychain.values = ["access_token": try token("logout"), "refresh_token": "fixture-rt-logout"]
        let loggingOut = Session(network: network, browserAuthorization: callback)
        let logout = Task { await loggingOut.signOut() }
        let logoutRequest = try await request()
        check(logoutRequest.request.url?.path.hasSuffix("/logout") == true && loggingOut.status == .signedOut, "local sign-out precedes realm response")
        let duringLogout = Task { await loggingOut.signIn(otherAccount: true) }
        let afterLogoutExchange = try await request()
        afterLogoutExchange.complete(200, try reply("after-logout")); await duringLogout.value
        logoutRequest.complete(204, Data()); await logout.value
        check(loggingOut.subject == "after-logout" && loggingOut.status == .signedIn, "late logout response cannot forget a newer sign-in")

        Keychain.values = ["access_token": try token("refresh-old", expired: true), "refresh_token": "fixture-rt-refresh-old"]
        let replacingRefresh = Session(network: network, browserAuthorization: callback)
        let obsoleteRefresh = Task { () -> Bool in
            do { _ = try await replacingRefresh.renew(); return true } catch { return false }
        }
        let obsoleteRequest = try await request()
        let replacementLogin = Task { await replacingRefresh.signIn(otherAccount: true) }
        let newerRequest = try await request()
        newerRequest.complete(200, try reply("refresh-new")); await replacementLogin.value
        obsoleteRequest.complete(400, Data("{\"error\":\"invalid_grant\"}".utf8))
        let obsoleteSucceeded = await obsoleteRefresh.value
        check(!obsoleteSucceeded && replacingRefresh.subject == "refresh-new", "old refresh refusal cannot forget a replacement")
        let renewNew = Task { try await replacingRefresh.renew() }
        let newRefreshRequest = try await request()
        newRefreshRequest.complete(200, try reply("refresh-new")); _ = try await renewNew.value
        check(replacingRefresh.subject == "refresh-new" && Keychain.values["refresh_token"] == "fixture-rt-refresh-new", "new session remains refreshable after old task cleanup")

        Keychain.values = ["access_token": try token("bound", expired: true), "refresh_token": "fixture-rt-bound"]
        let bound = Session(network: network, browserAuthorization: callback)
        let changedSubject = Task { () -> Bool in
            do { _ = try await bound.renew(); return true } catch { return false }
        }
        let badRefresh = try await request()
        badRefresh.complete(200, try reply("different-person"))
        check(await changedSubject.value == false, "refresh cannot switch the signed-in subject")
        check(bound.status == .signedOut && Keychain.values.isEmpty, "mismatched refresh retires only its current session")

        var controllers: [ASAuthorizationController] = []
        let native = AppleAuthorization(performRequests: { controllers.append($0) })
        func controller() async throws -> ASAuthorizationController {
            for _ in 0..<400 {
                if !controllers.isEmpty { return controllers.removeFirst() }
                try await Task.sleep(for: .milliseconds(5))
            }
            preconditionFailure("Expected an isolated native authorization controller")
        }
        let firstNative = Task { () -> Bool in
            do { _ = try await native.perform(); return true } catch { return false }
        }
        let firstController = try await controller()
        var secondFinished = false
        let secondNative = Task { () -> Bool in
            defer { secondFinished = true }
            do { _ = try await native.perform(); return true } catch { return false }
        }
        let secondController = try await controller()
        check(await firstNative.value == false, "replacing native authorization cancels its old waiter")
        native.authorizationController(controller: firstController, didCompleteWithError: ASAuthorizationError(.canceled))
        try await Task.sleep(for: .milliseconds(20))
        check(!secondFinished, "old native controller callback cannot finish the newer request")
        native.authorizationController(controller: secondController, didCompleteWithError: ASAuthorizationError(.canceled))
        check(await secondNative.value == false && secondFinished, "current native callback completes its own waiter")

        let cancelledNative = Task { () -> Bool in
            do { _ = try await native.perform(); return true } catch { return false }
        }
        _ = try await controller(); native.cancel()
        check(await cancelledNative.value == false, "local retirement cancels a pending native controller")
        print("Session lifetime: \(checks) checks passed")
    }
}
