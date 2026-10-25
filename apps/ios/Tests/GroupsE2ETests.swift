// Group E2EE round trips through the Swift binding against a real postbox (Tests/groups-e2e.sh
// starts one): the client loop of docs/groups.md §3 exactly as the app runs it — the app's own
// GroupEngine, GroupAPI and GroupVault, and the library compiled for this Mac.
//
// Alice creates a group with Bob and Carol; both accept and are added by Alice's device; they
// exchange messages and an encrypted file; safety numbers agree; Alice removes Carol, who can read
// nothing after; Alice's second device joins without history and Bob's history shows it as a new
// device with its fingerprint; Bob's device survives a restart from its sealed vault; a timer gives
// new messages an expiry.
import CryptoKit
import Foundation

@MainActor final class Session {
    let bearer: String
    init(_ bearer: String) { self.bearer = bearer }
    func token() async throws -> String { bearer }
    func renew() async throws -> String { bearer }
}
enum AuthError: Error { case sessionExpired }

struct Person: Decodable { let token: String; let address: String }
struct Setup: Decodable { let postbox: String; let lossProxy: String?; let people: [String: Person] }

var checks = 0, failures = 0
func check(_ ok: Bool, _ what: String) {
    checks += 1
    if ok { print("  ok   " + what) } else { failures += 1; print("  FAIL " + what) }
}

@MainActor
func until(_ what: String, seconds: Double = 20, _ test: () async throws -> Bool) async -> Bool {
    let end = Date().addingTimeInterval(seconds)
    while Date() < end {
        if (try? await test()) == true { return true }
        try? await Task.sleep(nanoseconds: 250_000_000)
    }
    print("  …timed out waiting for " + what)
    return false
}

