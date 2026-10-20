//  Groups as the screens see them: the acting mailbox's groups, what this device has read of each,
//  and every action a person can take on one.
//
//  The orchestration of site-inbox/groups.js (`onMailbox`, `refresh`, `syncGroup`, the info sheet's
//  actions) over `GroupEngine`. The app does not hold the account's event stream open, so the
//  wake-ups the web takes from typed events come from a cadence here instead — the open group every
//  few seconds, the list and the device's welcomes every quarter minute — and from push, which
//  `wake(_:)` turns into an immediate sync of the group it was about.

import Foundation
import Observation

@MainActor
@Observable
final class GroupStore {
    enum Status: Equatable {
        /// The postbox has not switched groups on, or this build has no group library.
        case off
        case starting
        case ready
        /// Groups are on but this device could not set up its encryption. Nothing is offered then:
        /// there is no weaker fallback.
        case failed(String)
    }

    private(set) var status: Status = .off
    private(set) var groups: [GroupSummary] = []
    /// What this device has decrypted of each group, newest last. Copied from the engine after every
    /// change so the screens never await anything to draw.
    private(set) var histories: [String: [GroupItem]] = [:]
    private(set) var members: [String: [GroupMember]] = [:]
    private(set) var devices: [String: [MailboxDevices.Device]] = [:]
    private(set) var memberDevices: [String: [GroupDeviceInfo]] = [:]
    private(set) var safetyNumbers: [String: String] = [:]
    private(set) var inGroup: Set<String> = []
    private(set) var identity: GroupDeviceIdentity?
    private(set) var calls: [String: GroupCallState] = [:]
    /// Address → handle, learned from member lists, so names read as names.
    private(set) var handles: [String: String] = [:]
    /// The group on screen; its history is polled faster and its arrivals are not announced.
    var open: String?
    /// A join link waiting for the person to confirm (from a universal link or a pasted URL).
    var pendingJoin: JoinLink?
    var invitationsOpen = false

    struct JoinLink: Equatable, Identifiable {
        let groupId: String
        let token: String?
        var id: String { groupId + (token ?? "") }
    }

    @ObservationIgnored let account: Account
    @ObservationIgnored weak var inbox: Inbox?
    @ObservationIgnored private var engines: [String: any GroupCrypting] = [:]
    @ObservationIgnored private var mailbox: String?
    @ObservationIgnored private var generation = UUID()
    @ObservationIgnored private var syncing: Set<String> = []
    @ObservationIgnored private var lastTopUp: Date = .distantPast
    @ObservationIgnored let vault: GroupVault

    init(account: Account, vault: GroupVault = .standard()) {
        self.account = account
        self.vault = vault
    }

    private var client: PostboxClient { account.client }
    private var me: String? { account.me?.address }
    private var caller: GroupCaller? { me.map { GroupCaller(mailbox: $0, device: identity?.deviceId) } }
    private var engine: (any GroupCrypting)? { mailbox.flatMap { engines[$0] } }
    var enabled: Bool { status == .ready }

    // ---- the lifecycle ------------------------------------------------------------------------------

    /// Called whenever the mailbox or the capabilities may have changed, and on every return to the
    /// foreground. Cheap when nothing did.
    func attach() async {
        #if DEBUG
        if Fixtures.enabled {
            if account.features.groups { installFixtures() } else { status = .off }
            return
        }
        #endif
        guard let me, account.features.groups, GroupEngines.available else {
            teardown()
            return
        }
        if mailbox != me {
            teardown()
            mailbox = me
            generation = UUID()
        }
        let generation = self.generation
        if engines[me] == nil {
            engines[me] = GroupEngines.make(mailbox: me, client: client, vault: vault) { [weak self] event in
                Task { @MainActor in self?.handle(event, for: me) }
            }
        }
        guard let engine = engines[me] else { return }
        if status != .ready { status = .starting }
        do {
            let identity = try await engine.start()
            guard generation == self.generation else { return }
            self.identity = identity
            status = .ready
        } catch {
            guard generation == self.generation else { return }
            status = .failed(GroupText.explain(error))
            return
        }
        if Date().timeIntervalSince(lastTopUp) > 600 {
            lastTopUp = Date()
            Task { _ = try? await engine.topUpKeyPackages() }
        }
        _ = try? await engine.processWelcomes()
        await refresh()
    }

