// Real Inbox + PostboxClient with a controlled URLSession transport. No credentials or server.
import Foundation
import Observation

@MainActor @Observable final class Session {
    enum Status: Equatable { case signedIn, signedOut }
    var status = Status.signedIn
    var subject: String? = "fixture-subject"
    var authorizationId = UUID()
    let accessLease = AuthorizationLease()
    func token() async throws -> String { "test-only" }
    func renew() async throws -> String { "test-only-renewed" }
}
enum AuthError: Error { case sessionExpired }
@MainActor @Observable final class Account {
    var me: Mailbox?
    var mailboxes: [Mailbox] = []
    var expired = false
    var actingGeneration = UUID()
    #if MAC_INBOX_UI_TESTS
    var push: PushService?
    #endif
    let session = Session()
    let client: PostboxClient
    #if MAC_INBOX_UI_TESTS
    // Nothing switched on: the Phase 1b surfaces stay out of the way of these checks.
    let features = Features()
    #endif
    var ownAgents: [Mailbox] { mailboxes.filter { $0.address != me?.address } }
    init() { client = PostboxClient(base: URL(string: "https://pigeonpost.test")!, tokens: session) }
    func sessionExpired() { expired = true }
    func act(as mailbox: Mailbox) { me = mailbox; actingGeneration = UUID() }
    func accessForActingMailbox() -> AuthorizationLease? { AuthorizationLease(parent: session.accessLease) }
    func scopedClient(acting: Bool = false) -> PostboxClient { client }
}
enum Fixtures {
    static let enabled = false
    static let quotaState: String? = nil
    static let openPeer: String? = nil
    static let sheet: String? = nil
}
#if !MAC_INBOX_UI_TESTS
@MainActor enum AppLife { static var isActive = true }
struct StagedFile { let name: String; let mediaType: String; let data: Data }
#endif

final class ControlledURLProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var pending: [ControlledURLProtocol] = []

    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.host == "pigeonpost.test"
    }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.lock.lock()
        Self.pending.append(self)
        Self.lock.unlock()
    }
    override func stopLoading() {
        Self.lock.lock()
        Self.pending.removeAll { $0 === self }
        Self.lock.unlock()
    }
    static func requests() -> [URLRequest] {
        lock.lock()
        defer { lock.unlock() }
        return pending.map(\.request)
    }
    static func identity(_ request: URLRequest) -> String? {
        URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?
            .queryItems?.first { $0.name == "identity" }?.value ?? request.value(forHTTPHeaderField: "x-pigeonpost-identity")
    }
    /// The connection drops with no answer at all — the postbox may or may not have stored it.
    @discardableResult
    static func drop(_ path: String) -> Bool {
        lock.lock()
        let index = pending.firstIndex { $0.request.url?.path == path }
        let call = index.map { pending.remove(at: $0) }
        lock.unlock()
        guard let call else { return false }
        call.client?.urlProtocol(call, didFailWithError: URLError(.networkConnectionLost))
        return true
    }
    @discardableResult
    static func complete(_ path: String, identity: String?, body: String, status: Int = 200, poll: Bool? = nil) -> Bool {
        lock.lock()
        let index = pending.firstIndex {
            let isPoll = URLComponents(url: $0.request.url!, resolvingAgainstBaseURL: false)?
                .queryItems?.contains { $0.name == "wait" } ?? false
            return $0.request.url?.path == path && Self.identity($0.request) == identity
                && (poll == nil || poll == isPoll)
        }
        let call = index.map { pending.remove(at: $0) }
        lock.unlock()
        guard let call else { return false }
        call.client?.urlProtocol(call, didReceive: HTTPURLResponse(
            url: call.request.url!, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"]
        )!, cacheStoragePolicy: .notAllowed)
        call.client?.urlProtocol(call, didLoad: Data(body.utf8))
        call.client?.urlProtocolDidFinishLoading(call)
        return true
    }
}

#if !MAC_INBOX_UI_TESTS
@main
#endif
@MainActor struct InboxLoadingTests {
    static var failures = 0
    static var checks = 0
    static let a = Mailbox(address: "/k/test-a", handle: "/test/a", label: "A")
    static let b = Mailbox(address: "/k/test-b", handle: "/test/b", label: "B")