@main
struct GroupsE2E {
    @MainActor static func main() async {
        guard let path = CommandLine.arguments.dropFirst().first,
              let data = FileManager.default.contents(atPath: path),
              let setup = try? JSONDecoder().decode(Setup.self, from: data),
              let base = URL(string: setup.postbox) else {
            print("usage: groups-e2e <setup.json>"); exit(2)
        }
        let scratch = FileManager.default.temporaryDirectory.appendingPathComponent("pp-groups-e2e-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: scratch) }
        let key = SymmetricKey(size: .bits256)
        var sessions: [Session] = []
        func client(_ who: String, baseAt: URL? = nil) -> PostboxClient {
            let session = Session(setup.people[who]!.token)
            sessions.append(session)
            return PostboxClient(base: baseAt ?? base, tokens: session)
        }
        func vault(_ name: String) -> GroupVault { GroupVault(directory: scratch.appendingPathComponent(name), key: { key }) }
        let alice = setup.people["alice"]!.address, bob = setup.people["bob"]!.address, carol = setup.people["carol"]!.address
        let aliceClient = client("alice"), bobClient = client("bob"), carolClient = client("carol")
        var a = GroupEngine(mailbox: alice, client: aliceClient, vault: vault("alice"), events: { _ in })
        var b = GroupEngine(mailbox: bob, client: bobClient, vault: vault("bob"), events: { _ in })
        let c = GroupEngine(mailbox: carol, client: carolClient, vault: vault("carol"), events: { _ in })
        let names: (String) -> String = { [alice: "Alice", bob: "Bob", carol: "Carol"][$0] ?? $0 }

        do {
            print("Groups end to end against \(setup.postbox)")
            let ida = try await a.start(), idb = try await b.start(), _ = try await c.start()
            check(ida.deviceId.hasPrefix("mac-") && !ida.fingerprint.isEmpty, "devices register and are attested")
            for engine in [a, b, c] { _ = try await engine.topUpKeyPackages() }
            let count = try await bobClient.keyPackageCount(GroupCaller(mailbox: bob, device: idb.deviceId))
            check((count.unclaimed ?? 0) >= 10 && count.lastResort == true, "key packages topped up with a last-resort package")

            let created = try await a.createGroup(settings: ["name": "Swift club", "join_policy": "private", "post_policy": "everyone",
                                                              "visibility": "unlisted", "invite": [bob, carol]])
            let gid = created.group.id
            check(created.invitations?.allSatisfy { $0.status == "invited" } == true, "created with two invitations")
            check(await a.inGroup(gid), "the creator's device is in the MLS group")

            for (who, http) in [(bob, bobClient), (carol, carolClient)] {
                let caller = GroupCaller(mailbox: who)
                let listed = try await http.groups(caller)
                check(listed.first { $0.id == gid }?.isInvited == true, "\(names(who)) sees the invitation")
                try await http.answerInvitation(caller, id: gid, accept: true)
            }
            check(try await a.processPending(gid, noJitter: true), "Alice's device commits the two adds")
            check(try await b.processWelcomes().contains(gid), "Bob joins from his Welcome")
            check(try await c.processWelcomes().contains(gid), "Carol joins from her Welcome")

            let photo = Data((0..<5000).map { UInt8($0 % 251) })
            try await a.send(gid, text: "hello from Swift", files: [OutgoingGroupFile(name: "plan.bin", mime: "application/octet-stream", data: photo)],
                             replyTo: nil, editOf: nil, mentions: [])
            try await b.sync(gid)
            try await c.sync(gid)
            let seenByBob = await b.history(gid).last { $0.kind == .message }
            check(seenByBob?.text == "hello from Swift" && seenByBob?.sender == alice, "Bob decrypts Alice's message")
            check(await c.history(gid).contains { $0.text == "hello from Swift" }, "Carol decrypts it too")
            if let file = seenByBob?.files?.first {
                let opened = try await b.openFile(gid, file: file)
                check(opened == photo && file.name == "plan.bin", "the sealed attachment opens to the original bytes")
            } else { check(false, "the message carries its attachment reference") }

            try await b.send(gid, text: "hi Alice", files: [], replyTo: seenByBob?.seq, editOf: nil, mentions: [])
            try await a.sync(gid)
            check(await a.history(gid).contains {
                $0.sender == bob && $0.text == "hi Alice" && $0.replyTo == seenByBob?.seq.map(String.init) && $0.actionsVerified == true
            }, "Alice decrypts Bob's reply with its authenticated parent")
            let originalEdit = try await a.send(gid, text: "before editing", files: [], replyTo: nil, editOf: nil, mentions: [])
            try await b.sync(gid)
            let changed = try await a.send(gid, text: "after editing", files: [], replyTo: nil, editOf: originalEdit.seq, mentions: [])
            try await b.sync(gid)
            let editHistory = await b.history(gid)
            if let current = editHistory.first(where: { $0.seq == changed.seq }) {
                check(current.actionsVerified == true && current.editOf == String(originalEdit.seq), "an actual MLS edit verifies its inner and outer reference")
                check(GroupActions.versions(of: current, in: editHistory).compactMap(\.text) == ["before editing", "after editing"],
                      "the recipient retains its original and decrypted edit")
                let visible = GroupActions.visible(editHistory, now: Int(Date().timeIntervalSince1970))
                check(!visible.contains { $0.seq == originalEdit.seq } && visible.contains { $0.seq == changed.seq },
                      "the recipient shows the latest edit once")
            } else { check(false, "the recipient receives the encrypted edit") }
            let deletion = try await aliceClient.deleteGroupMessage(GroupCaller(mailbox: alice), id: gid, seq: originalEdit.seq)
            check(Set(deletion.seqs ?? []) == Set([originalEdit.seq, changed.seq]), "deleting an older version names the whole server chain")
            try await b.reconcileDeletions(gid)
            let afterDelete = await b.history(gid)
            check(!afterDelete.contains { $0.text == "before editing" || $0.text == "after editing" },
                  "a metadata reconciliation erases remote deletions, including retained versions")
            check(afterDelete.contains { $0.text == "hello from Swift" }, "remote erasure leaves unrelated decrypted history intact")
            // A legacy response/event may name only the selected version.
            let dropRoot = try await a.send(gid, text: "drop root", files: [], replyTo: nil, editOf: nil, mentions: [])
            try await b.sync(gid)
            let dropEdit = try await a.send(gid, text: "drop edit", files: [], replyTo: nil, editOf: dropRoot.seq, mentions: [])
            try await b.sync(gid)
            await b.dropSeqs(gid, seqs: [dropEdit.seq])
            check(!(await b.history(gid).contains { $0.text == "drop root" || $0.text == "drop edit" }),
                  "a single-version local drop erases its authenticated predecessors")
            let captionRoot = try await a.send(gid, text: "original file caption", files: [OutgoingGroupFile(name: "caption.bin", mime: "application/octet-stream", data: photo)],
                                               replyTo: nil, editOf: nil, mentions: [])
            try await b.sync(gid)
            let firstFile = await b.history(gid).first { $0.seq == captionRoot.seq }?.files?.first
            let captionEdit = try await a.send(gid, text: "updated file caption", files: [], replyTo: nil, editOf: captionRoot.seq, mentions: [])
            try await b.sync(gid)
            if let retained = await b.history(gid).first(where: { $0.seq == captionEdit.seq })?.files?.first {
                check(retained == firstFile, "a text edit retains the exact sealed file id and encryption reference")
                check(try await b.openFile(gid, file: retained) == photo, "the retained file still opens without re-uploading")
            } else { check(false, "the edited caption keeps its file") }
            if let loss = setup.lossProxy, let proxy = URL(string: loss) {
                func mode(_ value: String) async throws {
                    var request = URLRequest(url: proxy.appendingPathComponent("__test/control"))
                    request.httpMethod = "POST"
                    request.httpBody = try JSONSerialization.data(withJSONObject: ["mode": value])
                    let (_, response) = try await URLSession.shared.data(for: request)
                    guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw APIError(status: 0, code: "fixture", detail: nil) }
                }
                func attempts() async throws -> [[String: Any]] {
                    let (data, _) = try await URLSession.shared.data(from: proxy.appendingPathComponent("__test/attempts"))
                    return (try JSONSerialization.jsonObject(with: data) as? [String: Any])?["attempts"] as? [[String: Any]] ?? []
                }
                let lossClient = client("alice", baseAt: proxy)
                a = GroupEngine(mailbox: alice, client: lossClient, vault: vault("alice"), events: { _ in })
                _ = try await a.start()
                try await mode("after")
                do {
                    _ = try await a.send(gid, text: "response lost after delivery", files: [OutgoingGroupFile(name: "retry.bin", mime: "application/octet-stream", data: photo)],
                                         replyTo: nil, editOf: nil, mentions: [])
                    check(false, "a lost response is surfaced as unconfirmed")
                } catch { check(true, "a lost response is surfaced as unconfirmed") }
                guard let pending = vault("alice").load(alice)?.pendingSends?.values.first else {
                    throw APIError(status: 0, code: "missing_outbox", detail: nil)
                }
                check(pending.request != nil && pending.files.count == 1, "the exact encrypted request and file references are durable before posting")
                try await b.sync(gid)
                try await b.selfUpdate(gid)
                a = GroupEngine(mailbox: alice, client: lossClient, vault: vault("alice"), events: { _ in })
                check(try await a.start().deviceId == ida.deviceId, "an unconfirmed send survives restart without replacing the MLS identity")
                check(await a.history(gid).contains { $0.cmid == pending.cmid && $0.sendFailed == true }, "the restarted attempt offers retry rather than an endless Sending state")
                try await mode("off")
                let recovered = try await a.retryPending(gid, cmid: pending.cmid)
                try await b.sync(gid)
                let exact = try await attempts()
                check(exact.count >= 2 && Set(exact.compactMap { $0["sha"] as? String }).count == 1,
                      "response-loss retry posts byte-identical JSON, even after an epoch change")
                check(Set(exact.compactMap { $0["cmid"] as? String }) == [pending.cmid], "the recovered send keeps one client message id")
                check(await b.history(gid).filter { $0.text == "response lost after delivery" }.count == 1, "the recipient gets exactly one copy across restart and retry")
                check(vault("alice").load(alice)?.pendingSends?.isEmpty != false, "a saved acknowledgement clears its pending request")
                if let file = await b.history(gid).first(where: { $0.seq == recovered.seq })?.files?.first {
                    check(try await b.openFile(gid, file: file) == photo, "a retried file uses the original upload and still decrypts")
                }
                let distinct = try await a.send(gid, text: "response lost after delivery", files: [OutgoingGroupFile(name: "retry.bin", mime: "application/octet-stream", data: photo)],
                                                 replyTo: nil, editOf: nil, mentions: [])
                check(distinct.seq != recovered.seq, "an intentional identical send after acknowledgement is a new message")

                try await mode("before")
                do {
                    _ = try await a.send(gid, text: "response lost before delivery", files: [], replyTo: nil, editOf: nil, mentions: [])
                    check(false, "a pre-delivery disconnect is unconfirmed")
                } catch { check(true, "a pre-delivery disconnect is unconfirmed") }
                guard let before = vault("alice").load(alice)?.pendingSends?.values.first else { throw APIError(status: 0, code: "missing_outbox", detail: nil) }
                try await b.sync(gid)
                try await b.selfUpdate(gid)
                try await mode("off")
                _ = try await a.retryPending(gid, cmid: before.cmid)
                let rebased = try await attempts()
                check(Set(rebased.compactMap { $0["cmid"] as? String }) == [before.cmid]
                      && Set(rebased.compactMap { $0["sha"] as? String }).count == 2
                      && rebased.contains { $0["status"] as? Int == 409 }, "only an explicit epoch rejection re-encrypts, under the same id")
                try await b.sync(gid)
                check(await b.history(gid).filter { $0.text == "response lost before delivery" }.count == 1, "the re-encrypted retry is delivered once")

                let deletionRoot = try await a.send(gid, text: "base for pending edit", files: [], replyTo: nil, editOf: nil, mentions: [])
                try await mode("before")
                do { _ = try await a.send(gid, text: "pending edit must disappear", files: [], replyTo: nil, editOf: deletionRoot.seq, mentions: []) }
                catch { /* retained, not delivered */ }
                guard let edit = vault("alice").load(alice)?.pendingSends?.values.first else { throw APIError(status: 0, code: "missing_outbox", detail: nil) }
                _ = try await aliceClient.deleteGroupMessage(GroupCaller(mailbox: alice), id: gid, seq: deletionRoot.seq)
                try await a.reconcileDeletions(gid)
                let afterQueuedErasure = await a.history(gid)
                check(vault("alice").load(alice)?.pendingSends?[edit.cmid] == nil
                      && !afterQueuedErasure.contains { $0.text == "pending edit must disappear" }, "deleting the base erases a queued edit's plaintext and saved request")
                try await mode("off")
                do { _ = try await a.retryPending(gid, cmid: edit.cmid); check(false, "an erased queued edit cannot be retried") }
                catch { check(true, "an erased queued edit cannot be retried") }

                try await mode("before")
                for index in 0..<16 {
                    do { _ = try await a.send(gid, text: "bounded pending \(index)", files: [], replyTo: nil, editOf: nil, mentions: []) }
                    catch { /* each distinct attempt is retained */ }
                }
                let queued = vault("alice").load(alice)?.pendingSends ?? [:]
                let beforeLimit = try await attempts().count
                check(queued.count == 16, "the bounded queue retains sixteen independent failed attempts")
                do {
                    _ = try await a.send(gid, text: "one over the limit", files: [], replyTo: nil, editOf: nil, mentions: [])
                    check(false, "the pending-send limit is enforced before another HTTP post")
                } catch let error as APIError {
                    check(error.code == "outbox_full", "the pending-send limit is enforced before another HTTP post")
                }
                check(try await attempts().count == beforeLimit, "queue exhaustion makes no extra message request")
                try await mode("off")
                if let first = queued.values.first {
                    _ = try await a.retryPending(gid, cmid: first.cmid)
                    check((vault("alice").load(alice)?.pendingSends?.count ?? 0) == 15, "acknowledgement frees a pending-send slot")
                }
                for pending in queued.values { try await a.discardPending(gid, cmid: pending.cmid) }
                check(vault("alice").load(alice)?.pendingSends?.isEmpty != false, "explicit local dismissal forgets the remaining prepared requests")
            } else { check(false, "the response-loss transport fixture is required") }
            let sa = await a.safetyNumber(gid), sb = await b.safetyNumber(gid), sc = await c.safetyNumber(gid)
            check(sa != nil && sa == sb && sb == sc, "all three see the same safety number")

            // Removal.
            let aliceCaller = GroupCaller(mailbox: alice)
            let carolRow = try await aliceClient.groupMembers(aliceCaller, id: gid).first { $0.address == carol }
            try await aliceClient.changeMember(aliceCaller, id: gid, address: carol, revision: carolRow?.revision, state: "removed")
            check(try await a.processPending(gid, noJitter: true), "Alice's device commits Carol's removal")
            try await a.send(gid, text: "after Carol left", files: [], replyTo: nil, editOf: nil, mentions: [])
            try await b.sync(gid)
            check(await b.history(gid).contains { $0.text == "after Carol left" }, "Bob still reads the group")
            _ = try? await c.sync(gid)
            check(!(await c.history(gid).contains { $0.text == "after Carol left" }), "Carol reads nothing after her removal")

            // A second device for Alice: added by a member's device, no history before it.
            let a2 = GroupEngine(mailbox: alice, client: aliceClient, vault: vault("alice-2"), events: { _ in })
            let ida2 = try await a2.start()
            _ = try await a2.topUpKeyPackages()
            check(ida2.deviceId != ida.deviceId, "the second device enrols under its own id")
            let added = await until("a pending add for the second device") { try await b.processPending(gid, noJitter: true) }
            check(added, "Bob's device adds Alice's second device")
            check(try await a2.processWelcomes().contains(gid), "the second device joins")
            check(!(await a2.history(gid).contains { $0.text == "hello from Swift" }), "the new device has no earlier messages")
            let sharedCaption = try await a.send(gid, text: "caption shared after joining", files: [], replyTo: nil, editOf: captionEdit.seq, mentions: [])
            try await a2.sync(gid)
            if let retained = await a2.history(gid).first(where: { $0.seq == sharedCaption.seq })?.files?.first {
                check(try await a2.openFile(gid, file: retained) == photo, "a later member device can open a current edited file shared after joining")
            } else { check(false, "the current edit delivers its file to the new device") }
            _ = try await aliceClient.deleteGroupMessage(GroupCaller(mailbox: alice), id: gid, seq: captionRoot.seq)
            try await a2.reconcileDeletions(gid)
            check(!(await a2.history(gid).contains { $0.files?.contains(where: { $0.id == firstFile?.id }) == true }),
                  "remote chain deletion forgets the new device's retained attachment key")
            try await b.send(gid, text: "welcome, second device", files: [], replyTo: nil, editOf: nil, mentions: [])
            try await a2.sync(gid)
            check(await a2.history(gid).contains { $0.text == "welcome, second device" }, "the second device reads new messages")
            try await a.sync(gid)
            let bobHistory = await b.history(gid)
            let securityLines = bobHistory.filter { $0.kind == .commit }.flatMap { commit in
                GroupText.commitLines(bobHistory, at: commit, me: bob, myDevice: idb.deviceId, names: names).filter(\.security)
            }
            check(securityLines.contains { $0.text == "Alice added a new device" && $0.fingerprint != nil },
                  "Bob is shown that Alice added a new device, with its fingerprint")

            // Restart Bob from his sealed vault.
            let sealedDir = scratch.appendingPathComponent("bob")
            let onDisk = (try? FileManager.default.contentsOfDirectory(at: sealedDir, includingPropertiesForKeys: nil))?
                .compactMap { try? Data(contentsOf: $0) }.reduce(Data(), +) ?? Data()
            check(!onDisk.isEmpty && onDisk.range(of: Data("hello from Swift".utf8)) == nil, "the vault on disk holds no plaintext")
            b = GroupEngine(mailbox: bob, client: bobClient, vault: vault("bob"), events: { _ in })
            let again = try await b.start()
            check(again.deviceId == idb.deviceId, "Bob's device restores from its vault")
            check(await b.history(gid).contains { $0.text == "hello from Swift" }, "and keeps its decrypted history")
            check(!(await b.history(gid).contains { $0.text == "before editing" || $0.text == "after editing" || $0.text == "drop root" || $0.text == "drop edit" }),
                  "erased version plaintext does not return after a vault restart")
            try await a.send(gid, text: "after the restart", files: [], replyTo: nil, editOf: nil, mentions: [])
            try await b.sync(gid)
            check(await b.history(gid).contains { $0.text == "after the restart" }, "the restored device decrypts new messages")

            // Disappearing messages.
            let fresh = try await aliceClient.group(aliceCaller, id: gid)
            try await aliceClient.updateGroup(aliceCaller, id: gid, fields: ["ephemeral_secs": 300, "expected_revision": fresh.revision ?? 0])
            let posted = try await a.send(gid, text: "this one disappears", files: [], replyTo: nil, editOf: nil, mentions: [])
            check((posted.expiresAt ?? 0) > Int(Date().timeIntervalSince1970), "a timer gives new messages an expiry")
            try await b.sync(gid)
            check(await b.history(gid).contains { $0.text == "this one disappears" && $0.expiresAt != nil }, "and the reader records it")
            let lines = GroupText.lines(await b.history(gid), me: bob, myDevice: idb.deviceId,
                                        now: (posted.expiresAt ?? 0) + 1, names: names, digits: { $0 })
            check(!lines.contains { $0.item?.text == "this one disappears" }, "an expired message is not drawn, by the local clock")

            // Interop: Dana runs the web inbox's own client (Tests/groups-web-peer.mjs).
            if let dana = setup.people["dana"]?.address {
                let interop = try await a.createGroup(settings: ["name": "Interop", "join_policy": "private", "invite": [dana]])
                let iid = interop.group.id
                let danaAdded = await until("Dana to accept", seconds: 60) { try await a.processPending(iid, noJitter: true) }
                check(danaAdded, "Alice's device adds the web client's device")
                try await a.send(iid, text: "from Swift, with a file",
                                 files: [OutgoingGroupFile(name: "notes.txt", mime: "text/plain", data: Data("written on a Mac".utf8))],
                                 replyTo: nil, editOf: nil, mentions: [])
                let answered = await until("the web client's answer", seconds: 60) {
                    try await a.sync(iid)
                    return await a.history(iid).contains { $0.sender == dana && $0.text == "from the web client" }
                }
                check(answered, "Swift decrypts the web client's message")
                if let file = await a.history(iid).last(where: { $0.sender == dana })?.files?.first {
                    let opened = try await a.openFile(iid, file: file)
                    check(file.name == "reply.txt" && String(decoding: opened, as: UTF8.self) == "written in a browser",
                          "and opens the web client's file")
                } else { check(false, "the web client's message carries its file") }
            }
        } catch {
            check(false, "unexpected error: \(error)")
        }
        _ = sessions
        print("Groups end to end: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }
}
