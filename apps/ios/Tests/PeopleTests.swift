import Foundation

enum AuthError: Error { case sessionExpired }
@MainActor final class Session {
    func token() async throws -> String { "synthetic-token" }
    func renew() async throws -> String { "synthetic-renewal" }
}

private final class PeopleTransport: URLProtocol, @unchecked Sendable {
    @MainActor static var reply = Data()
    @MainActor static var requests: [URLRequest] = []
    @MainActor static var bodies: [Data] = []
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "people.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() { }
    override func startLoading() {
        Task { @MainActor in
            Self.requests.append(request)
            var data = request.httpBody ?? Data()
            if data.isEmpty, let stream = request.httpBodyStream {
                stream.open(); defer { stream.close() }
                var buffer = [UInt8](repeating: 0, count: 2048)
                while stream.hasBytesAvailable {
                    let count = stream.read(&buffer, maxLength: buffer.count)
                    if count <= 0 { break }; data.append(contentsOf: buffer.prefix(count))
                }
            }
            Self.bodies.append(data)
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: ["content-type": "application/json"])!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Self.reply)
            client?.urlProtocolDidFinishLoading(self)
        }
    }
}

@main enum PeopleTests {
    @MainActor static func main() async throws {
        var count = 0
        func check(_ value: Bool, _ message: String) { precondition(value, message); count += 1 }
        func scope(_ actor: UUID = UUID(), authorization: UUID = UUID(), mailbox: String = "/k/self", features: Set<String> = ["phone_numbers", "contact_discovery", "sms_invites", "display_names"]) -> PeopleScope {
            .init(subject: "fixture-person", authorization: authorization, actor: actor, mailbox: mailbox, features: features)
        }
        let hash = try PeopleInput.hash("+442079460123")
        check(try hash.count == 32 && hash == PeopleInput.hash("+442079460123"), "stable 128-bit phone hash")
        for (phone, expected) in [("+15555550100", "7c0dc19d3eae376a630cee358d0f8912"), ("+447700900123", "5c4fd5bb0f19345b15863f865cb76cee"), ("+61491570156", "511ebc743c8a1d1ea8be5541f2fee95d")] {
            check(try PeopleInput.hash(phone) == expected, "published cross-client hash vector")
        }
        check(hash != (try PeopleInput.hash("+442079460124")), "distinct numbers")
        for value in ["12345", "+12025550123\n", "+0012345678", "+1234567", "+1234567890123456", "+90555\n1234567"] { check(!PeopleInput.phone(value), "reject invalid E164") }
        for value in ["/marina", "@marina", "admin", "Pigeonpost Team", "Mar\u{202e}ina", "Mar\u{200d}ina", "\u{2800}", "...", "a\nname"] {
            check((try? PeopleInput.name(value)) == nil, "reject reserved, address-like and invisible names")
        }
        check(try PeopleInput.name("  O\u{308}zgu\u{308}r  Dag  ") == "Özgür Dag", "NFC and spacing")
        check(try PeopleInput.name("Support", alias: true) == "Support", "private names are not public badges")

        var current: PeopleScope? = scope()
        let api = PeopleFixtureAPI(); let storage = PeopleFixtureStorage()
        let store = PeopleStore(scope: { current }, api: { api }, storage: storage)
        store.synchronize(); let original = current!
        store.alias(original, peer: "/k/peer", name: "Marina")
        check(store.displayName("/k/peer") == "Marina", "private alias wins")
        await store.refreshProfiles(original, addresses: ["/k/peer", "/k/other", "/paid"])
        check(store.displayName("/k/peer") == "Marina" && store.displayName("/k/other") == "~Example Person" && store.displayName("/paid") == "~Example Person", "local alias and self-name precedence")
        check(store.displayName("/paid", legacyName: "My inbox") == "My inbox", "existing owned-inbox and legacy contact names remain stable")
        store.dismissPrompt(original)
        check(store.state.promptSeen, "skippable name prompt persists locally")
        current = scope(mailbox: "/k/other")
        check(store.state.aliases.isEmpty && !store.state.promptSeen, "other mailbox hides names before lifecycle runs")
        store.synchronize(); store.alias(original, peer: "/k/peer", name: "Wrong")
        check(store.state.aliases.isEmpty, "old dialog cannot write to new inbox")
        current = scope(authorization: original.authorization, mailbox: original.mailbox)
        store.synchronize()
        check(store.state.promptSeen && store.localName("/k/peer") == "Marina", "local state belongs to account and inbox")
        await store.saveName(original, name: "Stale", visibility: "known")
        check(api.name == nil, "mailbox round trip invalidates captured action")
        let beforeLogin = current!
        current = scope(beforeLogin.actor, mailbox: beforeLogin.mailbox)
        await store.sendCode(beforeLogin, phone: "+442079460123")
        check(store.state.challenge == nil, "same-address new login rejects old request")
        store.synchronize(); let active = current!
        api.beforeName = { current = scope() }
        await store.loadName(active)
        check(!store.state.nameLoaded, "stale response hidden after login replacement")
        api.beforeName = nil; store.synchronize()

        let contacts = try (0..<250).map { try PhoneContact(phone: "+1202555" + String(format: "%04d", $0), name: "Local \($0)") }
        let discovery = current!
        store.importContacts(discovery, contacts: contacts)
        await store.discover(discovery)
        check(api.batches.map(\.count) == [200], "first batch respects new-user daily cap")
        check(store.state.checked.count == 200 && store.state.contacts.count == 250 && store.state.remaining == 0, "retain partial results and unchecked numbers")
        api.numbers = [.init(id: "phone_fixture", identity: discovery.mailbox, countryCode: "1", masked: "+1 •••• 0000", discoverable: false, verifiedAt: 1)]
        await store.loadPhones(discovery)
        store.reviewInvite(discovery, contact: contacts[249])
        check(store.state.invitation == nil, "unchecked contact cannot be invited")
        store.reviewInvite(discovery, contact: contacts[0])
        check(api.invitations.isEmpty && store.state.invitation != nil, "review does not send SMS")
        let review = store.state.invitation!
        store.cancelInvite(); check(store.prepareInviteConfirmation(review) == nil, "cancelled review cannot become a confirmation"); await store.confirmInvite(review)
        check(api.invitations.isEmpty, "cancelled review cannot send")
        store.reviewInvite(discovery, contact: contacts[0]); let confirmed = store.prepareInviteConfirmation(store.state.invitation!)!
        store.cancelInvite() // SwiftUI automatically clears the dialog binding before the async task runs.
        await store.confirmInvite(confirmed); await store.confirmInvite(confirmed)
        check(api.invitations == [contacts[0].phone], "one confirmation is one attempt")
        store.reviewInvite(discovery, contact: contacts[1]); let retired = store.prepareInviteConfirmation(store.state.invitation!)!
        current = scope(); await store.confirmInvite(retired)
        check(api.invitations.count == 1 && store.state.contacts.isEmpty, "old invitation cannot follow a login change")
        store.synchronize(); store.importContacts(current, contacts: contacts)
        api.budget = 400; api.beforeDiscovery = { store.clearContacts(current) }
        await store.discover(current)
        check(store.state.contacts.isEmpty && store.state.matches.isEmpty && store.state.checked.isEmpty, "permission withdrawal discards in-flight discovery")
        api.beforeDiscovery = nil
        let enabled = current!
        current = scope(enabled.actor, authorization: enabled.authorization, features: [])
        check(store.state.profiles.isEmpty && store.displayName("/k/peer") == nil, "feature withdrawal hides cached state immediately")
        store.synchronize(); store.clearLocalAccount()
        check(storage.aliases(subject: original.subject, mailbox: original.mailbox).isEmpty, "explicit signout clears account aliases")

        // A page opened during background name loading must not silently drop its initial read.
        var readScope: PeopleScope? = scope()
        let readAPI = PeopleFixtureAPI()
        readAPI.numbers = [.init(id: "phone_wait", identity: readScope!.mailbox, countryCode: "1", masked: "+1 •••• 0100", discoverable: false, verifiedAt: 1)]
        let reads = PeopleStore(scope: { readScope }, api: { readAPI }, storage: PeopleFixtureStorage())
        var nameGate: CheckedContinuation<Void, Never>?
        readAPI.beforeName = { await withCheckedContinuation { nameGate = $0 } }
        for scenario in 0..<3 {
            readScope = scope(); reads.synchronize(); nameGate = nil
            let captured = readScope!
            let background = Task { await reads.loadName(captured) }
            while nameGate == nil { await Task.yield() }
            let page = Task { await reads.loadPhones(captured) }
            try await Task.sleep(for: .milliseconds(40))
            if scenario == 1 { page.cancel() }
            if scenario == 2 { readScope = scope() }
            nameGate?.resume(); await background.value; await page.value
            if scenario == 0 { check(readAPI.phoneReads == 1 && reads.state.phones.count == 1, "page read survives overlapping background load") }
            else { check(readAPI.phoneReads == 1, "cancelled or retired page read cannot request another account") }
        }

        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("pp-people-test-" + UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let disk = PeopleFiles(root: directory)
        try disk.setAlias(subject: "first", mailbox: "/k/self", peer: "/k/peer", name: "Marina")
        try disk.markPromptSeen(subject: "first", mailbox: "/k/self")
        let reopened = PeopleFiles(root: directory)
        check(reopened.aliases(subject: "first", mailbox: "/k/self")["/k/peer"] == "Marina" && reopened.promptSeen(subject: "first", mailbox: "/k/self"), "local state survives restart")
        check(reopened.aliases(subject: "other", mailbox: "/k/self").isEmpty && !reopened.promptSeen(subject: "first", mailbox: "/k/other"), "disk scope excludes other accounts and inboxes")
        let folder = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.isExcludedFromBackupKey])[0]
        check(try folder.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup == true, "aliases excluded from device backups")
        try reopened.clear(subject: "first")
        check(reopened.aliases(subject: "first", mailbox: "/k/self").isEmpty, "explicit signout removes persisted aliases")

