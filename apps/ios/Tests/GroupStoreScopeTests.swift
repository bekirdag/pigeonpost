import Foundation
import CryptoKit
import Observation

enum AuthError: Error { case sessionExpired }
enum Fixtures { static let enabled = false }
@MainActor enum AppLife { static let isActive = false }
enum CallHost { static let page = URL(string: "https://group-scope.invalid")! }
enum PeerFace {
    static func displayName(_ value: String) -> String { value }
    static func conversationAddressInput(_ value: String) -> String { value }
}
struct StagedFile { let name: String; let mediaType: String; let data: Data }
@MainActor final class Inbox { var toast: String? }
@MainActor final class HandleStore { init(account: Account) {}; func reset() {} }
@MainActor final class PushService { func unregister() async {} }
@MainActor @Observable final class Session {
    enum Status { case signedIn, signedOut }
    var status = Status.signedIn
    var subject: String? = "first"
    var authorizationId = UUID()
    var accessLease = AuthorizationLease()
    var renewals = 0
    func token() async throws -> String { "synthetic-" + (subject ?? "none") }
    func renew() async throws -> String { renewals += 1; return "synthetic-renewed" }
    func authorize(_ subject: String) { accessLease.invalidate(); accessLease = AuthorizationLease(); authorizationId = UUID(); self.subject = subject; status = .signedIn }
    func forget() { accessLease.invalidate(); accessLease = AuthorizationLease(); authorizationId = UUID(); subject = nil; status = .signedOut }
    func signOut(expectedAuthorization: UUID? = nil) async { if expectedAuthorization == nil || expectedAuthorization == authorizationId { forget() } }
}

private final class GroupScopeTransport: URLProtocol, @unchecked Sendable {
    @MainActor static var pending: [GroupScopeTransport] = []
    @MainActor static var count = 0
    private let lock = NSLock()
    private var finished = false
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        precondition(request.url?.host == "group-scope.invalid")
        Task { @MainActor in Self.count += 1; Self.pending.append(self) }
    }
    override func stopLoading() { lock.lock(); finished = true; lock.unlock() }
    @MainActor func complete(_ body: [String: Any], status: Int = 200) throws {
        lock.lock(); let deliver = !finished; finished = true; lock.unlock()
        guard deliver else { return }
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil,
                                       headerFields: ["content-type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: try JSONSerialization.data(withJSONObject: body))
        client?.urlProtocolDidFinishLoading(self)
    }
}

/// Controlled engine suspension; the real GroupStore, client, account and sealed vault are used.
private actor SuspendedGroupEngine: GroupCrypting {
    let mailbox: String
    nonisolated let client: PostboxClient
    nonisolated let vault: GroupVault
    nonisolated let event: @Sendable (GroupEngineEvent) -> Void
    let device: GroupDeviceIdentity
    var holdStart: Bool
    var starting: CheckedContinuation<Void, Never>?
    var historyReads = 0
    init(mailbox: String, client: PostboxClient, vault: GroupVault, index: Int, holdStart: Bool,
         event: @escaping @Sendable (GroupEngineEvent) -> Void) {
        self.mailbox = mailbox; self.client = client; self.vault = vault; self.event = event; self.holdStart = holdStart
        device = GroupDeviceIdentity(deviceId: "synthetic-device-\(index)", fingerprint: "synthetic-fingerprint")
    }
    func start() async throws -> GroupDeviceIdentity {
        if holdStart { await withCheckedContinuation { starting = $0 } }
        return device
    }
    func resumeStart() { starting?.resume(); starting = nil }
    func isStarting() -> Bool { starting != nil }
    func readCount() -> Int { historyReads }
    func reEnrol() async throws -> GroupDeviceIdentity { device }
    func topUpKeyPackages() async throws -> Int { 0 }
    func processWelcomes() async throws -> [String] { [] }
    func sync(_ group: String) async throws -> Int { 0 }
    func reconcileDeletions(_ group: String) async throws {}
    func processPending(_ group: String, noJitter: Bool) async throws -> Bool { false }
    func selfUpdate(_ group: String) async throws {}
    func createGroup(settings: [String: Any]) async throws -> CreatedGroup { throw CancellationError() }
    func activate(_ group: String) async throws {}
    func send(_ group: String, text: String, files: [OutgoingGroupFile], replyTo: Int?, editOf: Int?, mentions: [String]) async throws -> PostedGroupMessage { throw CancellationError() }
    func retryPending(_ group: String, cmid: String) async throws -> PostedGroupMessage { throw CancellationError() }
    func discardPending(_ group: String, cmid: String) async throws {}
    func openFile(_ group: String, file: GroupFile) async throws -> Data { throw CancellationError() }
    func pruneExpired() async -> Int { 0 }
    func dropSeqs(_ group: String, seqs: [Int]) async {}
    func forget(_ group: String) async {}
    func history(_ group: String) -> [GroupItem] {
        historyReads += 1
        return [GroupItem(kind: .message, seq: 1, sender: mailbox, text: "synthetic-private-history")]
    }
    func inGroup(_ group: String) -> Bool { false }
    func members(_ group: String) -> [GroupDeviceInfo] { [] }
    func safetyNumber(_ group: String) -> String? { "synthetic-safety-number" }
    func identity() -> GroupDeviceIdentity? { device }
}

