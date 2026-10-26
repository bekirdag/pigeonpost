import Foundation
import Observation

enum AuthError: Error { case sessionExpired }
enum Fixtures { static let enabled = false }
@MainActor @Observable final class Session {
    enum Status { case signedIn, signedOut }
    var status = Status.signedIn
    var subject: String? = "first"
    var authorizationId = UUID()
    var accessLease = AuthorizationLease()
    var renewals = 0
    var waitForToken = false
    var waitingToken: CheckedContinuation<String, Never>?
    func token() async throws -> String {
        if waitForToken { return await withCheckedContinuation { waitingToken = $0 } }
        return "synthetic-" + (subject ?? "none")
    }
    func renew() async throws -> String { renewals += 1; return "synthetic-renewed" }
    func authorize(_ subject: String) { accessLease.invalidate(); accessLease = AuthorizationLease(); self.subject = subject; authorizationId = UUID(); status = .signedIn }
    func forget() { accessLease.invalidate(); accessLease = AuthorizationLease(); status = .signedOut; subject = nil; authorizationId = UUID() }
    func signOut(expectedAuthorization: UUID? = nil) async { if expectedAuthorization == nil || expectedAuthorization == authorizationId { forget() } }
}
@MainActor final class HandleStore {
    init(account: Account) {}
    func reset() {}
}
@MainActor final class PushService {
    var pending: CheckedContinuation<Void, Never>?
    func unregister() async { await withCheckedContinuation { pending = $0 } }
}
@MainActor final class GroupStore {
    var signOuts = 0
    func signOut(expectedAuthorization: UUID? = nil) async { signOuts += 1 }
}

private final class ScopeTransport: URLProtocol, @unchecked Sendable {
    @MainActor static var pending: [ScopeTransport] = []
    @MainActor static var count = 0
    private let lock = NSLock()
    private var finished = false
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        precondition(request.url?.host == "account-scope.invalid")
        Task { @MainActor in Self.count += 1; Self.pending.append(self) }
    }
    override func stopLoading() { lock.lock(); finished = true; lock.unlock() }
    @MainActor func complete(_ status: Int = 200, _ body: [String: Any] = [:]) throws {
        lock.lock(); let deliver = !finished; finished = true; lock.unlock()
        guard deliver else { return }
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil,
                                       headerFields: ["content-type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: try JSONSerialization.data(withJSONObject: body))
        client?.urlProtocolDidFinishLoading(self)
    }
}