        URLProtocol.registerClass(PeopleTransport.self)
        defer { URLProtocol.unregisterClass(PeopleTransport.self) }
        let session = Session(); let wire = PeopleClient(client: PostboxClient(base: URL(string: "https://people.invalid")!, tokens: session))
        func reply(_ body: [String: Any]) throws { PeopleTransport.reply = try JSONSerialization.data(withJSONObject: body) }
        try reply(["matches": [], "checked": 1, "new": 1, "remaining_today": 199])
        _ = try await wire.discover("/k/self", hashes: [hash])
        let payload = try JSONSerialization.jsonObject(with: PeopleTransport.bodies.last!) as! [String: Any]
        check(Set(payload.keys) == ["hashes"] && payload["hashes"] as? [String] == [hash], "discovery contains no names or raw phone numbers")
        check(PeopleTransport.requests.last?.value(forHTTPHeaderField: "x-pigeonpost-identity") == "/k/self", "acting mailbox on wire")
        try reply(["matches": [["hash": String(repeating: "f", count: 32), "address": "/k/other"]], "checked": 1, "new": 1, "remaining_today": 199])
        do { _ = try await wire.discover("/k/self", hashes: [hash]); check(false, "unsolicited match accepted") } catch { check(true, "unsolicited match refused") }
        try reply(["name": ["display_name": "Marina", "visibility": "known", "updated_at": 1]])
        check(try await wire.ownName("/k/self")?.displayName == "Marina", "snake_case decoded once")
        try reply(["status": "sent", "invite_id": "fixture_invite"])
        _ = try await wire.invite("/k/self", phone: "+442079460123")
        let invite = try JSONSerialization.jsonObject(with: PeopleTransport.bodies.last!) as! [String: Any]
        check(invite["confirm"] as? Bool == true && invite["phone"] as? String == "+442079460123", "only confirmed invite request carries destination")
        print("People: \(count) checks passed")
    }
}
