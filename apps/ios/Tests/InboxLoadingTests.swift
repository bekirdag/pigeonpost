// Real Inbox + PostboxClient with a controlled URLSession transport. No credentials or server.
import Foundation
import Observation

@MainActor final class Session {
    func token() async throws -> String { "test-only" }
    func renew() async throws -> String { "test-only-renewed" }
}
enum AuthError: Error { case sessionExpired }
@MainActor @Observable final class Account {
    var me: Mailbox?
    var mailboxes: [Mailbox] = []
    var expired = false
    #if MAC_INBOX_UI_TESTS
    var push: PushService?
    #endif
    let session = Session()
    let client: PostboxClient
    var ownAgents: [Mailbox] { mailboxes.filter { $0.address != me?.address } }
    init() { client = PostboxClient(base: URL(string: "https://pigeonpost.test")!, tokens: session) }
    func sessionExpired() { expired = true }
    func act(as mailbox: Mailbox) { me = mailbox }
}
enum Fixtures {
    static let enabled = false
    static let quotaState: String? = nil
}
#if !MAC_INBOX_UI_TESTS
enum AppLife { static let isActive = true }
#endif
struct StagedFile { let data: Data; let name: String; let mediaType: String }

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
            .queryItems?.first { $0.name == "identity" }?.value
    }
    @discardableResult
    static func complete(_ path: String, identity: String, body: String, status: Int = 200, poll: Bool? = nil) -> Bool {
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
    static func finish(_ identity: String, marker: String, inboxStatus: Int = 200, empty: Bool = false) {
        let bodies = [
            "/v1/inbox": inboxStatus == 200 ? (empty ? "{\"messages\":[]}" : messages(marker)) : "{\"error\":\"unavailable\"}",
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
        await initialLoadAndSwitch()
        await oldLoadCannotFinishNewSpinner()
        await rapidReturnToSameMailbox()
        await cancelledLoad()
        await failedLoadAndRetry()
        await oldPollIsIgnored()
        await oldUnauthorizedPollIsIgnored()
        print("Inbox loading: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
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
