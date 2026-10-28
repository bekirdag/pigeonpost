// Live streams' group side and the Meeting notes reader, without a network or a page: the banner's
// words, the bridge's refusals, the go-live rule, the brief's shape and text, and the reader's store
// against a stand-in postbox and a stand-in notes key. Run by Tests/run.sh.
import Foundation

enum AuthError: Error { case sessionExpired }
@MainActor final class Session {
    func token() async throws -> String { "synthetic-token" }
    func renew() async throws -> String { "synthetic-renewed" }
}

final class Postbox: URLProtocol, @unchecked Sendable {
    nonisolated(unsafe) static var routes: [String: (Int, String)] = [:]
    nonisolated(unsafe) static var seen: [(String, String, Data)] = []
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "notes-fixture.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        var body = request.httpBody ?? Data()
        if body.isEmpty, let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable { let n = stream.read(&buffer, maxLength: buffer.count); if n <= 0 { break }; body.append(contentsOf: buffer.prefix(n)) }
        }
        let key = (request.httpMethod ?? "GET") + " " + (request.url?.path ?? "")
        Self.seen.append((key, request.value(forHTTPHeaderField: "x-pigeonpost-identity") ?? "", body))
        let (status, text) = Self.routes[key] ?? (404, #"{"error":"not_found"}"#)
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: ["content-type": "application/json"])!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(text.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
}

enum Refusal: Error { case code(String) }
@MainActor final class Keys: MeetingNotesKeys {
    var opened: [[String: Any]] = []
    func meetingNotesKey() async throws -> MeetingNotesKey { MeetingNotesKey(id: "nkDEVICE", publicKey: "BPUB", label: "iPhone or iPad app") }
    func openMeetingBrief(_ meeting: [String: Any]) async throws -> Data {
        opened.append(meeting)
        if meeting["id"] as? String == "mtg_other" { throw Refusal.code("no_key_for_device") }
        return Data(#"{"v":1,"title":"Planning","started_at":"2026-10-27T09:00:00.000Z","duration_s":1520,"participants":["/a","/b"],"summary":"Ship it.","decisions":["Thursday"],"action_items":[{"owner":"/a","task":"Fix","due":"Wed","quote_ts":null}],"open_questions":["Windows?"],"draft":false,"transcript":[{"at":"00:04","speaker":"/a","text":"Hi","lang":"en"}],"generated_by":{"stt":"whisper","llm":"qwen"}}"#.utf8)
    }
    func refusal(_ error: Error) -> String? { if case let Refusal.code(c)? = error as? Refusal { return c }; return nil }
}

@main @MainActor enum StreamNotesTests {
    static var checks = 0, failures = 0
    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if ok { print("  ok   " + what) } else { failures += 1; print("  FAIL " + what) }
    }

