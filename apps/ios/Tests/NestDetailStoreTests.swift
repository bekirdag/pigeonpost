//  One Nest's page (Nests B3-B5): what is offered to whom, that an approval binds exactly the call
//  shown, and that the answers the postbox really sends decode. Plain swiftc, like NestStoreTests.

import Foundation

@MainActor
private final class Backend {
    var log: [String] = []
    var nest: NestView
    var cards: [NestApproval] = []
    var error: APIError?
    var nestError: APIError?

    init(_ nest: NestView) { self.nest = nest }

    private func take() throws {
        if let e = error { error = nil; throw e }
    }

    func services() -> NestDetailServices {
        NestDetailServices(
            nest: { if let e = self.nestError { throw e }; return self.nest },
            files: { NestFiles(files: [], usedBytes: 0, quotaBytes: 1) },
            approvals: { self.cards },
            usage: { NestUsage(period: nil, inputTokens: .init(used: nil, limit: nil), outputTokens: .init(used: nil, limit: nil),
                               runningSecs: nil, filesBytes: 0, package: nil) },
            upload: { name, data in try self.take(); self.log.append("upload:\(name):\(data.count)") },
            deleteFile: { id in try self.take(); self.log.append("deleteFile:\(id)") },
            decide: { id, decision, hash in try self.take(); self.log.append("decide:\(id):\(decision):\(hash)") },
            action: { action in try self.take(); self.log.append("action:\(action)") },
            rename: { label in try self.take(); self.log.append("rename:\(label)") },
            export: { self.log.append("export"); return Data("PK".utf8) },
            delete: { confirm in try self.take(); self.log.append("delete:\(confirm)") },
            changed: { self.log.append("changed") })
    }
}

private func nest(state: String = "running", paused: String? = nil, role: String? = nil, status: String? = "ready") -> NestView {
    NestView(nestId: "nst_1", address: "/k/a", label: "Helper", state: state, funding: "package", status: status,
             statusReason: nil, pausedReason: paused, deleteAfter: nil, role: role)
}

private func card(_ id: String = "c1", tool: String = "send_message", to: String? = "/bekir/main", status: String = "pending",
                  expires: Int = 4_000_000_000) -> NestApproval {
    NestApproval(approvalId: id, tool: tool, call: .init(tool: tool, args: .init(to: to, body: "hello")),
                 argsHash: "hash-" + id, status: status, expiresAt: expires, result: nil)
}

@main
@MainActor
struct NestDetailStoreTests {
    static var checks = 0
    static func expect(_ value: @autoclosure () -> Bool, _ what: String, line: UInt = #line) {
        checks += 1
        if !value() { fatalError("Nest detail test line \(line): \(what)") }
    }

    fileprivate static func make(_ view: NestView = nest()) -> (Backend, NestDetailStore) {
        let api = Backend(view)
        return (api, NestDetailStore(nest: view, services: api.services()))
    }