    /// The cadence: run for as long as the caller's task lives (while the app is in front of somebody).
    func live() async {
        var tick = 0
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            guard !Task.isCancelled, enabled, !Fixtures.enabled else { continue }
            tick += 1
            if let open { await syncGroup(open) }
            if tick % 5 == 0 {
                if let open { await pollCall(open) }
                _ = try? await engine?.processWelcomes()
                await refresh()
                if (await engine?.pruneExpired() ?? 0) > 0 { await reloadHistories() }
            }
        }
    }

    /// A push or a tap about a group: sync it now rather than at the next tick.
    func wake(_ group: String) {
        guard enabled else { return }
        Task {
            if groups.contains(where: { $0.id == group }) { await syncGroup(group, pending: true) } else { await refresh() }
        }
    }

    private func teardown() {
        status = .off
        groups = []
        histories = [:]
        members = [:]
        devices = [:]
        memberDevices = [:]
        safetyNumbers = [:]
        inGroup = []
        identity = nil
        calls = [:]
        open = nil
        mailbox = nil
        generation = UUID()
    }

    func reset() {
        teardown()
        engines = [:]
    }

    /// Signing out: each mailbox's device revokes itself (so co-members stop counting it), then this
    /// device's group state and its Keychain key are erased.
    func signOut() async {
        for (mailbox, engine) in engines {
            guard let id = await engine.identity()?.deviceId else { continue }
            try? await client.revokeGroupDevice(GroupCaller(mailbox: mailbox, device: id), deviceId: id)
        }
        reset()
        vault.removeAll()
        GroupKeychain.forget()
    }

    private func handle(_ event: GroupEngineEvent, for mailbox: String) {
        guard mailbox == self.mailbox else { return }
        switch event {
        case let .history(group):
            Task { await reloadHistory(group) }
        case let .removed(group), let .resync(group):
            Task {
                await reloadHistory(group)
                await refresh()
            }
        }
    }

    private func reloadHistory(_ group: String) async {
        guard let engine else { return }
        histories[group] = await engine.history(group)
        if await engine.inGroup(group) { inGroup.insert(group) } else { inGroup.remove(group) }
        safetyNumbers[group] = await engine.safetyNumber(group)
        memberDevices[group] = await engine.members(group)
    }

    private func reloadHistories() async {
        for group in groups { await reloadHistory(group.id) }
    }

    // ---- the list -----------------------------------------------------------------------------------

    private var refreshing: Task<Void, Never>?

    func refresh() async {
        if let refreshing { return await refreshing.value }
        let job = Task { await refreshNow() }
        refreshing = job
        await job.value
        refreshing = nil
    }

    private func refreshNow() async {
        guard enabled, let caller, let engine else { return }
        let generation = self.generation
        let listed: [GroupSummary]
        do {
            listed = try await client.groups(caller)
        } catch let error as APIError where error.status == 404 {
            groups = []
            return
        } catch {
            return
        }
        guard generation == self.generation else { return }
        groups = listed
        for group in listed { await reloadHistory(group.id) }
        for group in listed where group.deletedAt == nil {
            if group.isKeying, group.activationState == "provisioning", group.createdBy == caller.mailbox, group.isOwner {
                try? await engine.activate(group.id)
                await reloadHistory(group.id)
            } else if group.isActive {
                Task { await syncGroup(group.id, pending: true) }
            }
        }
    }

    func syncGroup(_ group: String, pending: Bool = false) async {
        guard let engine, !syncing.contains(group) else { return }
        syncing.insert(group)
        defer { syncing.remove(group) }
        if await engine.inGroup(group) {
            do {
                try await engine.sync(group)
            } catch {
                await recover(from: error)
            }
            if pending { _ = try? await engine.processPending(group, noJitter: false) }
        }
        await reloadHistory(group)
        if group == open { markRead(group) }
    }

    /// The postbox no longer knows this device — revoked from another device, or a database reset.
    private func recover(from error: Error) async {
        guard let api = error as? APIError, ["unknown_device", "device_revoked"].contains(api.code ?? ""),
              let engine else { return }
        if let identity = try? await engine.reEnrol() {
            self.identity = identity
            _ = try? await engine.topUpKeyPackages()
        }
    }

    var invitations: [GroupSummary] {
        groups.filter { $0.isInvited && $0.deletedAt == nil }
    }

    /// The groups to list, newest activity first, narrowed by the conversation search.
    func listed(filter: String) -> [GroupSummary] {
        let needle = filter.lowercased()
        return groups
            .filter { !$0.isInvited && $0.deletedAt == nil && (needle.isEmpty || $0.name.lowercased().contains(needle)) }
            .sorted { ($0.lastAt ?? 0) > ($1.lastAt ?? 0) }
    }

    var unreadCount: Int {
        groups.filter { $0.isActive && !$0.muted && $0.deletedAt == nil }.reduce(0) { $0 + $1.unread }
    }

    func group(_ id: String) -> GroupSummary? { groups.first { $0.id == id } }

    func preview(_ group: GroupSummary) -> String {
        if group.isKeying { return "Waiting for a member's device to add you…" }
        if let me, let text = GroupText.preview(histories[group.id] ?? [], me: me, names: name) { return text }
        return group.activationState == "provisioning" ? "Setting up encryption…" : "No messages yet"
    }

    /// What to call a member: "You", their handle's name, or a shortened key address.
    func name(_ address: String) -> String {
        if address.isEmpty { return "Someone" }
        if address == me { return "You" }
        return PeerFace.displayName(handles[address] ?? address)
    }

    /// The key that faces (avatars, blocking) are looked up by.
    func key(_ address: String) -> String { handles[address] ?? address }

    func lines(_ group: String) -> [GroupLine] {
        guard let me else { return [] }
        return GroupText.lines(histories[group] ?? [], me: me, myDevice: identity?.deviceId,
                               now: Int(Date().timeIntervalSince1970), names: name, digits: Self.digits)
    }

    /// Fingerprint digits from hex, through the library when it is there.
    static func digits(_ hex: String) -> String {
        #if GROUPCRYPTO
        var bytes = Data()
        var index = hex.startIndex
        while index < hex.endIndex, let next = hex.index(index, offsetBy: 2, limitedBy: hex.endIndex) {
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return hex }
            bytes.append(byte)
            index = next
        }
        return (try? fingerprintDigits(fingerprint: bytes)) ?? hex
        #else
        return hex
        #endif
    }

    // ---- opening and reading ------------------------------------------------------------------------

    func show(_ group: String) {
        open = group
        Task {
            await loadMembers(group)
            await syncGroup(group, pending: true)
            await pollCall(group)
        }
    }

    func close(_ group: String) {
        if open == group { open = nil }
    }

    func markRead(_ id: String) {
        guard AppLife.isActive, let caller, let index = groups.firstIndex(where: { $0.id == id }) else { return }
        let seen = max((histories[id] ?? []).compactMap(\.seq).max() ?? 0, groups[index].lastSeq ?? 0)
        guard seen > (groups[index].my?.readCursor ?? 0) else { return }
        groups[index].my?.readCursor = seen
        groups[index].my?.unread = 0
        guard !Fixtures.enabled else { return }
        let client = self.client
        Task { try? await client.markGroupRead(caller, id: id, seq: seen) }
    }

    func loadMembers(_ group: String) async {
        #if DEBUG
        if Fixtures.enabled { return }
        #endif
        guard let caller, let list = try? await client.groupMembers(caller, id: group) else { return }
        for member in list { if let handle = member.handle { handles[member.address] = handle } }
        members[group] = list
    }

    func loadDevices(_ mailbox: String) async {
        #if DEBUG
        if Fixtures.enabled { return }
        #endif
        guard let caller, let list = try? await client.groupDevices(caller, of: mailbox) else { return }
        devices[mailbox] = list
    }

    // ---- sending ------------------------------------------------------------------------------------

    /// Returns whether it went. On failure the words stay with the caller to put back.
    @discardableResult
    func send(_ group: String, text: String, files: [StagedFile]) async -> Bool {
        #if DEBUG
        if Fixtures.enabled {
            installFixtureMessage(group, text: text)
            return true
        }
        #endif
        guard let engine else { return false }
        let outgoing = files.map { OutgoingGroupFile(name: $0.name, mime: $0.mediaType, data: $0.data) }
        do {
            try await engine.send(group, text: text, files: outgoing, replyTo: nil, editOf: nil, mentions: [])
            await reloadHistory(group)
            return true
        } catch {
            await reloadHistory(group)
            say("Not sent: " + GroupText.explain(error))
            return false
        }
    }

    func deleteMessage(_ group: String, seq: Int) async {
        guard let caller else { return }
        do {
            try await client.deleteGroupMessage(caller, id: group, seq: seq)
            await syncGroup(group)
        } catch {
            say(GroupText.explain(error))
        }
    }

    func openFile(_ group: String, file: GroupFile) async -> Data? {
        guard let engine else { return nil }
        do { return try await engine.openFile(group, file: file) } catch {
            say("Could not open the file: " + GroupText.explain(error))
            return nil
        }
    }

    // ---- creating, joining, leaving ----------------------------------------------------------------

    /// Create a group and open it. Returns its id, or nil with the reason said.
    func create(name: String, description: String?, invite: [String], joinPolicy: String, postPolicy: String,
                timer: Int?) async -> Result<String, Error> {
        guard let engine else { return .failure(APIError(status: 0, code: "feature_disabled", detail: nil)) }
        var settings: [String: Any] = [
            "name": name, "join_policy": joinPolicy, "post_policy": postPolicy, "visibility": "unlisted", "invite": invite,
        ]
        if let description, !description.isEmpty { settings["description"] = description }
        if let timer, account.features.disappearing { settings["ephemeral_secs"] = timer }
        do {
            let created = try await engine.createGroup(settings: settings)
            let refused = (created.invitations ?? []).filter { $0.status != "invited" }
            if !refused.isEmpty {
                say("\(refused.count) \(refused.count == 1 ? "person" : "people") could not be invited.")
            }
            await refresh()
            return .success(created.group.id)
        } catch {
            return .failure(error)
        }
    }

    func answer(_ group: GroupSummary, accept: Bool) async {
        guard let caller else { return }
        do {
            try await client.answerInvitation(caller, id: group.id, accept: accept)
            say(accept ? "Joined \(group.name). A member's device will add you in a moment." : "Invitation declined.")
        } catch {
            say(GroupText.explain(error))
        }
        await refresh()
    }

    func resolve(_ link: JoinLink) async throws -> GroupPreview {
        guard let caller else { throw APIError(status: 0, code: "not_started", detail: nil) }
        return try await client.resolveGroup(caller, id: link.groupId, linkToken: link.token)
    }

    func join(_ link: JoinLink) async throws {
        guard let caller else { return }
        try await client.joinGroup(caller, id: link.groupId, linkToken: link.token)
        say("Joined. A member's device will add you to the encryption in a moment.")
        await refresh()
    }

    func leave(_ group: GroupSummary) async -> Bool {
        guard let caller else { return false }
        do {
            try await client.leaveGroup(caller, id: group.id)
            await engine?.forget(group.id)
            close(group.id)
            await refresh()
            return true
        } catch {
            say(GroupText.explain(error))
            return false
        }
    }

    func delete(_ group: GroupSummary) async -> Bool {
        guard let caller else { return false }
        do {
            try await client.deleteGroup(caller, id: group.id)
            await engine?.forget(group.id)
            close(group.id)
            await refresh()
            return true
        } catch {
            say(GroupText.explain(error))
            return false
        }
    }

    // ---- group info -----------------------------------------------------------------------------------

    /// Settings carry the group's revision, which also moves with every key change. Read it fresh;
    /// if somebody else's change still lands in between, reload once and reapply only these fields.
    func update(_ group: GroupSummary, _ fields: [String: Any]) async -> Bool {
        guard let caller else { return false }
        for attempt in 0..<2 {
            do {
                let fresh = try? await client.group(caller, id: group.id)
                var body = fields
                body["expected_revision"] = fresh?.revision ?? group.revision ?? 0
                try await client.updateGroup(caller, id: group.id, fields: body)
                say("Saved.")
                await refresh()
                return true
            } catch let error as APIError where error.code == "revision_conflict" && attempt == 0 {
                continue
            } catch {
                say(GroupText.explain(error))
                break
            }
        }
        await refresh()
        return false
    }

    func change(_ group: GroupSummary, member: GroupMember, role: String? = nil, state: String? = nil) async {
        guard let caller else { return }
        do {
            try await client.changeMember(caller, id: group.id, address: member.address, revision: member.revision,
                                          role: role, state: state)
            if state == "removed" || state == "banned", let engine {
                // Commit the removal now rather than waiting for somebody else's device.
                _ = try? await engine.processPending(group.id, noJitter: true)
            }
        } catch {
            say(GroupText.explain(error))
        }
        await loadMembers(group.id)
        await refresh()
    }

    func invite(_ group: GroupSummary, raw: String) async {
        guard let caller else { return }
        let people = raw.split(whereSeparator: { $0 == "," || $0.isWhitespace }).map { PeerFace.conversationAddressInput(String($0)) }
        guard !people.isEmpty else { return }
        do {
            let results = try await client.invite(caller, id: group.id, people: people)
            let ok = results.filter { $0.status == "invited" }.count
            say(ok == people.count ? "Invited." : "\(ok) of \(people.count) invited.")
        } catch {
            say(GroupText.explain(error))
        }
        await loadMembers(group.id)
    }

    /// The link is shown once: a new one replaces the old.
    func link(_ group: GroupSummary, enabled: Bool) async -> String? {
        guard let caller else { return nil }
        do {
            let link = try await client.groupLink(caller, id: group.id, enabled: enabled)
            await refresh()
            if !enabled { say("Invite link turned off.") }
            return link.url
        } catch {
            say(GroupText.explain(error))
            return nil
        }
    }

    func transfer(_ group: GroupSummary, to: String) async {
        guard let caller else { return }
        do {
            try await client.transferGroup(caller, id: group.id, to: to, revision: group.revision)
            say("Ownership transferred.")
        } catch {
            say(GroupText.explain(error))
        }
        await refresh()
    }

    func liftBan(_ group: GroupSummary, address: String) async {
        guard let caller else { return }
        do { try await client.liftBan(caller, id: group.id, address: address) } catch { say(GroupText.explain(error)) }
        await loadMembers(group.id)
    }

    func setNotifications(_ group: GroupSummary, notify: String, muted: Bool) async {
        guard let caller else { return }
        // Muted means until further notice: the far future the web uses too.
        let until = muted ? 4_102_444_800 : nil
        do {
            try await client.setGroupNotifications(caller, id: group.id, notify: notify, mutedUntil: until)
            if let index = groups.firstIndex(where: { $0.id == group.id }) {
                groups[index].my?.notify = notify
                groups[index].my?.mutedUntil = until
            }
        } catch {
            say(GroupText.explain(error))
        }
    }

    // ---- group calls (docs/groups.md §6) -----------------------------------------------------------------

    func pollCall(_ group: String) async {
        #if DEBUG
        if Fixtures.enabled { return }
        #endif
        guard let me, let token = try? await account.session.token() else { return }
        var components = URLComponents(url: CallHost.page, resolvingAgainstBaseURL: false)!
        components.path = "/calls/v1/groups/" + (group.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? group) + "/call"
        components.query = nil
        components.queryItems = [URLQueryItem(name: "from", value: me)]
        guard let url = components.url else { return }
        var request = URLRequest(url: url)
        request.setValue("Bearer " + token, forHTTPHeaderField: "authorization")
        request.setValue("application/json", forHTTPHeaderField: "accept")
        request.timeoutInterval = 15
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              (response as? HTTPURLResponse)?.statusCode == 200,
              let state = try? PostboxClient.decode(GroupCallState.self, from: data) else {
            calls[group] = nil
            return
        }
        calls[group] = state
    }

    // ---- disappearing direct messages -------------------------------------------------------------------

    func setThreadTimer(thread: String, seconds: Int?) async -> Bool {
        guard let me else { return false }
        #if DEBUG
        if Fixtures.enabled { return true }
        #endif
        do {
            try await client.setThreadTimer(identity: me, thread: thread, seconds: seconds)
            say(seconds.map { "New messages disappear after \(GroupTimers.label($0))." } ?? "Disappearing messages are off.")
            return true
        } catch {
            say("Could not change the timer: " + GroupText.explain(error))
            return false
        }
    }

    // ---- helpers ----------------------------------------------------------------------------------------

    func say(_ text: String) { inbox?.toast = text }

    #if DEBUG
    // ---- fixtures ---------------------------------------------------------------------------------------

    /// `-fixtures`: groups in every state the screens draw, with a history that exercises notices, a
    /// verified join, a new-device security line, a warning and a disappearing message. No library,
    /// no network.
    func installFixtures() {
        guard groups.isEmpty, let me = account.me?.address else { return }
        let now = Int(Date().timeIntervalSince1970)
        mailbox = me
        identity = GroupDeviceIdentity(deviceId: "ios-fixture0000", fingerprint: "40512 77310 98245 11034")
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let json = """
        {"groups":[
         {"id":"grp_fixturegarden000000000000","name":"Garden club","description":"Seeds, soil and Saturday plans",
          "join_policy":"public","post_policy":"everyone","visibility":"unlisted","members_can_invite":true,
          "members_can_change_timer":false,"ephemeral_secs":86400,"revision":7,"epoch":4,"activation_state":"active",
          "member_count":4,"created_by":"\(me)","created_at":\(now - 90000),"last_at":\(now - 300),"last_seq":12,
          "has_link":true,"my":{"role":"owner","state":"active","read_cursor":9,"unread":2,"notify":"all","revision":3}},
         {"id":"grp_fixtureannounce0000000000","name":"Release notes","post_policy":"admins","join_policy":"private",
          "revision":2,"epoch":2,"activation_state":"active","member_count":12,"created_by":"/k/aaaa1111bbbb2222cccc3333dd",
          "created_at":\(now - 400000),"last_at":\(now - 7200),"last_seq":5,
          "my":{"role":"member","state":"active","read_cursor":5,"unread":0,"notify":"mentions","revision":1}},
         {"id":"grp_fixturekeying00000000000","name":"Infra on-call","post_policy":"everyone","join_policy":"private",
          "revision":1,"epoch":1,"activation_state":"active","member_count":3,"created_at":\(now - 3000),"last_at":\(now - 3000),
          "my":{"role":"member","state":"keying","revision":1}},
         {"id":"grp_fixtureinvite00000000000","name":"Book swap","member_count":6,"join_policy":"private",
          "my":{"role":"member","state":"invited","invited_by":"/k/eeee5555ffff6666gggg7777hh","filtered":true}}
        ]}
        """
        groups = (try? decoder.decode(GroupsResponse.self, from: Data(json.utf8)))?.groups ?? []
        let garden = "grp_fixturegarden000000000000"
        let alp = "/k/aaaa1111bbbb2222cccc3333dd", ozgur = "/k/eeee5555ffff6666gggg7777hh"
        handles = [alp: "/alp/main", ozgur: "/ozgur/main", me: account.me?.handle ?? me]
        let alpPhone = GroupDeviceInfo(mailbox: alp, deviceId: "ios-alp", fingerprint: "a1b2c3d4")
        let alpLaptop = GroupDeviceInfo(mailbox: alp, deviceId: "web-alp2", fingerprint: "e5f6a7b8")
        let mine = GroupDeviceInfo(mailbox: me, deviceId: "ios-fixture0000", fingerprint: "00112233")
        histories[garden] = [
            GroupItem(kind: .notice, seq: 1, createdAt: now - 90000, notice: "group_created", actor: me),
            GroupItem(kind: .commit, seq: 2, createdAt: now - 89000, epoch: 1, committer: mine, added: [alpPhone], removed: [], updated: []),
            GroupItem(kind: .message, seq: 3, createdAt: now - 88000, sender: alp, text: "Tomatoes are in. Who wants seedlings?"),
            GroupItem(kind: .message, seq: 4, createdAt: now - 87000, sender: me, text: "Two please, for the balcony."),
            GroupItem(kind: .notice, seq: 5, createdAt: now - 3600, notice: "timer_changed", actor: me, metadata: NoticeMeta(ephemeralSecs: 86400)),
            GroupItem(kind: .commit, seq: 6, createdAt: now - 3000, epoch: 3, committer: alpLaptop, added: [alpLaptop], removed: [], updated: []),
            GroupItem(kind: .message, seq: 7, createdAt: now - 2000, expiresAt: now + 80000, sender: ozgur, text: "Saturday at ten?",
                      files: [GroupFile(id: "gat_fixture", name: "map.png", mime: "image/png", size: 48213, ref: nil)]),
            GroupItem(kind: .message, seq: 8, createdAt: now - 300, expiresAt: now + 86000, sender: alp, text: "Works for me."),
        ]
        histories["grp_fixtureannounce0000000000"] = [
            GroupItem(kind: .message, seq: 5, createdAt: now - 7200, sender: alp, text: "0.9.0 is out: groups and disappearing messages."),
        ]
        inGroup = [garden, "grp_fixtureannounce0000000000"]
        safetyNumbers[garden] = "305718840266914073559128840013375066481927"
        members[garden] = [
            GroupMember(address: me, handle: account.me?.handle, role: "owner", state: "active", revision: 3),
            GroupMember(address: alp, handle: "/alp/main", role: "admin", state: "active", revision: 2),
            GroupMember(address: ozgur, handle: "/ozgur/main", role: "member", state: "active", revision: 1),
            GroupMember(address: "/k/qq9999v2h90vnwefj7g7ezvbh1", role: "member", state: "invited", revision: 1),
        ]
        devices[alp] = [MailboxDevices.Device(deviceId: "ios-alp", fingerprint: nil, fingerprintDigits: "11402 88301 54390 22817", createdAt: now - 89000),
                        MailboxDevices.Device(deviceId: "web-alp2", fingerprint: nil, fingerprintDigits: "90213 44871 03392 77105", createdAt: now - 3000)]
        memberDevices[garden] = [mine, alpPhone, alpLaptop]
        status = .ready
        if let staged = CommandLine.arguments.first(where: { $0.hasPrefix("-join=") }) {
            pendingJoin = JoinLink(groupId: String(staged.dropFirst("-join=".count)), token: "fixture")
        }
    }

    private func installFixtureMessage(_ group: String, text: String) {
        let next = ((histories[group] ?? []).compactMap(\.seq).max() ?? 0) + 1
        histories[group, default: []].append(GroupItem(kind: .message, seq: next, createdAt: Int(Date().timeIntervalSince1970),
                                                       sender: me, text: text))
    }
    #endif
}
