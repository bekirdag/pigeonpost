//  The group client loop (docs/groups.md §3, "Client loop"), for one mailbox on this device.
//
//  A port of site-inbox/groups-core.js: same steps, same order, same retry rules, same history
//  records, so the two clients behave identically against the same postbox and the web client's
//  tests describe this one too. Cryptography is the MLS library's (crates/pigeonpost-groupcrypto
//  through its Swift binding); nothing here invents any.
//
//  Everything that touches MLS state runs one call at a time (`serial`): the library holds one
//  pending commit per group, and the state blob must be written after each mutation, in order. An
//  actor alone is not enough for that — it is re-entrant at every `await`, and every step here
//  awaits the network.

import Foundation

/// What the engine tells the screen about. Delivered on the main actor.
enum GroupEngineEvent: Sendable, Equatable {
    case history(String)
    case removed(String)
    case resync(String)
}

/// The engine's surface, so the screens can run without the library (fixtures, the hand-run test
/// scripts) and a client that cannot load it hides groups entirely, as the contract requires.
protocol GroupCrypting: Actor {
    var mailbox: String { get }
    func start() async throws -> GroupDeviceIdentity
    func reEnrol() async throws -> GroupDeviceIdentity
    func topUpKeyPackages() async throws -> Int
    func processWelcomes() async throws -> [String]
    @discardableResult func sync(_ group: String) async throws -> Int
    func reconcileDeletions(_ group: String) async throws
    @discardableResult func processPending(_ group: String, noJitter: Bool) async throws -> Bool
    func selfUpdate(_ group: String) async throws
    func createGroup(settings: [String: Any]) async throws -> CreatedGroup
    func activate(_ group: String) async throws
    @discardableResult func send(_ group: String, text: String, files: [OutgoingGroupFile], replyTo: Int?, editOf: Int?, mentions: [String]) async throws -> PostedGroupMessage
    func retryPending(_ group: String, cmid: String) async throws -> PostedGroupMessage
    func discardPending(_ group: String, cmid: String) async throws
    func openFile(_ group: String, file: GroupFile) async throws -> Data
    @discardableResult func pruneExpired() async -> Int
    func dropSeqs(_ group: String, seqs: [Int]) async
    func forget(_ group: String) async
    func history(_ group: String) -> [GroupItem]
    func inGroup(_ group: String) -> Bool
    func members(_ group: String) -> [GroupDeviceInfo]
    func safetyNumber(_ group: String) -> String?
    func identity() -> GroupDeviceIdentity?
}

struct GroupDeviceIdentity: Sendable, Equatable {
    let deviceId: String
    /// The digits a person compares (`fingerprint_digits`).
    let fingerprint: String
}

enum GroupEngines {
    /// Whether this build carries the group crypto library at all.
    static var available: Bool {
        #if GROUPCRYPTO
        return true
        #else
        return false
        #endif
    }

    static func make(mailbox: String, client: PostboxClient, vault: GroupVault,
                     events: @escaping @Sendable (GroupEngineEvent) -> Void) -> (any GroupCrypting)? {
        #if GROUPCRYPTO
        return GroupEngine(mailbox: mailbox, client: client, vault: vault, events: events)
        #else
        return nil
        #endif
    }
}

/// Hex, as the web client stores fingerprints.
func groupHex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

#if GROUPCRYPTO