    static func decode<T: Decodable>(_ type: T.Type, _ json: String) -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        do { return try decoder.decode(T.self, from: Data(json.utf8)) } catch { fatalError("decode: \(error)") }
    }

    static func main() async {
        // An approval binds the hash of exactly the card on screen.
        do {
            let (api, store) = make()
            api.cards = [card()]
            await store.load()
            expect(store.pending.count == 1 && store.approvalsLoaded, "the card is shown")
            await store.decide(store.pending[0], approve: true)
            expect(api.log == ["decide:c1:approve:hash-c1"], "the hash shown: \(api.log)")
        }
        // A card this app cannot display may be rejected and never approved.
        do {
            let (api, store) = make()
            let unknown = card(tool: "post_invoice")
            expect(!unknown.isReviewable, "an unknown tool is not reviewable")
            expect(!card(to: nil).isReviewable, "a card with no recipient is not reviewable")
            await store.decide(unknown, approve: true)
            expect(api.log.isEmpty, "not approved")
            await store.decide(unknown, approve: false)
            expect(api.log == ["decide:c1:reject:hash-c1"], "rejected: \(api.log)")
        }
        // The call changed under the person: they are told, and shown the new card.
        do {
            let (api, store) = make()
            api.cards = [card()]
            await store.load()
            api.cards = [card("c2")]
            api.error = APIError(status: 409, code: "approval_changed", detail: nil)
            await store.decide(card(), approve: true)
            expect(store.message?.contains("changed") == true, "explained")
            expect(store.pending.map(\.approvalId) == ["c2"], "and the new card replaces it")
        }
        // Expired cards are not offered even before the postbox marks them.
        do {
            let (api, store) = make()
            api.cards = [card("old", expires: 500), card("new", expires: 2000)]
            store.now = { Date(timeIntervalSince1970: 1000) }
            await store.load()
            expect(store.pending.map(\.approvalId) == ["new"], "only the live card")
        }
        // Decided cards keep their outcome, most recent five.
        do {
            let (api, store) = make()
            api.cards = (0..<7).map { card("d\($0)", status: "rejected") }
            await store.load()
            expect(store.decided.count == 5 && store.pending.isEmpty, "five decided")
            let denied = NestApproval(approvalId: "x", tool: "send_message", call: nil, argsHash: "h", status: "failed", expiresAt: 0, result: "recipient_not_allowed")
            expect(NestWords.outcome(denied).contains("may only message"), "a refused recipient is explained")
        }
        // Pause and resume: a funding pause is lifted by paying, not by a button.
        do {
            expect(make(nest(state: "paused", paused: "owner", status: "paused")).1.canResume, "an owner's pause resumes")
            let unfunded = make(nest(state: "paused", paused: "unfunded", status: "needs_you")).1
            expect(!unfunded.canResume && !unfunded.canPause, "unfunded: neither")
            let over = make(nest(state: "paused", paused: "over_capacity", status: "paused")).1
            expect(!over.canResume && !over.canPause, "over capacity: neither")
            expect(make(nest(state: "stopped")).1.canPause, "idle-stopped still pauses")
            expect(!make(nest(state: "creating", status: "unavailable")).1.canPause, "not while setting up")
            let explained = NestWords.pausedExplanation(nest(state: "paused", paused: "unfunded", status: "needs_you"))
            expect(explained?.contains("paid for") == true, "the reason is said")
            expect(NestWords.pausedExplanation(nest()) == nil, "nothing to explain when running")
        }
        // An operator of a company Nest uses it; managing is for its admins.
        do {
            let (_, store) = make(nest(role: "operator"))
            expect(!store.canManage && !store.canPause, "an operator does not manage")
            expect(make(nest(role: "owner")).1.canManage, "an admin does")
            expect(make(nest()).1.canManage, "a person's own Nest (no role in the list) does")
        }
        // Uploads: the limit is checked before the file is read.
        do {
            let (api, store) = make()
            var read = false
            await store.upload(name: "big.bin", size: NestDetailStore.maxUploadBytes + 1) { read = true; return Data() }
            expect(!read && api.log.isEmpty, "not read, not sent")
            expect(store.message?.contains("25 MB") == true, "and explained")
            await store.upload(name: "a.txt", size: 3) { Data("abc".utf8) }
            expect(api.log == ["upload:a.txt:3"], "within the limit: \(api.log)")
            expect(store.message == nil, "cleared")
            await store.upload(name: "empty.txt", size: 0) { Data() }
            expect(api.log.count == 1, "an empty file is not sent")
        }
        // Delete: only the exact name, then the page closes and the list refreshes.
        do {
            let (api, store) = make()
            await store.delete(typed: "helper")
            expect(api.log.isEmpty && !store.gone, "a mistyped name makes no call")
            expect(store.message?.contains("exactly") == true, "and says why")
            await store.delete(typed: " Helper ")
            expect(api.log == ["delete:Helper", "changed"], "trimmed, then the list refreshes: \(api.log)")
            expect(store.gone, "the page closes")
        }
        // A refused delete leaves the Nest where it was.
        do {
            let (api, store) = make()
            api.error = APIError(status: 502, code: "nestd_failed", detail: nil)
            await store.delete(typed: "Helper")
            expect(!store.gone && store.message?.contains("reached") == true, "nothing was deleted")
        }
        // Access ended elsewhere: the page and the header go away.
        do {
            let (api, store) = make()
            api.nestError = APIError(status: 404, code: "not_found", detail: nil)
            await store.refreshStatus()
            expect(store.gone, "a 404 ends the page")
        }
        // A blip is not the end of the Nest.
        do {
            let (api, store) = make()
            api.nestError = APIError(status: 503, code: "nests_unavailable", detail: nil)
            await store.refreshStatus()
            expect(!store.gone, "only a 404 is")
        }
        // Rename: what the postbox accepts, and the account's list follows.
        do {
            let (api, store) = make()
            await store.rename("   ")
            expect(api.log.isEmpty, "blank")
            await store.rename(String(repeating: "x", count: 65))
            expect(api.log.isEmpty, "too long")
            await store.rename("  New name ")
            expect(api.log == ["rename:New name", "changed"], "trimmed: \(api.log)")
        }
        // Export hands back bytes to save; a second request while busy is ignored by `busy`.
        do {
            let (api, store) = make()
            await store.export()
            expect(store.exported == Data("PK".utf8) && api.log == ["export"], "ready to save")
            store.clearExport()
            expect(store.exported == nil, "cleared after saving")
            expect(NestWords.exportName("Nest/../my: file") == "nest-Nestmyfile.zip", "the label is never a path: \(NestWords.exportName("Nest/../my: file"))")
            expect(NestWords.exportName("///") == "nest-export.zip", "an all-symbol label")
        }
        expect(NestWords.tokens(.init(used: 0, limit: 0)) == "No AI connected", "an Empty Nest has no allowance")
        expect(NestWords.status(nest(status: nil)) == "Checking…", "a row with no status yet")

        // The answers the postbox really sends (crates/pigeonpost-postbox/src/nest_*.rs).
        let files = decode(NestFiles.self, """
        {"files":[{"file_id":"nfl_1","name":"a.pdf","bytes":10,"sha256":"00","kind":"pdf","status":"partial","reason":null,
          "coverage":{"units_total":4,"units_text":3},"chars":100,"warnings":["page 2 empty"],"created_at":1}],
         "used_bytes":10,"quota_bytes":100}
        """)
        expect(files.files[0].coverage.unitsText == 3 && files.files[0].warnings == ["page 2 empty"] && files.quotaBytes == 100, "files")
        let answer = decode([String: [NestApproval]].self, """
        {"approvals":[{"approval_id":"nap_1","tool":"send_message","call":{"args":{"body":"hi","to":"/bekir/main"},"tool":"send_message"},
          "args_hash":"abc","status":"pending","created_at":1,"expires_at":2,"decided_at":null,"result":null}]}
        """)
        let decoded = answer["approvals"]![0]
        expect(decoded.argsHash == "abc" && decoded.isReviewable && decoded.call?.args.to == "/bekir/main", "a card")
        let empty = decode(NestUsage.self, """
        {"nest_id":"n","period":null,"input_tokens":{"used":null,"limit":null},"output_tokens":{"used":null,"limit":null},
         "requests":null,"running_secs":null,"files_bytes":0,"disk":null,"package":null}
        """)
        expect(empty.inputTokens.used == nil && empty.package == nil, "usage before the first run")
        let usage = decode(NestUsage.self, """
        {"nest_id":"n","period":"2026-11","input_tokens":{"used":5,"limit":1000},"output_tokens":{"used":1,"limit":100},
         "requests":2,"running_secs":3600,"files_bytes":7,"disk":{"used":1},
         "package":{"code":"nest.ai.starter.monthly","active_hours_pooled":10,"storage_bytes_per_nest":5368709120}}
        """)
        expect(usage.runningSecs == 3600 && usage.package?.activeHoursPooled == 10, "usage")
        let single = decode(NestView.self, """
        {"nest_id":"n","address":"/k/a","label":"L","state":"paused","funding":"package","org_id":"o","role":"operator",
         "status":"paused","status_reason":"over_capacity","paused_reason":"over_capacity","delete_after":null,"created_at":1}
        """)
        expect(single.role == "operator" && !single.isOwner && single.orgId == "o", "GET /v1/nests/{nest} carries the role")
        let snapshot = decode(NestSnapshot.self, """
        {"allowance":{"funding":{"entitlement_id":"e","source":"apple","package":"nest.ai.plus.monthly","status":"active",
          "paid_through":100,"cancel_requested_at":null,"pending_plan_code":null},"slots":3,"used":1,"duplicates":[]},
         "package":null,"nests":[],"setups":[{"setup_id":"nsu_1","entitlement_id":"e","label":"L","status":"pending","attempts":1,
          "next_attempt_at":2,"last_error":null,"nest_id":null}],
         "claimed":{"entitlement_id":"e","package":"nest.ai.plus.monthly","active":true,"duplicate":false}}
        """)
        expect(snapshot.allowance.slots == 3 && snapshot.setups.count == 1, "a claim answer is a snapshot")
        let org = decode(OrgNestList.self, """
        {"nests":[{"nest_id":"n","address":"/k/o","label":"HR","state":"running","role":"operator"}],
         "allowance":{"slots":2,"used":1},"package":null}
        """)
        expect(org.nests[0].view(org: "o1").orgId == "o1" && org.nests[0].view(org: "o1").role == "operator", "an org row")
        print("nest detail: \(checks) checks passed")
    }
}