@main enum AccountScopeTests {
    @MainActor static func main() async throws {
        var checks = 0
        func check(_ value: Bool, _ label: String) { precondition(value, label); checks += 1 }
        func until(_ ready: () -> Bool) async throws {
            for _ in 0..<500 { if ready() { return }; try await Task.sleep(for: .milliseconds(5)) }
            preconditionFailure("Timed out waiting for controlled operation")
        }
        func request(_ path: String) async throws -> ScopeTransport {
            try await until { ScopeTransport.pending.contains { $0.request.url?.path == path } }
            return ScopeTransport.pending.remove(at: ScopeTransport.pending.firstIndex { $0.request.url?.path == path }!)
        }
        URLProtocol.registerClass(ScopeTransport.self)
        defer { URLProtocol.unregisterClass(ScopeTransport.self) }
        let suite = "dev.pigeonpost.scope-tests." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let session = Session()
        let account = Account(session: session, base: URL(string: "https://account-scope.invalid")!, defaults: defaults)
        func finishLoad(_ name: String, addresses: [String]) async throws {
            try await request("/v1/identities").complete(200, ["identities": addresses.map { ["address": $0] }])
            for _ in addresses {
                let who = try await request("/v1/whoami")
                let address = URLComponents(url: who.request.url!, resolvingAgainstBaseURL: false)!.queryItems!.first!.value!
                try who.complete(200, ["address": address, "handle": "/" + name + "/" + address.dropFirst(3)])
            }
            try await request("/v1/capabilities").complete(200, ["features": ["groups", "orgs", "calendar"]])
        }
        let original = Task { await account.loadIdentities() }
        try await finishLoad("first", addresses: ["/k/main", "/k/other"])
        await original.value
        check(account.mailboxes.count == 2 && account.me?.address == "/k/main", "real identities and whoami populate the account")
        check(account.features.groups && account.features.orgs, "capabilities belong to the loaded person")
        let beforeSwitch = account.actingGeneration
        account.act(as: Mailbox(address: "/k/foreign", handle: "/forged/main", label: nil))
        check(account.me?.address == "/k/main" && account.actingGeneration == beforeSwitch, "unknown inbox cannot become the actor")
        account.act(as: Mailbox(address: "/k/other", handle: "/forged/main", label: "forged"))
        check(account.me?.handle == "/first/other" && account.actingGeneration != beforeSwitch, "actor uses canonical owned metadata")
        let otherGeneration = account.actingGeneration
        account.act(as: account.mailboxes.first!)
        check(account.actingGeneration != beforeSwitch && account.actingGeneration != otherGeneration, "A to B to A retires the original actor generation")

        let oldClient = account.scopedClient()
        session.authorize("first")
        check(account.me?.address == "/k/main" && account.features.orgs, "same-person strong authorization keeps its screen and capabilities")
        let count = ScopeTransport.count
        do { _ = try await oldClient.sendRaw("/v1/old"); preconditionFailure("old client used a new grant") } catch is CancellationError {}
        check(ScopeTransport.count == count && session.renewals == 0, "captured client cannot use replacement credentials")

        let late = Task { await account.loadIdentities() }
        let obsolete = try await request("/v1/identities")
        session.authorize("second")
        check(account.me == nil && account.mailboxes.isEmpty && !account.features.orgs, "new person immediately hides old identities and capabilities")
        let replacement = Task { await account.loadIdentities() }
        try await finishLoad("second", addresses: ["/k/second"])
        await replacement.value
        try obsolete.complete(401, ["error": "unauthorized"]); await late.value
        check(account.me?.address == "/k/second" && account.load == .ready, "late old load cannot overwrite the new person")
        check(session.renewals == 0 && account.features.orgs, "late old 401 neither renews nor clears new capabilities")

        let frozen = account.scopedClient()
        let pendingRead = Task { () -> Bool in
            do { _ = try await frozen.sendRaw("/v1/delayed"); return true } catch { return false }
        }
        let read = try await request("/v1/delayed")
        session.authorize("second")
        try read.complete(200, ["secret": "synthetic-old-response"])
        check(await pendingRead.value == false, "late success cannot return data after same-person reauthorization")

        session.waitForToken = true
        let tokenClient = account.scopedClient()
        let tokenRead = Task { () -> Bool in
            do { _ = try await tokenClient.sendRaw("/v1/token-wait"); return true } catch { return false }
        }
        try await until { session.waitingToken != nil }
        let beforeToken = ScopeTransport.count
        session.authorize("third")
        session.waitingToken?.resume(returning: "synthetic-new-token"); session.waitingToken = nil; session.waitForToken = false
        check(await tokenRead.value == false && ScopeTransport.count == beforeToken, "late token cannot send an old request under a new person")

        let creating = Task { await account.createFirstMailbox() }
        let oldCreate = try await request("/v1/identities")
        session.authorize("fourth")
        let newerCreate = Task { await account.createFirstMailbox() }
        let newCreate = try await request("/v1/identities")
        try oldCreate.complete(200, ["address": "/k/third"]); await creating.value
        check(account.creating, "old create cleanup cannot clear a new create's busy flag")
        try newCreate.complete(200, ["address": "/k/fourth"])
        try await finishLoad("fourth", addresses: ["/k/fourth"]); await newerCreate.value
        check(account.me?.address == "/k/fourth" && !account.creating, "only current creation reloads and completes")

        let push = PushService(), groups = GroupStore()
        account.push = push; account.groups = groups
        let logout = Task { await account.signOut() }
        try await until { push.pending != nil }
        session.authorize("fifth")
        let final = Task { await account.loadIdentities() }
        try await finishLoad("fifth", addresses: ["/k/fifth"]); await final.value
        push.pending?.resume(); push.pending = nil; await logout.value
        check(session.subject == "fifth" && account.me?.address == "/k/fifth", "late old sign-out cannot erase or sign out a new login")
        check(groups.signOuts == 0, "old sign-out cannot begin group cleanup under the replacement grant")
        let cancelledLoad = Task { await account.loadIdentities() }
        let cancelledRead = try await request("/v1/identities")
        cancelledLoad.cancel(); await cancelledLoad.value
        try cancelledRead.complete(200, ["identities": []])
        check(account.load != .loading, "cancellation retires its own loading state")
        let retryLoad = Task { await account.loadIdentities() }
        try await finishLoad("fifth", addresses: ["/k/fifth"]); await retryLoad.value
        check(account.load == .ready && account.me?.address == "/k/fifth", "same authorization can retry after cancelled loading")
        account.sessionExpired()
        check(account.me == nil && account.mailboxes.isEmpty && !account.features.loaded, "expiry erases cached identities and capabilities")
        check(session.status == .signedOut, "expiry retires the session")
        check(ScopeTransport.pending.isEmpty, "controlled requests are fully resolved")
        print("Account scope: \(checks) checks passed")
    }
}