    static func main() async {
        streams()
        await notes()
        print("Streams and notes: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }

    static func streams() {
        let names: (String) -> String = { ["/k/jane": "Jane"][$0] ?? $0 }
        let live = GroupStream(id: "str_" + String(repeating: "a", count: 26), title: "  Sprint demo ", by: "/k/jane", state: "live")
        check(LiveStreams.bannerText(live, me: "/k/me", name: names) == "Jane is live: Sprint demo", "the banner names the streamer and the title")
        check(LiveStreams.bannerText(live, me: "/k/jane", name: names) == "You are live: Sprint demo", "your own stream says You")
        check(LiveStreams.bannerAction(live, me: "/k/me", current: nil) == "Watch", "others' streams offer Watch")
        check(LiveStreams.bannerAction(live, me: "/k/jane", current: nil) == "Open", "your own stream offers Open")
        check(LiveStreams.bannerAction(live, me: "/k/me", current: live.id) == "Return", "a stream this device is in offers Return")
        check(LiveStreams.validStream(live.id) && !LiveStreams.validStream("str_fixture0") && !LiveStreams.validGroup("grp_ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
              "ids are checked like the bridge checks them")
        check(LiveStreams.cleanTitle("  " + String(repeating: "x", count: 130)).count == 100, "titles are trimmed and bounded at 100")
        check(LiveStreams.explain(nil) == nil && LiveStreams.explain("stream_full") == nil, "nothing extra is said when the page already explained")
        check(LiveStreams.explain("needs_e2ee")?.contains("end to end") == true, "an engine without frame encryption is told so")
        check(LiveStreams.explain("busy") == "Finish the current call or stream first.", "one call or stream at a time")
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let wire = #"{"streams":[{"id":"str_aaaaaaaaaaaaaaaaaaaaaaaaaa","group_id":"grp_x","title":"T","sources":["camera"],"by":"/k/a","state":"live","started_at":1,"ends_by":2,"viewers":3,"stage":[{"address":"/k/b","state":"invited"}],"max_viewers":200,"revision":4,"future_field":true}]}"#
        let parsed = try? decoder.decode(GroupStreamsResponse.self, from: Data(wire.utf8))
        check(parsed?.streams?.first?.stage?.first?.state == "invited" && parsed?.streams?.first?.viewers == 3, "the postbox's stream decodes, unknown fields ignored")
        func group(_ role: String, _ policy: String?, state: String = "active") -> GroupSummary {
            GroupSummary(id: "grp_x", name: "G", streamPolicy: policy, my: GroupMembership(role: role, state: state))
        }
        check(group("admin", nil).canGoLive && group("owner", "admins").canGoLive, "admins may go live by default")
        check(!group("member", nil).canGoLive && group("member", "everyone").canGoLive, "members only when the group lets everyone")
        check(!group("restricted", "everyone").canGoLive && !group("admin", "everyone", state: "invited").canGoLive, "restricted and inactive members may not")
    }

    static func notes() async {
        URLProtocol.registerClass(Postbox.self)
        let tokens = Session()
        let client = PostboxClient(base: URL(string: "https://notes-fixture.invalid")!, tokens: tokens, retainingTokens: true)
        let keys = Keys()
        let store = MeetingNotesStore(client: { client }, keys: keys)
        Postbox.routes["GET /v1/meetings"] = (200, #"{"meetings":[{"id":"mtg_old","kind":"direct","state":"ready","created_at":10,"expires_at":99999999999,"host":"/a"},{"id":"mtg_new","kind":"group","state":"processing","created_at":20,"expires_at":99999999999,"host":"/b"}]}"#)
        await store.load()
        if case let .loaded(rows) = store.list { check(rows.map(\.id) == ["mtg_new", "mtg_old"], "the list loads, newest first") } else { check(false, "the list loads") }
        check(Postbox.seen.last?.1 == "", "meeting notes are the account's: no acting mailbox is named")
        Postbox.routes["PUT /v1/meetings/keys/nkDEVICE"] = (200, #"{"key_id":"nkDEVICE","registered":true}"#)
        await store.register()
        let put = Postbox.seen.last { $0.0 == "PUT /v1/meetings/keys/nkDEVICE" }
        let sent = put.flatMap { try? JSONSerialization.jsonObject(with: $0.2) } as? [String: Any]
        check(store.registered && sent?["public_key"] as? String == "BPUB" && sent?["label"] as? String == "iPhone or iPad app",
              "this device's notes key (public half) is registered for the account")
        Postbox.routes["GET /v1/meetings/mtg_ok"] = (200, #"{"id":"mtg_ok","kind":"group","state":"ready","created_at":1,"expires_at":99999999999,"host":"/a","alg":"A256GCM","iv":"aXY=","ciphertext":"Y3Q=","keys":[{"key_id":"nkDEVICE","epk":"E","iv":"I","wrapped":"W","extra":"x"}]}"#)
        await store.open("mtg_ok")
        guard case let .brief(_, brief)? = store.opened["mtg_ok"] else { check(false, "a ready brief opens"); return finish() }
        check(brief.title == "Planning" && brief.actionItems?.first?.owner == "/a" && brief.transcript?.count == 1, "a ready brief opens with this device's key")
        let handed = keys.opened.last
        check(handed?["ciphertext"] as? String == "Y3Q=" && (handed?["keys"] as? [[String: Any]])?.first?["extra"] == nil,
              "only the ciphertext and this account's wraps go to the page")
        check(MeetingNotesText.plain(brief) == "Planning\n\nShip it.\n\nDecisions:\n- Thursday\n\nAction items:\n- /a: Fix (due Wed)\n\nOpen questions:\n- Windows?\n\nTranscript:\n- [00:04] /a: Hi\n",
              "Copy as text matches the web's layout")
        check(MeetingNotesText.header(brief).hasSuffix("25 min · /a, /b"), "the header gives length and speakers")
        check(MeetingNotesText.generated(brief).contains("(whisper, qwen)"), "the footer names the engines")
        Postbox.routes["GET /v1/meetings/mtg_other"] = (200, #"{"id":"mtg_other","state":"ready","created_at":1,"expires_at":99999999999,"keys":[]}"#)
        await store.open("mtg_other")
        if case .otherDevices? = store.opened["mtg_other"] { check(true, "notes for the account's other devices say so") } else { check(false, "notes for the account's other devices say so") }
        Postbox.routes["GET /v1/meetings/mtg_wait"] = (200, #"{"id":"mtg_wait","state":"failed","failure":"no_speech","created_at":1,"expires_at":99999999999}"#)
        await store.open("mtg_wait")
        if case let .pending(m)? = store.opened["mtg_wait"] {
            check(MeetingNotesText.failure(m.failure) == "No speech was captured.", "a failed meeting gives the reason")
        } else { check(false, "a failed meeting gives the reason") }
        check(keys.opened.count == 2, "nothing is decrypted for a meeting that is not ready")
        await store.open("mtg_missing")
        check(store.opened["mtg_missing"] == .gone, "a removed or expired meeting reads as gone")
        Postbox.routes["DELETE /v1/meetings/mtg_old"] = (200, #"{"removed":true}"#)
        check(await store.delete("mtg_old"), "Delete my copy")
        if case let .loaded(rows) = store.list { check(!rows.contains { $0.id == "mtg_old" } && store.toast == "Deleted.", "the deleted copy leaves the list") }
        check(await store.delete("mtg_unknown") == false && store.toast == "Couldn't delete.", "a failed delete says so")
        let fixtures = MeetingNotesStore(client: { nil }, keys: nil, fixtures: true)
        await fixtures.load()
        await fixtures.open(MeetingNotesStore.fixtureReady)
        if case .brief? = fixtures.opened[MeetingNotesStore.fixtureReady] { check(true, "the screen fixtures work offline") } else { check(false, "the screen fixtures work offline") }
        finish()
    }
    static func finish() {}
}