actor GroupEngine: GroupCrypting {
    static let encScheme = "mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1"
    private static let keyPackageTarget = 20
    private static let keyPackageFloor = 10
    private static let historyLimit = 1000
    private static let maxRetries = 6
    private static let maxPending = 16
    private static let maxPendingBytes = 8 * 1024 * 1024

    let mailbox: String
    private let client: PostboxClient
    private let vault: GroupVault
    private let emit: @Sendable (GroupEngineEvent) -> Void
    private var device: GroupDevice?
    private var record: GroupRecord?
    private var reconciliationCursors: [String: Int] = [:]

    init(mailbox: String, client: PostboxClient, vault: GroupVault, events: @escaping @Sendable (GroupEngineEvent) -> Void) {
        self.mailbox = mailbox
        self.client = client
        self.vault = vault
        self.emit = events
    }

    private var caller: GroupCaller { GroupCaller(mailbox: mailbox, device: record?.deviceId) }

    // ---- one at a time ------------------------------------------------------------------------------

    private var busy = false
    private var waiting: [CheckedContinuation<Void, Never>] = []

    private func serial<T>(_ body: () async throws -> T) async throws -> T {
        try vault.lease?.check()
        if busy {
            await withCheckedContinuation { waiting.append($0) }
        } else {
            busy = true
        }
        defer {
            if waiting.isEmpty { busy = false } else { waiting.removeFirst().resume() }
        }
        try Task.checkCancellation()
        try vault.lease?.check()
        let result = try await body()
        try vault.lease?.check()
        return result
    }

    // ---- the device ---------------------------------------------------------------------------------

    func start() async throws -> GroupDeviceIdentity {
        try await serial {
            if device == nil, let saved = vault.load(mailbox), saved.registered, let blob = saved.stateBlob,
               let restored = try? GroupDevice.importState(blob: blob, key: saved.stateKey) {
                record = saved
                device = restored
                for pending in (saved.pendingSends ?? [:]).values {
                    markPendingFailed(pending.group, cmid: pending.cmid, code: "send_interrupted")
                }
                for (group, history) in saved.history {
                    for item in history where item.pending == true && item.seq == nil {
                        if let cmid = item.cmid, saved.pendingSends?[cmid] == nil {
                            markPendingFailed(group, cmid: cmid, code: "send_unavailable")
                        }
                    }
                }
                if saved.pendingSends?.isEmpty == false { try persist() }
            }
            if device == nil { try await enrol() }
            return identityLocked()
        }
    }

    /// A device the postbox no longer knows (revoked elsewhere, or its state lost) starts again as a
    /// new device, which every co-member is shown.
    func reEnrol() async throws -> GroupDeviceIdentity {
        try await serial {
            vault.remove(mailbox)
            record = nil
            device = nil
            try await enrol()
            return identityLocked()
        }
    }

    private func enrol() async throws {
        let deviceId = GroupIDs.newDevice()
        let fresh = try GroupDevice(mailbox: mailbox, deviceId: deviceId)
        var made = GroupRecord(deviceId: deviceId, stateKey: try generateStateKey())
        let answer = try await client.registerGroupDevice(GroupCaller(mailbox: mailbox), deviceId: deviceId,
                                                          signatureKey: fresh.signaturePublicKey())
        guard let key = answer.mailboxPublicKey.flatMap({ Data(base64Encoded: $0) }),
              let attestation = answer.attestation.flatMap({ Data(base64Encoded: $0) })
        else { throw APIError(status: 200, code: "bad_response", detail: "The postbox did not attest this device.") }
        try fresh.installAttestation(mailboxPublicKey: key, signature: attestation)
        made.registered = true
        made.fingerprint = answer.fingerprintDigits ?? (try? fingerprintDigits(fingerprint: fresh.fingerprint()))
        device = fresh
        record = made
        try persist()
    }

    private func identityLocked() -> GroupDeviceIdentity {
        GroupDeviceIdentity(deviceId: record?.deviceId ?? "",
                            fingerprint: record?.fingerprint ?? device.flatMap { try? fingerprintDigits(fingerprint: $0.fingerprint()) } ?? "")
    }

    func identity() -> GroupDeviceIdentity? { live && record != nil ? identityLocked() : nil }

    /// After every mutation, the whole record: the MLS blob (encrypted by the library under its
    /// state key) and the history, sealed together by the vault.
    private func persist() throws {
        try vault.lease?.check()
        guard let device, var current = record else { return }
        current.stateBlob = try device.exportState(key: current.stateKey)
        record = current
        try vault.save(current, for: mailbox)
    }

    private func ready() throws -> GroupDevice {
        try vault.lease?.check()
        guard let device, record != nil else { throw APIError(status: 0, code: "not_started", detail: "Group encryption is not set up on this device yet.") }
        return device
    }

    func topUpKeyPackages() async throws -> Int {
        try await serial {
            let device = try ready()
            let count = try await client.keyPackageCount(caller)
            let unclaimed = count.unclaimed ?? 0
            if unclaimed >= Self.keyPackageFloor, count.lastResort == true { return 0 }
            let need = max(0, Self.keyPackageTarget - unclaimed)
            let packages = need > 0 ? try device.generateKeyPackages(count: UInt32(need)) : []
            let lastResort = count.lastResort == true ? nil : try device.generateLastResortKeyPackage()
            try persist()
            try await client.uploadKeyPackages(caller, packages: packages, lastResort: lastResort)
            return packages.count
        }
    }

    // ---- welcomes -----------------------------------------------------------------------------------

    func processWelcomes() async throws -> [String] {
        let joined: [String] = try await serial {
            let device = try ready()
            var joined: [String] = []
            for welcome in try await client.groupWelcomes(caller) {
                var ok = false
                var code: String?
                do {
                    if device.groups().contains(welcome.groupId) { try device.forgetGroup(groupId: welcome.groupId) }
                    guard let bytes = Data(base64Encoded: welcome.welcome) else { throw GroupCryptoError.Failure(code: "bad_welcome", detail: "") }
                    _ = try device.joinFromWelcome(welcome: bytes)
                    record?.groups[welcome.groupId] = .init(cursor: max(welcome.seq ?? 0, 0), inGroup: true, acked: 0)
                    ok = true
                    joined.append(welcome.groupId)
                } catch {
                    code = Self.code(of: error) ?? "join_failed"
                }
                try persist()
                try? await client.ackWelcome(caller, id: welcome.welcomeId, joined: ok, error: code)
                if ok {
                    remember(welcome.groupId, GroupItem(kind: .notice, seq: welcome.seq ?? 0, createdAt: Self.now(),
                                                        notice: "you_joined", local: true))
                    try persist()
                    await ackEpochLocked(welcome.groupId)
                }
            }
            return joined
        }
        // A last-resort package may have been used for this join; a self-update replaces that leaf
        // key at once (P-2 §13.6) — only when no single-use packages are left, which is exactly when
        // the last-resort one was handed out.
        if !joined.isEmpty, let count = try? await client.keyPackageCount(caller), (count.unclaimed ?? 0) == 0 {
            for group in joined { try? await selfUpdate(group) }
        }
        for group in joined { emit(.history(group)) }
        return joined
    }

    private func ackEpochLocked(_ group: String) async {
        guard let device, let epoch = try? device.epoch(groupId: group) else { return }
        guard epoch > (record?.groups[group]?.acked ?? 0) else { return }
        do {
            try await client.ackEpoch(caller, id: group, epoch: epoch)
            record?.groups[group]?.acked = epoch
        } catch {
            // Retried at the next commit.
        }
    }

    // ---- reading ------------------------------------------------------------------------------------

    func sync(_ group: String) async throws -> Int {
        try await serial { try await syncLocked(group, upTo: nil) }
    }

    private enum Outcome { case message, commit, notice, removed, gap, skipped }

    /// Fetch and process everything after the cursor, strictly in sequence order. `upTo` stops early
    /// (between a commit's acceptance and merging it).
    @discardableResult
    private func syncLocked(_ group: String, upTo: Int?) async throws -> Int {
        guard record?.groups[group]?.inGroup == true else { return 0 }
        var processed = 0
        pages: for _ in 0..<1000 {
            let cursor = record?.groups[group]?.cursor ?? 0
            let page: GroupMessagesPage
            do {
                page = try await client.groupMessages(caller, id: group, after: cursor)
            } catch let error as APIError where error.status == 410 {
                try await resyncLocked(group)
                return processed
            }
            let items = (page.items ?? []).sorted { $0.seq < $1.seq }
            var stopped = false
            for item in items {
                if let upTo, item.seq > upTo { stopped = true; break }
                if item.seq <= (record?.groups[group]?.cursor ?? 0) { continue }
                let outcome = processItem(group, item)
                record?.groups[group]?.cursor = item.seq
                processed += 1
                try persist()
                switch outcome {
                case .removed:
                    emit(.removed(group))
                    return processed
                case .gap:
                    emit(.history(group))
                    return processed
                case .commit:
                    await ackEpochLocked(group)
                default:
                    break
                }
            }
            if stopped || page.more != true || items.isEmpty { break pages }
        }
        if processed > 0 { emit(.history(group)) }
        return processed
    }

    private func isOwn(_ item: GroupWireItem) -> Bool {
        item.sender == mailbox && item.senderDevice == record?.deviceId
    }

    private func processItem(_ group: String, _ item: GroupWireItem) -> Outcome {
        guard let device else { return .skipped }
        switch item.type {
        case "notice":
            remember(group, GroupItem(kind: .notice, seq: item.seq, createdAt: item.createdAt, expiresAt: item.expiresAt,
                                      notice: item.kind, actor: item.actor, metadata: item.metadata))
            return .notice

        case "commit":
            if isOwn(item), (try? device.hasPendingCommit(groupId: group)) != true { return .skipped } // merged when accepted
            guard let bytes = item.commit.flatMap({ Data(base64Encoded: $0) }) else { return .skipped }
            let incoming: Incoming
            do {
                incoming = try device.processIncoming(groupId: group, message: bytes)
            } catch {
                if Self.code(of: error) == "future_epoch" { return .gap }
                // A commit this device cannot verify: it stays at this epoch and fails closed (P-2 §3).
                remember(group, GroupItem(kind: .warning, seq: item.seq, createdAt: item.createdAt,
                                          code: Self.code(of: error) ?? "mls", warning: "commit_rejected"))
                return .skipped
            }
            let outcome: CommitOutcome
            switch incoming {
            case let .commit(o), let .ownCommitConfirmed(o): outcome = o
            default: return .skipped
            }
            let added = outcome.added.map(Self.info), removed = outcome.removed.map(Self.info)
            remember(group, GroupItem(kind: .commit, seq: item.seq, createdAt: item.createdAt,
                                      epoch: Int(outcome.epoch), committer: Self.info(outcome.committer),
                                      added: added, removed: removed, updated: outcome.updated.map(Self.info)))
            if !Self.same(added, item.adds) || !Self.same(removed, item.removes) {
                remember(group, GroupItem(kind: .warning, seq: item.seq, order: 0.5, createdAt: item.createdAt,
                                          warning: "membership_mismatch"))
            }
            if outcome.selfRemoved {
                try? device.forgetGroup(groupId: group)
                record?.groups[group]?.inGroup = false
                return .removed
            }
            return .commit

        case "application":
            if item.deletedAt != nil || item.ciphertext == nil {
                if item.deletedAt != nil {
                    applyDeletionsLocked(group, items: [item])
                } else if item.replacedBy != nil,
                          record?.history[group]?.contains(where: { $0.kind == .message && $0.seq == item.seq && $0.text != nil }) == true {
                    // A metadata refresh of a superseded message must retain its locally
                    // decrypted version. This is different from an explicit deletion.
                    return .skipped
                }
                remember(group, GroupItem(kind: .message, seq: item.seq, createdAt: item.createdAt, expiresAt: item.expiresAt,
                                          sender: item.sender, replacedBy: item.replacedBy, deleted: true))
                return .message
            }
            if isOwn(item) {
                // Recorded when it was sent; the server's copy only confirms its seq.
                if let cmid = item.clientMessageId, let pending = record?.pendingSends?[cmid] {
                    let request = pending.request.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
                    guard request?["ciphertext"] as? String == item.ciphertext else {
                        remember(group, GroupItem(kind: .warning, seq: item.seq, createdAt: item.createdAt, warning: "own_message_mismatch"))
                        return .skipped
                    }
                }
                if var list = record?.history[group],
                   let at = list.firstIndex(where: { $0.kind == .message && $0.cmid == item.clientMessageId }) {
                    list[at].seq = item.seq
                    list[at].createdAt = item.createdAt
                    list[at].expiresAt = item.expiresAt
                    list[at].pending = false
                    list[at].sendFailed = nil
                    list[at].code = nil
                    record?.history[group] = Self.sorted(list)
                }
                return .skipped
            }
            guard let bytes = item.ciphertext.flatMap({ Data(base64Encoded: $0) }) else { return .skipped }
            let incoming: Incoming
            do {
                incoming = try device.processIncoming(groupId: group, message: bytes)
            } catch {
                let code = Self.code(of: error) ?? "mls"
                remember(group, GroupItem(kind: .message, seq: item.seq, createdAt: item.createdAt, expiresAt: item.expiresAt,
                                          sender: item.sender, undecryptable: true, code: code))
                return code == "future_epoch" ? .gap : .message
            }
            guard case let .message(received) = incoming else { return .skipped }
            guard received.sender.mailbox == item.sender else {
                remember(group, GroupItem(kind: .warning, seq: item.seq, createdAt: item.createdAt, warning: "sender_mismatch"))
                return .skipped
            }
            let body = GroupBody.decode(received.body)
            let editMatches = body.editOf == item.editOf.map(String.init)
            let replyMatches = body.replyTo == item.replyTo.map(String.init)
            if !editMatches || !replyMatches {
                remember(group, GroupItem(kind: .warning, seq: item.seq, order: 0.5, createdAt: item.createdAt,
                                          warning: "action_mismatch"))
            }
            let files = (body.files ?? []).enumerated().map { index, file in
                GroupFile(id: file.id, name: file.name, mime: file.mime, size: file.size,
                          ref: index < received.attachments.count ? Self.key(received.attachments[index]) : nil)
            }
            remember(group, GroupItem(kind: .message, seq: item.seq, createdAt: item.createdAt, expiresAt: item.expiresAt,
                                      sender: received.sender.mailbox, senderDevice: received.sender.deviceId, cmid: item.clientMessageId,
                                      text: body.text ?? "", files: files, replyTo: replyMatches ? body.replyTo : nil,
                                      editOf: editMatches ? body.editOf : nil, actionsVerified: editMatches && replyMatches))
            return .message

        default:
            return .skipped
        }
    }

    private func resyncLocked(_ group: String) async throws {
        try? device?.forgetGroup(groupId: group)
        let cursor = record?.groups[group]?.cursor ?? 0
        record?.groups[group]?.inGroup = false
        remember(group, GroupItem(kind: .warning, seq: cursor, order: 0.5, createdAt: Self.now(), warning: "resynced"))
        try persist()
        try? await client.resyncGroup(caller, id: group)
        emit(.resync(group))
    }

    // ---- commits ------------------------------------------------------------------------------------

    private struct Prepared {
        let bundle: CommitBundle
        let adds: [[String: Any]]
        let removes: [[String: Any]]
        let revision: Int?
    }

    /// Build, post and merge one commit. On a lost race: discard, catch up, rebuild — 200 ms base,
    /// doubling, 10 s cap (docs/groups.md §3 step 4).
    @discardableResult
    private func commitLocked(_ group: String, activation: Bool = false,
                              plan: () async throws -> Prepared?) async throws -> PostedCommit? {
        let device = try ready()
        for attempt in 0..<Self.maxRetries {
            // A provisioning group has nothing to read yet, and its creator is not yet a reader.
            if !activation { try await syncLocked(group, upTo: nil) }
            guard record?.groups[group]?.inGroup == true else { return nil }
            guard let prepared = try await plan() else { return nil }
            try persist() // the pending commit is part of the state
            let posted: PostedCommit
            do {
                var body: [String: Any] = [
                    "request_key": GroupIDs.requestKey(),
                    "epoch": prepared.bundle.epoch,
                    "commit_id": prepared.bundle.commitId,
                    "commit": prepared.bundle.commit.base64EncodedString(),
                    "envelope_signature": prepared.bundle.envelopeSignature.base64EncodedString(),
                    "welcome": prepared.bundle.welcome.map { $0.base64EncodedString() as Any } ?? NSNull(),
                    "adds": prepared.adds,
                    "removes": prepared.removes,
                ]
                body["expected_revision"] = prepared.revision.map { $0 as Any } ?? NSNull()
                posted = try await client.postCommit(caller, id: group, body: body)
            } catch {
                try? device.discardCommit(groupId: group)
                try? persist()
                if let api = error as? APIError, api.status == 409,
                   api.code == "revision_conflict" || api.code == "epoch_conflict" {
                    let backoff = min(10_000, 200 << attempt) + Int.random(in: 0..<100)
                    try? await Task.sleep(nanoseconds: UInt64(backoff) * 1_000_000)
                    continue
                }
                throw error
            }
            // Accepted. Anything sequenced before it is still in the old epoch: read that first. A
            // network failure here must not discard a commit the postbox has already sequenced —
            // that would leave this device an epoch behind its own group — so try a little longer,
            // and merge regardless.
            if !activation {
                for retry in 0..<3 {
                    do { try await syncLocked(group, upTo: posted.seq - 1); break } catch {
                        try? await Task.sleep(nanoseconds: UInt64(300 * (retry + 1)) * 1_000_000)
                    }
                }
            }
            let outcome = try device.confirmCommit(groupId: group)
            let cursor = max(record?.groups[group]?.cursor ?? 0, posted.seq)
            record?.groups[group]?.cursor = cursor
            remember(group, GroupItem(kind: .commit, seq: posted.seq, createdAt: Self.now(), epoch: Int(outcome.epoch),
                                      committer: Self.info(outcome.committer), added: outcome.added.map(Self.info),
                                      removed: outcome.removed.map(Self.info), updated: outcome.updated.map(Self.info)))
            try persist()
            await ackEpochLocked(group)
            emit(.history(group))
            return posted
        }
        throw APIError(status: 409, code: "revision_conflict", detail: nil)
    }

    /// Commit whatever the postbox says is pending: adds for keying members and new devices,
    /// removals for departed members and revoked devices. Any member device may do it; a short
    /// random wait first, so a group's online devices do not all race for the same commit.
    func processPending(_ group: String, noJitter: Bool) async throws -> Bool {
        var state = try await client.groupCrypto(caller, id: group)
        if state.inGroup != true, state.activationState == "active" { return false }
        guard !(state.pending ?? []).isEmpty else { return false }
        if !noJitter {
            try? await Task.sleep(nanoseconds: UInt64(Int.random(in: 0...3000)) * 1_000_000)
            state = try await client.groupCrypto(caller, id: group)
            guard !(state.pending ?? []).isEmpty else { return false }
        }
        return try await serial {
            try await commitLocked(group) { try await self.pendingPlan(group, adds: true) } != nil
        }
    }

    private func pendingPlan(_ group: String, adds includeAdds: Bool) async throws -> Prepared? {
        let device = try ready()
        let fresh = try await client.groupCrypto(caller, id: group)
        let pending = fresh.pending ?? []
        guard !pending.isEmpty else { return nil }
        let removes = pending.filter { $0.kind == "remove" }.compactMap { change -> DeviceAddress? in
            guard let id = change.deviceId else { return nil }
            return DeviceAddress(mailbox: change.mailbox, deviceId: id)
        }
        var claims: [KeyPackageClaims.Claim] = []
        if includeAdds {
            let wanted = Set(pending.filter { $0.kind == "add" }.map { $0.mailbox + "|" + ($0.deviceId ?? "") })
            let mailboxes = Array(Set(pending.filter { $0.kind == "add" }.map(\.mailbox))).sorted()
            if !mailboxes.isEmpty {
                // Only devices the postbox still lists as pending; never this device itself.
                claims = try await client.claimKeyPackages(caller, id: group, mailboxes: mailboxes).filter {
                    wanted.contains($0.mailbox + "|" + $0.deviceId) && !($0.mailbox == mailbox && $0.deviceId == record?.deviceId)
                }
            }
        }
        guard !claims.isEmpty || !removes.isEmpty else { return nil }
        let bundle = try device.commit(groupId: group,
                                       keyPackages: claims.compactMap { Data(base64Encoded: $0.keyPackage) },
                                       remove: removes.map { DeviceRef(mailbox: $0.mailbox, deviceId: $0.deviceId) })
        return Prepared(bundle: bundle,
                        adds: claims.map { ["mailbox": $0.mailbox, "device_id": $0.deviceId, "key_package_ref": $0.keyPackageRef] },
                        removes: removes.map { ["mailbox": $0.mailbox, "device_id": $0.deviceId] },
                        revision: fresh.revision)
    }

    func selfUpdate(_ group: String) async throws {
        try await serial {
            let device = try ready()
            try await commitLocked(group) {
                let fresh = try await self.client.groupCrypto(self.caller, id: group)
                return Prepared(bundle: try device.selfUpdate(groupId: group), adds: [], removes: [], revision: fresh.revision)
            }
        }
    }

    // ---- creating -----------------------------------------------------------------------------------

    /// Create the row, the MLS group at epoch 0, and the first commit — which activates the group
    /// and adds this mailbox's other devices in the same step.
    func createGroup(settings: [String: Any]) async throws -> CreatedGroup {
        _ = try ready()
        let created = try await client.createGroup(caller, settings: settings)
        try await activate(created.group.id)
        return created
    }

    func activate(_ group: String) async throws {
        try await serial {
            let device = try ready()
            if !device.groups().contains(group) {
                _ = try device.createGroup(groupId: group)
                record?.groups[group] = .init(cursor: 0, inGroup: true, acked: 0)
                try persist()
            }
            try await commitLocked(group, activation: true) {
                let fresh = try await self.client.groupCrypto(self.caller, id: group)
                if fresh.activationState == "active", ((try? device.epoch(groupId: group)) ?? 0) > 0 { return nil }
                let adds = (fresh.pending ?? []).filter {
                    $0.kind == "add" && !($0.mailbox == self.mailbox && $0.deviceId == self.record?.deviceId)
                }
                var claims: [KeyPackageClaims.Claim] = []
                if !adds.isEmpty {
                    claims = try await self.client.claimKeyPackages(self.caller, id: group,
                                                                    mailboxes: Array(Set(adds.map(\.mailbox))).sorted())
                        .filter { !($0.mailbox == self.mailbox && $0.deviceId == self.record?.deviceId) }
                }
                let bundle = claims.isEmpty
                    ? try device.selfUpdate(groupId: group)
                    : try device.commit(groupId: group, keyPackages: claims.compactMap { Data(base64Encoded: $0.keyPackage) }, remove: [])
                return Prepared(bundle: bundle,
                                adds: claims.map { ["mailbox": $0.mailbox, "device_id": $0.deviceId, "key_package_ref": $0.keyPackageRef] },
                                removes: [], revision: fresh.revision)
            }
        }
    }

    // ---- sending ------------------------------------------------------------------------------------

    func send(_ group: String, text: String, files: [OutgoingGroupFile], replyTo: Int?, editOf: Int?,
              mentions: [String]) async throws -> PostedGroupMessage {
        _ = try ready()
        let fingerprint = try GroupSendIntent.fingerprint(group, text: text, files: files, replyTo: replyTo, editOf: editOf, mentions: mentions)
        let retryId: String? = try await serial {
            if let existing = pendingSend(group, fingerprint: fingerprint) { return existing.cmid }
            try cleanConfirmedPendingLocked()
            guard (record?.pendingSends?.count ?? 0) < Self.maxPending else {
                throw APIError(status: 0, code: "outbox_full", detail: nil)
            }
            return nil
        }
        if let retryId { return try await retryPending(group, cmid: retryId) }
        let cmid = GroupIDs.clientMessage()
        // Sealed and uploaded before the lock: the bytes are encrypted under a fresh key each, and
        // only the reference to that key travels inside the MLS message.
        var wire: [GroupBody.WireFile] = []
        var refs: [AttachmentRef] = []
        if let editOf {
            guard files.isEmpty else { throw APIError(status: 400, code: "edit_has_attachments", detail: nil) }
            for file in record?.history[group]?.first(where: { $0.kind == .message && $0.seq == editOf })?.files ?? [] {
                guard let ref = file.ref, let key = Data(base64Encoded: ref.key), let sha = Data(base64Encoded: ref.sha) else {
                    throw APIError(status: 0, code: "no_key", detail: "This message's files cannot be retained on this device.")
                }
                wire.append(.init(id: file.id, name: file.name, mime: file.mime, size: file.size))
                refs.append(AttachmentRef(key: key, sealedSha256: sha, plaintextLen: ref.len))
            }
        }
        for file in files {
            let sealed = try sealAttachment(plaintext: file.data)
            let uploaded = try await client.uploadGroupAttachment(caller, id: group, sealed: sealed.sealed)
            wire.append(.init(id: uploaded.id, name: file.name, mime: file.mime, size: file.data.count))
            refs.append(sealed.reference)
        }
        let body = GroupBody(text: text, files: wire.isEmpty ? nil : wire, replyTo: replyTo.map(String.init),
                             editOf: editOf.map(String.init)).encoded()
        let localFiles = zip(wire, refs).map { file, ref in
            GroupFile(id: file.id, name: file.name, mime: file.mime, size: file.size, ref: Self.key(ref))
        }
        return try await serial {
            // Two calls may have uploaded concurrently. A failed identical attempt still owns
            // its original id and ciphertext; any redundant unclaimed upload expires normally.
            if let existing = pendingSend(group, fingerprint: fingerprint) {
                return try await deliverPendingLocked(group, cmid: existing.cmid)
            }
            try cleanConfirmedPendingLocked()
            guard (record?.pendingSends?.count ?? 0) < Self.maxPending else {
                throw APIError(status: 0, code: "outbox_full", detail: nil)
            }
            let pending = GroupPendingSend(group: group, cmid: cmid, fingerprint: fingerprint, createdAt: Self.now(),
                                           request: nil, body: body, files: localFiles, mentions: mentions.sorted())
            setPending(cmid, pending)
            // On screen at once as "Sending…", and the record the server's copy is matched against.
            remember(group, GroupItem(kind: .message, seq: nil, createdAt: pending.createdAt, sender: mailbox,
                                      senderDevice: record?.deviceId, cmid: cmid, text: text, files: localFiles,
                                      replyTo: replyTo.map(String.init), editOf: editOf.map(String.init), pending: true,
                                      sendFingerprint: fingerprint, actionsVerified: true))
            do { try persist() } catch {
                markPendingFailed(group, cmid: cmid, code: "storage_error")
                emit(.history(group))
                throw error
            }
            emit(.history(group))
            return try await deliverPendingLocked(group, cmid: cmid)
        }
    }

    func retryPending(_ group: String, cmid: String) async throws -> PostedGroupMessage {
        try await serial { try await deliverPendingLocked(group, cmid: cmid) }
    }

    /// Explicitly dismisses a local attempt; it never deletes a message that reached the server.
    func discardPending(_ group: String, cmid: String) async throws {
        try await serial {
            guard record?.pendingSends?[cmid]?.group == group
                || record?.history[group]?.contains(where: { $0.cmid == cmid && $0.pending == true && $0.seq == nil }) == true else { return }
            let before = record
            setPending(cmid, nil)
            record?.history[group]?.removeAll { $0.cmid == cmid && $0.pending == true && $0.seq == nil }
            do { try persist() } catch { record = before; throw error }
            emit(.history(group))
        }
    }

    private func pendingSend(_ group: String, fingerprint: String) -> GroupPendingSend? {
        record?.pendingSends?.values.filter { $0.group == group && $0.fingerprint == fingerprint }
            .min { $0.createdAt < $1.createdAt }
    }

    private func setPending(_ cmid: String, _ pending: GroupPendingSend?) {
        if record?.pendingSends == nil { record?.pendingSends = [:] }
        record?.pendingSends?[cmid] = pending
    }

    private func cleanConfirmedPendingLocked() throws {
        let saved = record?.pendingSends
        var changed = false
        for pending in (saved ?? [:]).values {
            if record?.history[pending.group]?.contains(where: { $0.cmid == pending.cmid && $0.seq != nil && $0.pending != true }) == true {
                setPending(pending.cmid, nil)
                changed = true
            }
        }
        guard changed else { return }
        do { try persist() } catch { record?.pendingSends = saved; throw error }
    }

    private func deliverPendingLocked(_ group: String, cmid: String) async throws -> PostedGroupMessage {
        guard let initial = record?.pendingSends?[cmid], initial.group == group else {
            if let sent = record?.history[group]?.first(where: { $0.kind == .message && $0.cmid == cmid && $0.pending != true }),
               let seq = sent.seq {
                return PostedGroupMessage(seq: seq, id: nil, expiresAt: sent.expiresAt)
            }
            throw APIError(status: 0, code: "send_unavailable", detail: nil)
        }
        do {
            for attempt in 0..<Self.maxRetries {
                guard var pending = record?.pendingSends?[cmid], pending.group == group else {
                    throw APIError(status: 0, code: "send_unavailable", detail: nil)
                }
                if let sent = record?.history[group]?.first(where: { $0.kind == .message && $0.cmid == cmid && $0.pending != true }),
                   let seq = sent.seq {
                    return try finishPendingLocked(pending, posted: PostedGroupMessage(seq: seq, id: nil, expiresAt: sent.expiresAt))
                }
                let now = Self.now()
                guard now >= pending.createdAt, now - pending.createdAt < 86_400 else {
                    throw APIError(status: 0, code: "send_retry_expired", detail: nil)
                }
                if pending.request == nil {
                    try await syncLocked(group, upTo: nil)
                    guard record?.pendingSends?[cmid] != nil else { throw APIError(status: 0, code: "send_unavailable", detail: nil) }
                    guard record?.groups[group]?.inGroup == true else { throw APIError(status: 403, code: "not_member", detail: nil) }
                    let device = try ready()
                    let refs = try pending.files.map { file -> AttachmentRef in
                        guard let ref = file.ref, let key = Data(base64Encoded: ref.key), let sha = Data(base64Encoded: ref.sha) else {
                            throw APIError(status: 0, code: "no_key", detail: nil)
                        }
                        return AttachmentRef(key: key, sealedSha256: sha, plaintextLen: ref.len)
                    }
                    let out = try device.encryptMessage(groupId: group, clientMessageId: cmid, body: pending.body, attachments: refs)
                    let body = GroupBody.decode(pending.body)
                    var request: [String: Any] = [
                        "request_key": "rk_" + cmid,
                        "client_message_id": cmid,
                        "epoch": out.epoch,
                        "enc_scheme": Self.encScheme,
                        "ciphertext": out.ciphertext.base64EncodedString(),
                        "envelope_signature": out.envelopeSignature.base64EncodedString(),
                        "attachments": pending.files.map(\.id),
                    ]
                    if let replyTo = GroupActions.sequence(body.replyTo) { request["reply_to"] = replyTo }
                    if let editOf = GroupActions.sequence(body.editOf) { request["edit_of"] = editOf }
                    if !pending.mentions.isEmpty { request["mentions"] = pending.mentions }
                    let bytes = try JSONSerialization.data(withJSONObject: request, options: [.sortedKeys, .withoutEscapingSlashes])
                    let otherBytes = record?.pendingSends?.values.filter { $0.cmid != cmid }.reduce(0) { $0 + ($1.request?.count ?? 0) + $1.body.count } ?? 0
                    guard bytes.count + pending.body.count + otherBytes <= Self.maxPendingBytes else {
                        throw APIError(status: 0, code: "outbox_full", detail: nil)
                    }
                    pending.request = bytes
                    setPending(cmid, pending)
                    // The exact request and the advanced MLS sender state commit together
                    // before the first HTTP attempt. A crash cannot require encrypting twice.
                    try persist()
                }
                guard let bytes = pending.request else { throw APIError(status: 0, code: "send_unavailable", detail: nil) }
                do {
                    let posted = try await client.postEncodedGroupMessage(caller, id: group, body: bytes)
                    return try finishPendingLocked(pending, posted: posted)
                } catch let error as APIError where error.status == 409 && error.code == "epoch_conflict" {
                    pending.request = nil
                    setPending(cmid, pending)
                    try persist()
                    try? await Task.sleep(nanoseconds: UInt64(50 * (attempt + 1)) * 1_000_000)
                } catch let error as APIError where error.status == 409 && error.code == "key_activation_pending" {
                    pending.request = nil
                    setPending(cmid, pending)
                    try persist()
                    try await commitLocked(group) { try await self.pendingPlan(group, adds: false) }
                }
            }
            throw APIError(status: 409, code: "epoch_conflict", detail: nil)
        } catch {
            markPendingFailed(group, cmid: cmid, code: Self.code(of: error) ?? "send_failed")
            try? persist()
            emit(.history(group))
            throw error
        }
    }

    private func finishPendingLocked(_ pending: GroupPendingSend, posted: PostedGroupMessage) throws -> PostedGroupMessage {
        let group = pending.group
        if var list = record?.history[group], let at = list.firstIndex(where: { $0.cmid == pending.cmid && $0.kind == .message }) {
            list[at].seq = posted.seq
            list[at].pending = false
            list[at].sendFailed = nil
            list[at].code = nil
            list[at].expiresAt = posted.expiresAt
            record?.history[group] = Self.sorted(list)
        }
        setPending(pending.cmid, nil)
        do { try persist() } catch {
            // Keep the retry identity in memory too if the acknowledgement could not be saved.
            setPending(pending.cmid, pending)
            throw error
        }
        emit(.history(group))
        return posted
    }

    private func markPendingFailed(_ group: String, cmid: String, code: String) {
        guard var list = record?.history[group], let at = list.firstIndex(where: { $0.cmid == cmid && $0.seq == nil }) else { return }
        list[at].sendFailed = true
        list[at].code = code
        record?.history[group] = list
    }

    func openFile(_ group: String, file: GroupFile) async throws -> Data {
        guard let ref = file.ref, let key = Data(base64Encoded: ref.key), let sha = Data(base64Encoded: ref.sha) else {
            throw APIError(status: 0, code: "no_key", detail: "This file cannot be opened on this device.")
        }
        let sealed = try await client.downloadGroupAttachment(caller, id: group, attachment: file.id)
        try vault.lease?.check()
        let data = try openAttachment(sealed: sealed, reference: AttachmentRef(key: key, sealedSha256: sha, plaintextLen: ref.len))
        try vault.lease?.check()
        return data
    }

    // ---- housekeeping -------------------------------------------------------------------------------

    /// The native app polls rather than retaining the SSE connection. Recheck only metadata
    /// within its retained history, so deletions made elsewhere cannot remain as plaintext
    /// indefinitely. Existing ciphertext is never fed through MLS a second time.
    func reconcileDeletions(_ group: String) async throws {
        try await serial {
            guard record?.groups[group]?.inGroup == true,
                  let first = record?.history[group]?.filter({ $0.kind == .message && $0.deleted != true }).compactMap(\.seq).min(),
                  let through = record?.groups[group]?.cursor, through < Int.max else { return }
            var after = max(first - 1, reconciliationCursors[group] ?? 0)
            if after > through { after = first - 1 }
            for _ in 0..<20 {
                let page = try await client.groupMessages(caller, id: group, after: after,
                                                         before: through + 1, metadataOnly: true)
                let items = (page.items ?? []).filter { $0.seq > after && $0.seq <= through }
                let deleted = items.filter { $0.type == "application" && $0.deletedAt != nil }
                if !deleted.isEmpty {
                    applyDeletionsLocked(group, items: deleted)
                    try persist()
                    emit(.history(group))
                }
                guard let next = items.map(\.seq).max(), next > after, page.more == true else {
                    reconciliationCursors[group] = nil
                    return
                }
                after = next
                reconciliationCursors[group] = next
            }
            // Very sparse/large windows continue at the next cadence, rather than issuing an
            // unbounded number of requests or repeatedly scanning only the first twenty pages.
        }
    }

    /// Drop expired items by this device's own clock, whether or not the postbox's event arrived.
    func pruneExpired() async -> Int {
        (try? await serial {
            guard var current = record else { return 0 }
            let now = Self.now()
            var dropped = 0
            for (group, list) in current.history {
                let kept = GroupActions.pruning(list, now: now)
                dropped += list.count - kept.count
                current.history[group] = kept
                let expired = GroupActions.linkedSequences(list, seeds: Set(list.filter { ($0.expiresAt ?? .max) <= now }.compactMap(\.seq)))
                let cmids = Set(list.filter { $0.seq.map(expired.contains) == true }.compactMap(\.cmid))
                for pending in (current.pendingSends ?? [:]).values where pending.group == group && pending.depends(on: expired, cmids: cmids) {
                    current.pendingSends?[pending.cmid] = nil
                }
            }
            if dropped > 0 {
                record = current
                try? persist()
            }
            return dropped
        }) ?? 0
    }

    func dropSeqs(_ group: String, seqs: [Int]) async {
        _ = try? await serial {
            eraseLocked(group, seqs: Set(seqs))
            try? persist()
            emit(.history(group))
        }
    }

    func forget(_ group: String) async {
        _ = try? await serial {
            try? device?.forgetGroup(groupId: group)
            record?.groups[group] = nil
            record?.history[group] = nil
            reconciliationCursors[group] = nil
            let pendingIds = (record?.pendingSends ?? [:]).values.filter { $0.group == group }.map(\.cmid)
            for cmid in pendingIds { setPending(cmid, nil) }
            try? persist()
        }
    }

    private var live: Bool { vault.lease?.isValid ?? true }
    func history(_ group: String) -> [GroupItem] { live ? record?.history[group] ?? [] : [] }
    func inGroup(_ group: String) -> Bool { live && record?.groups[group]?.inGroup == true }

    func members(_ group: String) -> [GroupDeviceInfo] {
        guard live else { return [] }
        return ((try? device?.members(groupId: group)) ?? []).map(Self.info)
    }

    func safetyNumber(_ group: String) -> String? { live ? try? device?.safetyNumber(groupId: group) : nil }

    // ---- helpers ------------------------------------------------------------------------------------

    private func applyDeletionsLocked(_ group: String, items: [GroupWireItem]) {
        var history = record?.history[group] ?? []
        for item in items where isOwn(item) {
            if let cmid = item.clientMessageId,
               let at = history.firstIndex(where: { $0.kind == .message && $0.cmid == cmid }) {
                history[at].seq = item.seq
            }
        }
        record?.history[group] = history
        eraseLocked(group, seqs: Set(items.map(\.seq)))
    }

    private func eraseLocked(_ group: String, seqs: Set<Int>) {
        let history = record?.history[group] ?? []
        let linked = GroupActions.linkedSequences(history, seeds: seqs)
        let cmids = Set(history.filter { $0.seq.map(linked.contains) == true }.compactMap(\.cmid))
        let pendingIds = (record?.pendingSends ?? [:]).values
            .filter { $0.group == group && $0.depends(on: linked, cmids: cmids) }.map(\.cmid)
        for cmid in pendingIds { setPending(cmid, nil) }
        record?.history[group] = GroupActions.erasing(history, seqs: seqs)
    }

    private func remember(_ group: String, _ item: GroupItem) {
        var list = record?.history[group] ?? []
        if let seq = item.seq, let at = list.firstIndex(where: { $0.seq == seq && $0.kind == item.kind && $0.order == item.order }) {
            list[at] = item
        } else {
            list.append(item)
        }
        list = Self.sorted(list)
        if list.count > Self.historyLimit { list.removeFirst(list.count - Self.historyLimit) }
        record?.history[group] = list
    }

    private func markReplaced(_ group: String, seq: Int, by replacement: Int) {
        guard var list = record?.history[group], let at = list.firstIndex(where: { $0.kind == .message && $0.seq == seq }) else { return }
        list[at].replacedBy = replacement
        record?.history[group] = list
    }

    private static func sorted(_ list: [GroupItem]) -> [GroupItem] {
        list.enumerated().sorted { a, b in
            a.element.sortKey == b.element.sortKey ? a.offset < b.offset : a.element.sortKey < b.element.sortKey
        }.map(\.element)
    }

    private static func info(_ member: MemberInfo) -> GroupDeviceInfo {
        GroupDeviceInfo(mailbox: member.mailbox, deviceId: member.deviceId, fingerprint: groupHex(member.fingerprint))
    }

    private static func key(_ ref: AttachmentRef) -> GroupFile.FileKey {
        .init(key: ref.key.base64EncodedString(), sha: ref.sealedSha256.base64EncodedString(), len: ref.plaintextLen)
    }

    /// Compare what the verified commit did with what the postbox declared it would do.
    private static func same(_ verified: [GroupDeviceInfo], _ declared: [DeviceAddress]?) -> Bool {
        guard let declared else { return true }
        return verified.map { $0.mailbox + "|" + $0.deviceId }.sorted() == declared.map { $0.mailbox + "|" + $0.deviceId }.sorted()
    }

    static func code(of error: Error) -> String? {
        if case let GroupCryptoError.Failure(code, _) = error { return code }
        return (error as? APIError)?.code
    }

    private static func now() -> Int { Int(Date().timeIntervalSince1970) }
}

#endif