    static func check(_ condition: @autoclosure () -> Bool, _ label: String) {
        checks += 1
        if condition() { print("  ok   \(label)") }
        else { failures += 1; print("  FAIL \(label)") }
    }
    static func wait(_ label: String, until condition: () -> Bool) async {
        let deadline = Date().addingTimeInterval(3)
        while !condition(), Date() < deadline {
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        guard condition() else { fatalError("Timed out: \(label)") }
    }
    static func pending(_ identity: String, count: Int) async {
        await wait("\(count) requests for \(identity)") {
            ControlledURLProtocol.requests().filter { ControlledURLProtocol.identity($0) == identity }.count >= count
        }
    }
    static func make() -> (Account, Inbox) {
        let account = Account()
        account.me = a
        account.mailboxes = [a, b]
        return (account, Inbox(account: account))
    }
    static func switchTo(_ mailbox: Mailbox, account: Account, inbox: Inbox) {
        inbox.reset()
        account.me = mailbox
    }
    static func messages(_ marker: String) -> String {
        """
        {"messages":[{"message_id":"\(marker)","from":"/k/peer","peer":"/test/peer","body":"\(marker)","direction":"in","read":true,"received_at":1}],"policy":{"accept_all":true,"auto_accept_known":false}}
        """
    }
    static func finish(_ identity: String, marker: String, inboxStatus: Int = 200, empty: Bool = false, inboxBody: String? = nil) {
        let bodies = [
            "/v1/inbox": inboxStatus == 200 ? (empty ? "{\"messages\":[]}" : inboxBody ?? messages(marker)) : "{\"error\":\"unavailable\"}",
            "/v1/contacts": empty ? "{\"contacts\":[]}" : "{\"contacts\":[{\"peer\":\"/test/peer\",\"alias\":\"\(marker)\",\"admission\":\"allow\",\"autonomy\":\"review\",\"allowed_verbs\":[]}],\"vocabulary\":{\"grantable\":[\"\(marker)\"]}}",
            "/v1/threads": empty ? "{\"threads\":[]}" : "{\"threads\":[{\"thread_id\":\"\(marker)\",\"peer\":\"/test/peer\",\"title\":\"\(marker)\",\"is_default\":true}]}",
            "/v1/archive": "{\"archived\":[\"/test/\(marker)\"]}",
            "/v1/quota": "{\"used_bytes\":7,\"limit_bytes\":20,\"warn_at_bytes\":16,\"tier\":\"\(marker)\"}"
        ]
        for (path, body) in bodies {
            check(ControlledURLProtocol.complete(path, identity: identity, body: body,
                  status: path == "/v1/inbox" ? inboxStatus : 200, poll: false), "answered \(path) for \(marker)")
        }
    }

    static func main() async {
        URLProtocol.registerClass(ControlledURLProtocol.self)
        await messageActionWireAndAcknowledgement()
        await editRetryAndMailboxIsolation()
        await attachmentSendRetry()
        await startedConversationOpensWhereTheServerFiledIt()
        await unconfirmedFirstSendOpensWithRetryAndDiscard()
        await unconfirmedFirstSendThatWasStoredIsConfirmed()
        await refusedFirstSendStaysInTheDialog()
        await startedConversationStaysWithItsMailbox()
        await newConversationTargetCannotFollowAChangedLogin()
        await initialLoadAndSwitch()
        await oldLoadCannotFinishNewSpinner()
        await rapidReturnToSameMailbox()
        await cancelledLoad()
        await failedLoadAndRetry()
        await oldPollIsIgnored()
        await oldUnauthorizedPollIsIgnored()
        #if !MAC_INBOX_UI_TESTS
        await backgroundUnreadAndThreadAlerts()
        #endif
        print("Inbox loading: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }

    static func requestBody(_ request: URLRequest) -> [String: Any] {
        var bytes = request.httpBody ?? Data()
        if bytes.isEmpty, let stream = request.httpBodyStream {
            stream.open()
            defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 1024)
            while true {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                bytes.append(contentsOf: buffer.prefix(count))
            }
        }
        return (try? JSONSerialization.jsonObject(with: bytes)) as? [String: Any] ?? [:]
    }

    static func messageActionWireAndAcknowledgement() async {
        let (account, inbox) = make()
        let loading = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        let request = ControlledURLProtocol.requests().first { $0.url?.path == "/v1/inbox" }!
        let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems!
        check(query.contains { $0.name == "actions" && $0.value == "1" }
              && query.contains { $0.name == "fold" && $0.value == "1" },
              "native history requests folded action metadata")
        let reference = "mref_" + String(repeating: "a", count: 26)
        let wire = """
        {"messages":[
          {"message_id":"root","peer":"/test/peer","body":"edited words","direction":"in","read":false,"received_at":1,
           "msg_ref":"\(reference)","edited_at":2,"edit_count":1,"reply_to":{"msg_ref":"parent","message_id":"parent-copy","quote":"remember this"}},
          {"message_id":"sent","peer":"/test/peer","body":"my words","direction":"out","sent_at":3,
           "receipt":{"state":"seen","delivered_at":4,"seen_at":5},"agent_status":{"state":"working","at":6}}
        ]}
        """
        finish(a.address, marker: "actions", inboxBody: wire)
        await loading.value
        check(inbox.messages.first?.msgRef == reference && inbox.messages.first?.editCount == 1,
              "folded original retains its identity and edit count")
        let sent = inbox.conversation(with: "/test/peer")?.messages.first { $0.id == "sent" }
        check(sent?.receipt?.state == "seen" && sent?.agentStatus?.state == "working",
              "conversation assembly preserves sent receipts and agent progress")
        await inbox.acknowledge(peer: "/test/peer", subthread: nil)
        await wait("acknowledgement") { ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/ack" } }
        let ack = ControlledURLProtocol.requests().first { $0.url?.path == "/v1/ack" }!
        let ackBody = requestBody(ack)
        check(ackBody["with_edits"] as? Bool == true && ackBody["identity"] as? String == a.address,
              "reading the folded message acknowledges its versions in the captured mailbox")
        check(ControlledURLProtocol.complete("/v1/ack", identity: nil, body: "{}"), "answered folded acknowledgement")
        await wait("ack transport drained") { ControlledURLProtocol.requests().isEmpty }
        let root = inbox.messages.first { $0.messageId == "root" }
        check(root?.isRead == true && root?.replyTo?.text == "remember this" && root?.editedAt == 2,
              "local read marking preserves reply and edit metadata")

        let sending = Task { try await account.client.sendMessage(
            from: a.address, to: "/test/peer", body: "answer", threadId: "subject",
            msgRef: reference, replyTo: "parent", quote: String(repeating: "ş", count: 240)) }
        await wait("outgoing reply") { ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/send" } }
        let outgoing = ControlledURLProtocol.requests().first { $0.url?.path == "/v1/send" }!
        let body = requestBody(outgoing)
        check(body["from"] as? String == a.address && body["msg_ref"] as? String == reference
              && body["reply_to"] as? String == "parent", "reply carries stable reference and mailbox scope")
        check((body["quote"] as? String)?.count == 200, "reply quote is bounded by characters, including Turkish")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{}", status: 401), "reply requires token renewal")
        await wait("reply retry after renewal") { ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/send" } }
        let retry = ControlledURLProtocol.requests().first { $0.url?.path == "/v1/send" }!
        let retried = requestBody(retry)
        check(retried["msg_ref"] as? String == reference && retried["reply_to"] as? String == "parent"
              && retried["body"] as? String == "answer", "authentication retry keeps the exact message action")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil,
              body: "{\"message_id\":\"delivered\",\"sent_copy_id\":\"own\",\"msg_ref\":\"\(reference)\"}"), "answered reply retry")
        do {
            let response = try await sending.value
            check(response.sentCopyId == "own" && response.msgRef == reference, "send response exposes shared reference")
        } catch { check(false, "reply should complete after renewal") }
    }

    static func actionHistory(_ marker: String, text: String = "original") -> String {
        let request: [String: Any] = ["v": 1, "verb": "review", "args": ["task": text, "nested": ["retain": true]], "note": "independent note"]
        let body = String(data: try! JSONSerialization.data(withJSONObject: request), encoding: .utf8)!
        let message: [String: Any] = ["message_id": marker, "from": a.address, "peer": "/test/peer",
            "body": body, "direction": "out", "sent_at": Int(Date().timeIntervalSince1970) - 10,
            "thread_id": "subject", "msg_ref": "mref_" + String(repeating: "a", count: 26)]
        return String(data: try! JSONSerialization.data(withJSONObject: ["messages": [message]]), encoding: .utf8)!
    }

    static func nextSend() async -> [String: Any] {
        await wait("message action POST") { ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/send" } }
        return requestBody(ControlledURLProtocol.requests().first { $0.url?.path == "/v1/send" }!)
    }

    static func editRetryAndMailboxIsolation() async {
        let (account, inbox) = make()
        let loading = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        finish(a.address, marker: "root", inboxBody: actionHistory("root"))
        await loading.value
        let scope = MessageActionScope(mailbox: a.address, peer: "/test/peer", subject: nil)
        let root = inbox.conversation(with: scope.peer)!.messages.first!
        inbox.beginMessageAction(.edit, message: root, in: scope)
        let actionID = inbox.action(in: scope)!.id
        inbox.updateMessageAction(actionID, text: "new words")
        let initial = inbox.action(in: scope)!
        let first = Task { await inbox.saveMessageEdit(initial) }
        let body = await nextSend()
        check(body["edit_of"] as? String == root.reference && body["from"] as? String == a.address,
              "edit names the original reference and captured mailbox")
        let content = (try! JSONSerialization.jsonObject(with: Data((body["body"] as! String).utf8))) as! [String: Any]
        let args = content["args"] as! [String: Any]
        check(content["verb"] as? String == "review" && args["task"] as? String == "new words"
              && (args["nested"] as? [String: Bool])?["retain"] == true && content["note"] as? String == "independent note",
              "editing preserves the request verb, nested arguments and independent note")
        check(inbox.actionSending, "edit visibly waits for the postbox")
        let duplicate = await inbox.saveMessageEdit(inbox.action(in: scope)!)
        check(!duplicate && ControlledURLProtocol.requests().filter { $0.url?.path == "/v1/send" }.count == 1,
              "double save cannot create a second concurrent edit")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"error\":\"unavailable\"}", status: 503), "failed first edit")
        let firstResult = await first.value
        check(!firstResult && !inbox.actionSending && inbox.action(in: scope)?.text == "new words",
              "failed edit keeps the draft and unlocks retry")
        let retryAction = inbox.action(in: scope)!
        let retry = Task { await inbox.saveMessageEdit(retryAction) }
        let retryBody = await nextSend()
        check(retryBody["msg_ref"] as? String == body["msg_ref"] as? String,
              "unchanged edit retry keeps its stable idempotency reference")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"error\":\"unavailable\"}", status: 503), "failed retry")
        _ = await retry.value
        inbox.updateMessageAction(actionID, text: "revised again")
        let revisedAction = inbox.action(in: scope)!
        let revised = Task { await inbox.saveMessageEdit(revisedAction) }
        let revisedBody = await nextSend()
        check(revisedBody["msg_ref"] as? String != body["msg_ref"] as? String,
              "changed edit text receives a new reference")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"message_id\":\"edit\",\"sent_copy_id\":\"edit-own\"}", status: 201), "saved revised edit")
        await pending(a.address, count: 1)
        check(ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: actionHistory("root", text: "revised again")), "reloaded folded history")
        let revisedResult = await revised.value
        check(revisedResult && inbox.composerAction == nil && !inbox.actionSending,
              "successful edit clears its action after refreshing the captured mailbox")

        let current = inbox.conversation(with: scope.peer)!.messages.first!
        inbox.beginMessageAction(.edit, message: current, in: scope)
        let oldAction = inbox.action(in: scope)!
        let old = Task { await inbox.saveMessageEdit(oldAction) }
        _ = await nextSend()
        switchTo(b, account: account, inbox: inbox)
        let newLoad = Task { await inbox.loadAll() }
        await pending(b.address, count: 5)
        finish(b.address, marker: "b-root", inboxBody: actionHistory("b-root"))
        await newLoad.value
        let bScope = MessageActionScope(mailbox: b.address, peer: "/test/peer", subject: nil)
        inbox.beginMessageAction(.edit, message: inbox.conversation(with: bScope.peer)!.messages.first!, in: bScope)
        let bActionID = inbox.action(in: bScope)!.id
        inbox.updateMessageAction(bActionID, text: "B draft")
        let bAction = inbox.action(in: bScope)!
        let bEdit = Task { await inbox.saveMessageEdit(bAction) }
        await wait("two mailbox sends") { ControlledURLProtocol.requests().filter { $0.url?.path == "/v1/send" }.count == 2 }
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"message_id\":\"late-a\"}", status: 201), "completed old mailbox edit")
        _ = await old.value
        check(inbox.action(in: bScope)?.id == bActionID && inbox.action(in: bScope)?.text == "B draft" && inbox.actionSending,
              "late A success cannot clear B's action, draft or in-flight state")
        check(!ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/inbox" }, "late edit does not reload another mailbox")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"error\":\"unavailable\"}", status: 503), "failed current B edit")
        _ = await bEdit.value
        check(!inbox.actionSending && inbox.action(in: bScope)?.text == "B draft", "B retains its own failed edit")
        inbox.cancelMessageAction(bActionID)

        let oldSend = Task { await inbox.send("old B draft", to: "/test/peer", threadId: nil) }
        _ = await nextSend()
        switchTo(a, account: account, inbox: inbox)
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"error\":\"unavailable\"}", status: 503), "failed stale ordinary send")
        _ = await oldSend.value
        check(inbox.toast == nil && inbox.pending.isEmpty, "late send failure cannot report against another mailbox")
    }

    static func attachmentSendRetry() async {
        let (account, inbox) = make()
        let file = StagedFile(name: "brief.txt", mediaType: "text/plain", data: Data("hello attachment".utf8))
        let reference = MessageReference.make()
        let first = Task { await inbox.send("with file", to: "/test/peer", threadId: "subject", files: [file], msgRef: reference) }
        await wait("attachment upload") { ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/attachments" } }
        check(ControlledURLProtocol.complete("/v1/attachments", identity: a.address,
              body: "{\"id\":\"file-one\",\"filename\":\"brief.txt\",\"media_type\":\"text/plain\",\"bytes\":16}", status: 201), "uploaded the file once")
        let body = await nextSend()
        check(body["attachments"] as? [String] == ["file-one"], "send names the uploaded attachment")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"error\":\"unavailable\"}", status: 503), "simulated an ambiguous file-send response")
        _ = await first.value
        let retry = Task { await inbox.send("with file", to: "/test/peer", threadId: "subject", files: [file], msgRef: reference) }
        let retried = await nextSend()
        check(!ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/attachments" }, "file retry reuses upload instead of changing the wire attachment ids")
        check(retried["attachments"] as? [String] == body["attachments"] as? [String]
              && retried["body"] as? String == body["body"] as? String
              && retried["msg_ref"] as? String == body["msg_ref"] as? String,
              "ambiguous retry keeps the exact body, attachments and reference")
        check(inbox.pending.count == 1, "retry replaces its failed optimistic row")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"error\":\"unavailable\"}", status: 503), "failed the repeated file send")
        _ = await retry.value
        switchTo(b, account: account, inbox: inbox)
        switchTo(a, account: account, inbox: inbox)
        let fresh = Task { await inbox.send("with file", to: "/test/peer", threadId: "subject", files: [file], msgRef: reference) }
        let afterReturn = await nextSend()
        check(!ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/attachments" }
              && afterReturn["attachments"] as? [String] == ["file-one"], "returning to the same mailbox preserves the exact send attempt's upload ids")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"message_id\":\"delivered\",\"sent_copy_id\":\"sent-copy\"}", status: 201), "accepted the fresh file send")
        let success = await fresh.value
        await pending(a.address, count: 1)
        check(ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: actionHistory("sent-copy")), "refreshed file send history")
        check(success, "fresh file send succeeds")
        await wait("file send refresh drained") { ControlledURLProtocol.requests().isEmpty }
        inbox.reset()
    }

    // ---- New conversation: the thread opens with its first message in it ---------------------

    static func startedConversationOpensWhereTheServerFiledIt() async {
        let (_, inbox) = make()
        let start = Task { await inbox.startConversation(to: "/test", body: "  first words  ") }
        let body = await nextSend()
        let reference = body["msg_ref"] as? String ?? ""
        check(body["to"] as? String == "/test" && reference.hasPrefix("mref_"), "first send goes to the typed address with a reference")
        check(inbox.conversation(with: "/test")?.messages.last?.status == .sending,
              "the first message is in its conversation while the postbox answers")
        check(inbox.started == nil, "nothing opens before the postbox has answered")
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"message_id\":\"m1\",\"sent_copy_id\":\"copy1\"}", status: 201),
              "postbox accepted the first send")
        await wait("started named") { inbox.started != nil }
        check(inbox.started?.selection(from: nil, reading: a.address) == "/test", "a successful first send opens its conversation at once")
        check(inbox.started?.selection(from: nil, reading: b.address) == nil, "another mailbox's window does not open it")
        check(inbox.takeStartedFocus(mailbox: b.address, peer: "/test") == false, "another mailbox's composer does not take focus")
        // The server filed it under the mailbox the namespace resolved to.
        let listing = """
        {"messages":[{"message_id":"copy1","direction":"out","from":"/k/test-a","peer":"/k/resolved","peer_handle":"/test/agent","body":"first words","msg_ref":"\(reference)","sent_at":5,"read":true}]}
        """
        await pending(a.address, count: 1)
        while ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: listing, poll: false) {}
        let outcome = await start.value
        check(outcome == .opened("/test/agent"), "the conversation is the one the server filed it under")
        check(inbox.started?.replacing == "/test" && inbox.started?.peer == "/test/agent", "the open conversation moves with it")
        check(inbox.started?.selection(from: "/test", reading: a.address) == "/test/agent", "a window still on the typed address follows")
        check(inbox.started?.selection(from: "/test/other", reading: a.address) == nil, "a window that moved on stays where it is")
        check(inbox.conversation(with: "/test/agent")?.messages.map(\.id) == ["copy1"], "the sent message is in the opened conversation once")
        check(inbox.takeStartedFocus(mailbox: a.address, peer: "/test/agent"), "its composer takes focus")
        check(!inbox.takeStartedFocus(mailbox: a.address, peer: "/test/agent"), "only once")
        await wait("first send drained") { ControlledURLProtocol.requests().isEmpty }
        inbox.reset()
    }

    static func unconfirmedFirstSendOpensWithRetryAndDiscard() async {
        let (_, inbox) = make()
        let start = Task { await inbox.startConversation(to: "/test/new", body: "lost words") }
        let body = await nextSend()
        check(ControlledURLProtocol.drop("/v1/send"), "the answer to the first send was lost")
        await pending(a.address, count: 1)
        check(ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: "{\"messages\":[]}", poll: false),
              "the listing does not show it stored")
        let outcome = await start.value
        check(outcome == .opened("/test/new"), "an unconfirmed first send still opens its conversation")
        check(inbox.started?.selection(from: nil, reading: a.address) == "/test/new", "and selects it")
        let row = inbox.conversation(with: "/test/new")?.messages.last
        check(row?.status == .failed && row?.body.contains("lost words") == true, "the thread shows the message as not sent")
        check(inbox.takeUnconfirmedFirstSend(mailbox: b.address, peer: "/test/new") == nil, "another mailbox cannot take its words")
        check(inbox.takeUnconfirmedFirstSend(mailbox: a.address, peer: "/test/new", intoEmptyComposer: false) == nil,
              "an occupied composer cannot consume the recovery record")
        check(inbox.unconfirmedFirstSends.count == 1, "the first send remains available after a draft collision")
        let retry = inbox.takeUnconfirmedFirstSend(mailbox: a.address, peer: "/test/new")
        check(retry?.text == "lost words" && retry?.reference == body["msg_ref"] as? String,
              "its words and reference wait for the composer, so Send retries the same message")
        check(inbox.takeUnconfirmedFirstSend(mailbox: a.address, peer: "/test/new") == nil, "once")
        // Retry is the composer's Send with that reference: the same message, replacing the row.
        let again = Task { await inbox.send("lost words", to: "/test/new", threadId: nil, msgRef: retry?.reference ?? "") }
        let resent = await nextSend()
        check(resent["msg_ref"] as? String == body["msg_ref"] as? String && resent["body"] as? String == body["body"] as? String,
              "retry repeats the exact first message")
        check(ControlledURLProtocol.drop("/v1/send"), "the retry is lost too")
        _ = await again.value
        check(inbox.pending.count == 1, "a retry replaces the unsent row instead of adding one")
        // Discard: the row was never confirmed, so it goes from this device only.
        await inbox.deleteMessage(id: inbox.pending[0].id)
        check(inbox.pending.isEmpty && inbox.conversation(with: "/test/new") == nil, "discard removes the unsent message")
        check(inbox.resolvedFirstSends.contains(retry?.reference ?? ""), "discard also retires an already-adopted composer retry")
        check(!ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/messages" || $0.httpMethod == "DELETE" },
              "discarding an unsent message asks the postbox nothing")
        inbox.reset()
    }

    static func unconfirmedFirstSendThatWasStoredIsConfirmed() async {
        let (_, inbox) = make()
        let start = Task { await inbox.startConversation(to: "/test/new", body: "stored words") }
        let body = await nextSend()
        check(ControlledURLProtocol.drop("/v1/send"), "the answer was lost after the postbox stored it")
        await pending(a.address, count: 1)
        let listing = """
        {"messages":[{"message_id":"copy9","direction":"out","from":"/k/test-a","peer":"/test/new","body":"stored words","msg_ref":"\(body["msg_ref"] as? String ?? "")","sent_at":5,"read":true}]}
        """
        check(ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: listing, poll: false), "the listing shows it stored")
        let outcome = await start.value
        check(outcome == .opened("/test/new"), "the conversation opens")
        check(inbox.conversation(with: "/test/new")?.messages.map(\.id) == ["copy9"] && inbox.pending.isEmpty,
              "the stored copy replaces the not-sent row")
        check(inbox.unconfirmedFirstSends.isEmpty, "and nothing waits to be retried")
        check(inbox.resolvedFirstSends.contains(body["msg_ref"] as? String ?? ""), "server confirmation retires any adopted retry")
        inbox.reset()
    }

    static func refusedFirstSendStaysInTheDialog() async {
        let (_, inbox) = make()
        let start = Task { await inbox.startConversation(to: "/nobody/here", body: "hello?") }
        _ = await nextSend()
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"error\":\"recipient_unresolved\"}", status: 404),
              "the postbox refused the address")
        let outcome = await start.value
        check(outcome == .failed("No mailbox at that address."), "a refusal is said in the dialog")
        check(inbox.started == nil && inbox.conversation(with: "/nobody/here") == nil && inbox.pending.isEmpty,
              "and leaves no conversation behind")
        check(inbox.unconfirmedFirstSends.isEmpty, "nothing waits to be retried")
        inbox.reset()
    }

    static func startedConversationStaysWithItsMailbox() async {
        let (account, inbox) = make()
        let start = Task { await inbox.startConversation(to: "/test/new", body: "from A") }
        _ = await nextSend()
        switchTo(b, account: account, inbox: inbox)
        check(ControlledURLProtocol.complete("/v1/send", identity: nil, body: "{\"message_id\":\"m1\",\"sent_copy_id\":\"copy1\"}", status: 201),
              "the first send is answered after a switch to another mailbox")
        let outcome = await start.value
        check(outcome == .failed(nil) && inbox.started == nil, "the answer opens nothing in the mailbox now on screen")
        check(inbox.conversation(with: "/test/new") == nil, "and puts nothing in it")
        // A lost answer after a switch leaves no retry for the other mailbox either.
        switchTo(a, account: account, inbox: inbox)
        let lost = Task { await inbox.startConversation(to: "/test/new", body: "from A again") }
        _ = await nextSend()
        switchTo(b, account: account, inbox: inbox)
        check(ControlledURLProtocol.drop("/v1/send"), "the answer was lost after the switch")
        let lostOutcome = await lost.value
        check(lostOutcome == .failed(nil) && inbox.unconfirmedFirstSends.isEmpty && inbox.started == nil,
              "an unconfirmed send is not reopened in another mailbox")
        await wait("switch drained") { ControlledURLProtocol.requests().allSatisfy { $0.url?.path != "/v1/send" } }
        inbox.reset()
        account.me = a
    }

    static func newConversationTargetCannotFollowAChangedLogin() async {
        let (account, inbox) = make()
        let beforeLogin = inbox.captureNewConversationTarget()!
        account.session.authorizationId = UUID()
        let loginResult = await inbox.startConversation(target: beforeLogin, to: "/test/new", body: "private draft")
        if case .opened = loginResult { check(false, "a prior login must not send") }
        check(!inbox.isCurrent(beforeLogin) && ControlledURLProtocol.requests().isEmpty,
              "same-mailbox replacement login cannot submit a retained dialog")
        let beforeSwitch = inbox.captureNewConversationTarget()!
        account.act(as: b)
        account.act(as: a)
        _ = await inbox.startConversation(target: beforeSwitch, to: "/test/new", body: "private draft")
        check(!inbox.isCurrent(beforeSwitch) && ControlledURLProtocol.requests().isEmpty,
              "a mailbox round trip invalidates the dialog even before inbox reset runs")
        let beforeReset = inbox.captureNewConversationTarget()!
        inbox.reset()
        check(!inbox.isCurrent(beforeReset), "reset invalidates the dialog")
        let current = inbox.captureNewConversationTarget()!
        check(inbox.isCurrent(current), "a freshly opened dialog is usable")
        check(!Inbox(account: account).isCurrent(current), "another inbox cannot adopt the dialog")
    }

    static func initialLoadAndSwitch() async {
        let (account, inbox) = make()
        let old = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        check(inbox.loading && !inbox.hasLoaded, "initial load is visibly pending")
        switchTo(b, account: account, inbox: inbox)
        let new = Task { await inbox.loadAll() }
        await pending(b.address, count: 5)
        finish(b.address, marker: "new")
        await new.value
        check(!inbox.loading && inbox.hasLoaded, "new inbox finishes without waiting for the old request")
        finish(a.address, marker: "old")
        await old.value
        check(inbox.messages.first?.messageId == "new", "late old messages cannot replace the selected inbox")
        check(inbox.contacts.first?.alias == "new", "late old contacts cannot replace the selected inbox")
        check(inbox.serverThreads.first?.threadId == "new", "late old threads cannot replace the selected inbox")
        check(inbox.archived == ["/test/new"], "late old archive cannot hide selected conversations")
        check(inbox.quota?.tier == "new", "late old quota cannot replace the selected inbox")
        check(inbox.announcement == nil, "old mailbox history is not announced as newly arrived mail")
        inbox.reset()
        check(inbox.quota == nil && inbox.policy == nil && inbox.vocabulary == nil,
              "switch clears quota, policy and vocabulary as well as messages")
    }

    #if !MAC_INBOX_UI_TESTS
    static func backgroundUnreadAndThreadAlerts() async {
        let (_, inbox) = make()
        let load = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        finish(a.address, marker: "baseline", empty: true)
        await load.value
        inbox.reading = "/test/peer"
        inbox.readingSubthread = "a"
        var delivered = 0
        inbox.onArrival = { delivered += $0.count }
        AppLife.isActive = false
        let poll = Task { await inbox.live() }
        await pending(a.address, count: 1)
        let body = """
        {"messages":[{"message_id":"background","from":"/test/peer","peer":"/test/peer","thread_id":"a","body":"background","read":false,"received_at":1}]}
        """
        ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: body, poll: true)
        await wait("background delivery") { delivered == 1 }
        await inbox.acknowledge(peer: "/test/peer", subthread: "a")
        check(inbox.unreadCount == 1, "background selected-thread arrival stays unread")
        check(!ControlledURLProtocol.requests().contains { $0.url?.path == "/v1/ack" }, "background window sends no acknowledgement")
        await pending(a.address, count: 1)
        ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: body, poll: true)
        await pending(a.address, count: 1)
        check(delivered == 1, "repeated listing does not repeat desktop alert")
        AppLife.isActive = true
        let otherThread = body.replacingOccurrences(of: "background", with: "other-thread").replacingOccurrences(of: "\"thread_id\":\"a\"", with: "\"thread_id\":\"b\"")
        ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: otherThread, poll: true)
        await wait("hidden subject announcement") { inbox.announcement != nil }
        check(inbox.announcement?.id == "other-thread", "another subject in the selected conversation is announced")
        poll.cancel()
        await poll.value
        inbox.reset()
        check(inbox.readingSubthread == nil && inbox.unreadCount == 0, "mailbox reset clears reading scope and count")
    }
    #endif

    static func oldLoadCannotFinishNewSpinner() async {
        let (account, inbox) = make()
        let old = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        switchTo(b, account: account, inbox: inbox)
        let new = Task { await inbox.loadAll() }
        await pending(b.address, count: 5)
        finish(a.address, marker: "old", inboxStatus: 503)
        await old.value
        check(inbox.loading && !inbox.hasLoaded, "old load cannot stop the new mailbox's spinner")
        check(!inbox.offline && inbox.toast == nil, "old failure is not reported against the new mailbox")
        finish(b.address, marker: "new")
        await new.value
    }

    static func rapidReturnToSameMailbox() async {
        let (account, inbox) = make()
        let old = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        switchTo(b, account: account, inbox: inbox)
        switchTo(a, account: account, inbox: inbox)
        finish(a.address, marker: "old-visit")
        await old.value
        check(inbox.messages.isEmpty && !inbox.hasLoaded, "A → B → A rejects responses from A's previous visit")
        let fresh = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        finish(a.address, marker: "fresh-visit")
        await fresh.value
        check(inbox.messages.first?.messageId == "fresh-visit", "returning to A loads current data")
    }

    static func cancelledLoad() async {
        let (_, inbox) = make()
        let task = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        task.cancel()
        await task.value
        // URLSession delivers stopLoading on its own queue, potentially after
        // data(for:) has resumed with cancellation. Drain those transports before
        // another load of the same mailbox counts or answers its requests.
        await wait("cancelled load transports stopped") {
            ControlledURLProtocol.requests().isEmpty
        }
        check(!inbox.loading && !inbox.hasLoaded, "cancelled load ends loading without claiming success")
        check(!inbox.offline && inbox.toast == nil, "navigation cancellation is not an offline error")
    }

    static func failedLoadAndRetry() async {
        let (_, inbox) = make()
        let failed = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        finish(a.address, marker: "failed", inboxStatus: 503)
        await failed.value
        check(!inbox.loading && !inbox.hasLoaded && inbox.offline,
              "failed first load is a retryable failure, not a successfully empty inbox")
        let retry = Task { await inbox.loadAll() }
        await pending(a.address, count: 5)
        check(inbox.loading, "retry shows loading feedback")
        finish(a.address, marker: "recovered")
        await retry.value
        check(inbox.hasLoaded && !inbox.loading && !inbox.offline, "retry restores a ready connected inbox")
    }

    static func oldPollIsIgnored() async {
        let (account, inbox) = make()
        let poll = Task { await inbox.live() }
        await pending(a.address, count: 1)
        switchTo(b, account: account, inbox: inbox)
        ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: messages("stale-poll"))
        try? await Task.sleep(nanoseconds: 40_000_000)
        check(inbox.messages.isEmpty && !inbox.hasLoaded, "old long-poll response cannot populate the new inbox")
        check(!ControlledURLProtocol.requests().contains { ControlledURLProtocol.identity($0) == b.address },
              "old live task exits instead of silently migrating to the new inbox")
        poll.cancel()
        await poll.value
        await wait("cancelled stale poll transport stopped") {
            ControlledURLProtocol.requests().isEmpty
        }
    }

    static func oldUnauthorizedPollIsIgnored() async {
        let (account, inbox) = make()
        let poll = Task { await inbox.live() }
        await pending(a.address, count: 1)
        switchTo(b, account: account, inbox: inbox)
        ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: "{}", status: 401)
        await pending(a.address, count: 1)
        ControlledURLProtocol.complete("/v1/inbox", identity: a.address, body: "{}", status: 401)
        await poll.value
        check(!account.expired, "late authorization failure from the old mailbox cannot sign out the new one")
    }
}