@main enum GroupStoreScopeTests {
    @MainActor static func main() async throws {
        var checks = 0
        func check(_ value: Bool, _ label: String) { precondition(value, label); checks += 1 }
        func until(_ ready: () -> Bool) async throws {
            for _ in 0..<500 { if ready() { return }; try await Task.sleep(for: .milliseconds(5)) }
            preconditionFailure("Timed out waiting for controlled group request")
        }
        func request(_ path: String) async throws -> GroupScopeTransport {
            try await until { GroupScopeTransport.pending.contains { $0.request.url?.path == path } }
            return GroupScopeTransport.pending.remove(at: GroupScopeTransport.pending.firstIndex { $0.request.url?.path == path }!)
        }
        URLProtocol.registerClass(GroupScopeTransport.self)
        defer { URLProtocol.unregisterClass(GroupScopeTransport.self) }
        let suite = "dev.pigeonpost.group-scope-tests." + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let key = SymmetricKey(size: .bits256)
        let vault = GroupVault(directory: directory, key: { key })
        let session = Session()
        let account = Account(session: session, base: URL(string: "https://group-scope.invalid")!, defaults: defaults)
        func loadAccount(_ addresses: [String]) async throws {
            let load = Task { await account.loadIdentities() }
            try await request("/v1/identities").complete(["identities": addresses.map { ["address": $0] }])
            for _ in addresses { try await request("/v1/whoami").complete([:]) }
            try await request("/v1/capabilities").complete(["features": ["groups", "orgs", "receipts"]])
            await load.value
        }
        try await loadAccount(["/k/first", "/k/other"])
        var engines: [SuspendedGroupEngine] = []
        var holdNextStart = false
        var forgottenKeys = 0
        let store = GroupStore(account: account, vault: vault, cryptoAvailable: { true }, makeEngine: { mailbox, client, vault, event in
            let engine = SuspendedGroupEngine(mailbox: mailbox, client: client, vault: vault, index: engines.count,
                                              holdStart: holdNextStart, event: event)
            holdNextStart = false; engines.append(engine); return engine
        }, forgetKey: { forgottenKeys += 1 })
        account.groups = store
        func finishGroups(_ id: String) async throws {
            try await request("/v1/groups").complete(["groups": [["id": id, "name": id,
                "activation_state": "active", "member_count": 2,
                "my": ["state": "active", "role": "owner", "unread": 2]]]])
        }
        func attach(_ id: String) async throws {
            let work = Task { await store.attach() }
            try await finishGroups(id); await work.value
            try await until { store.histories[id]?.isEmpty == false }
        }
        try await attach("grp_first")
        check(store.enabled && store.groups.first?.id == "grp_first", "group metadata belongs to the bound actor")
        check(store.histories["grp_first"]?.first?.text == "synthetic-private-history", "current actor sees its decrypted history")
        let originalEngine = engines[0]
        let oldRefresh = Task { await store.refresh() }
        let delayed = try await request("/v1/groups")
        account.act(as: account.mailboxes[1]); account.act(as: account.mailboxes[0])
        check(!store.enabled && store.groups.isEmpty && store.histories.isEmpty && store.identity == nil, "A to B to A hides the original activation before lifecycle callbacks")
        check(originalEngine.vault.lease?.isValid == false, "inbox retirement revokes old disk access synchronously")
        let newAttach = Task { await store.attach() }
        let newList = try await request("/v1/groups")
        let coalesced = Task { await store.refresh() }
        try delayed.complete(["error": "not_found"], status: 404); await oldRefresh.value
        let before = GroupScopeTransport.count
        let anotherWaiter = Task { await store.refresh() }
        try await Task.sleep(for: .milliseconds(30))
        check(GroupScopeTransport.count == before, "old refresh cleanup cannot retire a new coalesced refresh")
        try newList.complete(["groups": [["id": "grp_current", "name": "Current", "my": ["state": "keying", "role": "member"]]]])
        await newAttach.value; await coalesced.value; await anotherWaiter.value
        check(store.groups.map(\.id) == ["grp_current"] && engines.count == 2, "returning to the inbox creates a fresh activation")
        let oldReads = await originalEngine.readCount()
        originalEngine.event(.history("grp_first"))
        try await Task.sleep(for: .milliseconds(30))
        check(await originalEngine.readCount() == oldReads && store.histories["grp_first"] == nil, "delayed old engine event is ignored")
        let count = GroupScopeTransport.count
        do { _ = try await originalEngine.client.sendRaw("/v1/obsolete"); preconditionFailure("retired client sent") } catch is CancellationError {}
        check(GroupScopeTransport.count == count && session.renewals == 0, "old engine cannot acquire current credentials")

        let secondEngine = engines[1]
        session.authorize("first")
        check(account.me?.address == "/k/first" && account.features.orgs, "same-person reauthorization preserves the account screen")
        check(store.groups.isEmpty && store.histories.isEmpty && !store.enabled, "same-person reauthorization still retires group plaintext")
        check(secondEngine.vault.lease?.isValid == false, "new grant invalidates every old group disk lease")
        try await attach("grp_reauthorized")
        let record = GroupRecord(deviceId: "synthetic", stateKey: Data(repeating: 1, count: 32))
        try engines.last!.vault.save(record, for: "/k/first")
        check(vault.load("/k/first") == record, "new authorized activation can persist normally")

        // Delayed initialization of another inbox must not replace a newer activation.
        account.act(as: account.mailboxes[1]); holdNextStart = true
        let slowAttach = Task { await store.attach() }
        try await until { engines.count == 4 }
        let slow = engines[3]
        for _ in 0..<500 { if await slow.isStarting() { break }; try await Task.sleep(for: .milliseconds(5)) }
        session.authorize("first")
        try await attach("grp_newer")
        let currentDevice = store.identity
        await slow.resumeStart(); await slowAttach.value
        check(store.enabled && store.identity == currentDevice && store.groups.first?.id == "grp_newer", "late engine startup cannot replace current identity or metadata")

        let oldAuthorization = session.authorizationId
        let logout = Task { await account.signOut() }
        // The bound device is revoked only after all local erasure has finished.
        try await until { GroupScopeTransport.pending.contains { $0.request.httpMethod == "DELETE" } }
        let revokeIndex = GroupScopeTransport.pending.firstIndex { $0.request.httpMethod == "DELETE" }!
        let revoke = GroupScopeTransport.pending.remove(at: revokeIndex)
        check(forgottenKeys == 1 && !FileManager.default.fileExists(atPath: directory.path), "sign-out erases the vault and key before waiting on revocation")
        check(!store.enabled && store.histories.isEmpty && account.me == nil, "signing-out actor is unavailable during revocation")
        session.authorize("replacement")
        try await loadAccount(["/k/replacement"])
        try await attach("grp_replacement")
        try engines.last!.vault.save(record, for: "/k/replacement")
        try revoke.complete([:]); await logout.value
        check(account.me?.address == "/k/replacement" && store.enabled, "late sign-out preserves the replacement account and engine")
        check(forgottenKeys == 1 && vault.load("/k/replacement") == record, "late sign-out cannot erase newly written group state")
        await store.signOut(expectedAuthorization: oldAuthorization)
        check(forgottenKeys == 1 && store.enabled, "already-retired sign-out cannot begin cleanup after another authorization")
        account.features.reset()
        check(!store.enabled && store.groups.isEmpty && store.histories.isEmpty, "feature-off hides group plaintext before teardown")
        await store.attach()
        check(store.scopeId == nil, "feature-off retires the activation")
        for remaining in GroupScopeTransport.pending { try remaining.complete([:]) }
        print("Group store scope: \(checks) checks passed")
    }
}
