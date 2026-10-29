// Actual CallHost + WKWebView, with delayed synthetic tokens and a loopback-only page.
import AppKit
import Foundation
import Observation
import WebKit

@MainActor final class Session {
    enum Status { case signedIn, signedOut }
    var status = Status.signedIn
    var tokens: [CheckedContinuation<String, Error>] = []
    var renewals: [CheckedContinuation<String, Error>] = []
    func token() async throws -> String { try await withCheckedThrowingContinuation { tokens.append($0) } }
    func renew() async throws -> String { try await withCheckedThrowingContinuation { renewals.append($0) } }
}
@MainActor final class Account { var me: Mailbox? }
struct CallConversation { let peer: String; let name: String }
@MainActor final class Inbox { var conversations: [CallConversation] = []; var toast: String? }
@MainActor final class AvatarStore { func dataURLs(for keys: [String]) -> [String: String] { [:] } }

@main @MainActor struct CallHostTests {
    static var checks = 0, failures = 0
    static let a = Mailbox(address: "/k/call-a", handle: "/test/a", label: "A")
    static let b = Mailbox(address: "/k/call-b", handle: "/test/b", label: "B")
    static func check(_ ok: Bool, _ label: String) {
        checks += 1
        if ok { print("  ok   " + label) } else { failures += 1; print("  FAIL " + label) }
    }
    static func wait(_ label: String, _ predicate: () async throws -> Bool) async {
        let end = Date().addingTimeInterval(15)
        while Date() < end {
            if (try? await predicate()) == true { return }
            try? await Task.sleep(for: .milliseconds(20))
        }
        fatalError("Timed out: " + label)
    }
    static func records(_ host: CallHost) async -> [[String: Any]] {
        (try? await host.webView.evaluateJavaScript("window.records || []")) as? [[String: Any]] ?? []
    }
    static func settle() async { try? await Task.sleep(for: .milliseconds(200)) }
    static func main() async {
        _ = NSApplication.shared
        guard let page = CallHost.page.host, page == "127.0.0.1" else { fatalError("A loopback call page is required") }
        await mailboxSwitch()
        await detachedToken()
        await reloadToken()
        await detachedRenewal()
        await joinsAndCompatibility()
        await cancelBeforeReady()
        await liveStreams()
        await notesKeys()
        await confirmPanel()
        print("Call host: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }
    static func mailboxSwitch() async {
        let host = CallHost(), session = Session(), account = Account(), inbox = Inbox()
        account.me = a
        host.attach(session: session, account: account, inbox: inbox)
        await wait("first mailbox token") { session.tokens.count == 1 }
        account.me = b
        host.attach(session: session, account: account, inbox: inbox)
        await wait("second mailbox token") { session.tokens.count == 2 }
        session.tokens[1].resume(returning: "synthetic-b")
        await wait("new mailbox configured") { await records(host).contains { $0["mailbox"] as? String == b.address } }
        session.tokens[0].resume(returning: "synthetic-a")
        await settle()
        let configured = await records(host).filter { $0["kind"] as? String == "configure" }
        check(configured.count == 1 && configured.first?["mailbox"] as? String == b.address,
              "a delayed token cannot reconfigure the old mailbox")
        host.detach()
    }
    static func detachedToken() async {
        let host = CallHost(), session = Session(), account = Account(), inbox = Inbox()
        account.me = a
        host.attach(session: session, account: account, inbox: inbox)
        await wait("token before detach") { session.tokens.count == 1 }
        host.detach()
        session.tokens[0].resume(returning: "synthetic-a")
        await settle()
        let configured = await records(host).filter { $0["kind"] as? String == "configure" }
        check(configured.isEmpty, "a token completing after detach cannot sign the page back in")
        check(!host.isAttached, "detach releases the old identity binding")
    }
    static func reloadToken() async {
        let host = CallHost(), session = Session(), account = Account(), inbox = Inbox()
        account.me = a
        host.attach(session: session, account: account, inbox: inbox)
        await wait("token before reload") { session.tokens.count == 1 }
        host.webView.reload()
        await wait("token after reload") { session.tokens.count == 2 }
        session.tokens[1].resume(returning: "synthetic-fresh-page")
        await wait("fresh page configured") { await records(host).contains { $0["token"] as? String == "synthetic-fresh-page" } }
        session.tokens[0].resume(returning: "synthetic-old-page")
        await settle()
        let configured = await records(host).filter { $0["kind"] as? String == "configure" }
        check(configured.count == 1 && configured.first?["token"] as? String == "synthetic-fresh-page",
              "a token from the old page cannot configure its replacement")
        host.detach()
    }
    static func detachedRenewal() async {
        let host = CallHost(), session = Session(), account = Account(), inbox = Inbox()
        account.me = a
        host.attach(session: session, account: account, inbox: inbox)
        await wait("initial token before renewal") { session.tokens.count == 1 }
        session.tokens[0].resume(returning: "synthetic-a")
        await wait("initial configuration") { await records(host).contains { $0["kind"] as? String == "configure" } }
        _ = try? await host.webView.evaluateJavaScript("window.webkit.messageHandlers.pigeonpost.postMessage({type:'renew'})")
        await wait("pending renewal") { session.renewals.count == 1 }
        host.detach()
        session.renewals[0].resume(returning: "synthetic-stale-renewal")
        await settle()
        let renewed = await records(host).filter { $0["kind"] as? String == "renewed" }
        check(renewed.isEmpty, "renewal completing after sign-out cannot restore a token")
    }
    static func joinsAndCompatibility() async {
        let host = CallHost(), session = Session(), account = Account(), inbox = Inbox()
        account.me = a
        host.answer(room: "synthetic-room", ringToken: nil)
        host.attach(session: session, account: account, inbox: inbox)
        host.call("/test/peer", video: false)
        await wait("queued call token") { session.tokens.count == 1 }
        check(await records(host).allSatisfy { $0["kind"] as? String != "call" }, "direct calls wait for identity configuration")
        session.tokens[0].resume(returning: "synthetic-a")
        await wait("queued call delivered") { await records(host).contains { $0["kind"] as? String == "call" } }
        check(await records(host).contains { $0["kind"] as? String == "answer" }, "a cold-launch ring answer survives initial attachment")
        let request = CallHost.ScheduledJoin(mailbox: a.address, code: "ABCDEFGHJK", occurrenceId: "original:20261025T090017", title: "Review", video: false)!
        await host.joinScheduled(request)
        let sent = await records(host).last { $0["kind"] as? String == "scheduled" }
        check(sent?["as"] as? String == a.address && sent?["occurrenceId"] as? String == request.occurrenceId && sent?["kind"] as? String == "scheduled",
              "calendar joins use the captured member and original opaque occurrence")
        check(sent?["code"] as? String == request.code && sent?["title"] as? String == "Review", "calendar meeting locator and title reach the page")
        check(!host.preparingScheduled && inbox.toast == nil, "waiting in a server lobby completes native preparation without an error")
        _ = try? await host.webView.evaluateJavaScript("window.joinResult={state:'error',code:'window_closed',message:'This meeting is closed.'}")
        await host.joinScheduled(request)
        check(inbox.toast == "This meeting is closed.", "meeting-window refusal is visible")
        _ = try? await host.webView.evaluateJavaScript("delete window.PigeonpostNative.joinScheduled")
        await host.joinScheduled(request)
        check(inbox.toast?.contains("updated call page") == true, "an older bridge gives an explicit update error")
        let count = await records(host).filter { $0["kind"] as? String == "scheduled" }.count
        await host.joinScheduled(CallHost.ScheduledJoin(mailbox: b.address, code: request.code, occurrenceId: request.occurrenceId, title: "Review", video: true)!)
        check(await records(host).filter { $0["kind"] as? String == "scheduled" }.count == count,
              "a stale calendar mailbox never reaches the page")
        check(CallHost.ScheduledJoin(mailbox: a.address, code: "https://evil.test", occurrenceId: "occ", title: "Review", video: true) == nil,
              "a URL cannot become a meeting locator")
        check(CallHost.ScheduledJoin(mailbox: a.address, code: request.code, occurrenceId: "bad\nocc", title: "Review", video: true) == nil,
              "control characters are rejected in occurrence ids")
        host.detach()
    }
    static func cancelBeforeReady() async {
        let host = CallHost(), session = Session(), account = Account(), inbox = Inbox()
        account.me = a
        host.attach(session: session, account: account, inbox: inbox)
        await wait("pending calendar call token") { session.tokens.count == 1 }
        let request = CallHost.ScheduledJoin(mailbox: a.address, code: "ABCDEFGHJK", occurrenceId: "occ", title: "Review", video: true)!
        let task = Task { await host.joinScheduled(request) }
        await wait("call preparation") { host.preparingScheduled }
        host.cancelScheduledJoin()
        session.tokens[0].resume(returning: "synthetic-a")
        await task.value
        await settle()
        check(await records(host).allSatisfy { $0["kind"] as? String != "scheduled" }, "cancel during configuration sends no meeting join")
        check(!host.preparingScheduled, "cancel restores the native inbox")
        host.detach()
    }
    static let group = "grp_" + String(repeating: "g", count: 26)
    static let stream = "str_" + String(repeating: "t", count: 26)
    static var keep: [AnyObject] = []
    static func signedIn() async -> (CallHost, Session, Inbox) {
        let host = CallHost(), session = Session(), account = Account(), inbox = Inbox()
        account.me = a
        keep += [session, account, inbox]
        host.attach(session: session, account: account, inbox: inbox)
        await wait("live token") { session.tokens.count == 1 }
        session.tokens[0].resume(returning: "synthetic-a")
        await wait("live configured") { await records(host).contains { $0["kind"] as? String == "configure" } }
        await settle()
        return (host, session, inbox)
    }
    static func liveStreams() async {
        let (host, _, _) = await signedIn()
        let events = Events()
        host.events = events
        let refused = { () async -> String? in
            _ = try? await host.webView.evaluateJavaScript("window.liveRefusal={ok:false,code:'needs_e2ee'}")
            return await host.watchStream(group: group, name: "Crew", stream: stream)
        }
        check(await refused() == "needs_e2ee", "a refusal from the page comes back as its code")
        check(host.live == nil, "a refused watch leaves no stream behind")
        _ = try? await host.webView.evaluateJavaScript("delete window.liveRefusal")
        check(await host.watchStream(group: group, name: "Crew", stream: stream) == nil, "watching a stream is handed to the page")
        let watched = await records(host).last { $0["kind"] as? String == "watch" }
        check(watched?["group"] as? String == group && watched?["stream"] as? String == stream && watched?["name"] as? String == "Crew",
              "the group and stream reach watchStream")
        await wait("live state") { host.live?.stream == stream }
        check(host.live?.role == "viewer" && events.live.last?["state"] as? String == "on" && events.live.last?["name"] as? String == "Crew",
              "the stream's state and role are reported outside the page")
        check(await host.watchStream(group: group, name: "Crew", stream: stream) == nil, "watching the same stream again only shows it")
        check(await records(host).contains { $0["kind"] as? String == "show" }, "the page shows the stream it already has")
        check(await host.goLive(group: group, name: "Crew", title: "Demo", camera: true, screen: false) == "busy",
              "going live is refused while another stream is on")
        // The calls service moves an invited viewer's role; the page is told once.
        _ = try? await host.webView.evaluateJavaScript("window.liveNow.role='invited'")
        await wait("stage invite forwarded") { await records(host).contains { $0["kind"] as? String == "groupStream" } }
        await wait("invited role") { host.live?.role == "invited" }
        try? await Task.sleep(for: .milliseconds(3_400))
        let invites = await records(host).filter { $0["kind"] as? String == "groupStream" }
        check(invites.count == 1 && invites.first?["address"] as? String == a.address && invites.first?["state"] as? String == "invited",
              "a stage invitation reaches the page once, for this mailbox")
        _ = try? await host.webView.evaluateJavaScript("window.liveNow.role='speaker'")
        await wait("speaker") { host.live?.publishing == true }
        check(events.live.last?["role"] as? String == "speaker", "joining the stage is reported (the microphone is now on)")
        host.stopStream()
        await wait("stream over") { host.live == nil }
        check(await records(host).contains { $0["kind"] as? String == "stopStream" } && events.live.last?["state"] as? String == "off",
              "stopping ends the stream state")
        check(await host.goLive(group: group, name: "Crew", title: String(repeating: "t", count: 140), camera: true, screen: true) == nil,
              "going live is handed to the page")
        let live = await records(host).last { $0["kind"] as? String == "goLive" }
        check((live?["title"] as? String)?.count == 100 && live?["camera"] as? Bool == true && live?["screen"] as? Bool == true,
              "the title is bounded and the chosen sources pass through")
        await wait("hosting") { host.live?.role == "host" }
        host.detach()
        check(host.live == nil && events.live.last?["state"] as? String == "off", "signing out ends the stream state")
        let (fresh, _, _) = await signedIn()
        _ = try? await fresh.webView.evaluateJavaScript("delete window.PigeonpostNative.watchStream")
        check(await fresh.watchStream(group: group, name: "Crew", stream: stream) == "bridge_unavailable", "an older page answers bridge_unavailable")
        fresh.detach()
        let idle = CallHost()
        check(await idle.watchStream(group: group, name: "Crew", stream: stream) == "not_ready", "nothing is sent before the call view is attached")
    }
    static func notesKeys() async {
        let (host, _, _) = await signedIn()
        let key = try? await host.notesKey()
        check(key == CallHost.NotesKey(id: "nkSYNTHETIC0000000000", publicKey: "BPUB", label: "Desktop app"),
              "the page's notes key (public half and label) reaches the app")
        let meeting: [String: Any] = ["id": "mtg_ok", "iv": "aXY=", "ciphertext": "Y3Q=", "keys": [["key_id": "nkSYNTHETIC0000000000", "epk": "e", "iv": "i", "wrapped": "w"]]]
        let data = try? await host.openBrief(meeting)
        let brief = data.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any]
        check(brief?["title"] as? String == "Synthetic" && (brief?["decisions"] as? [String]) == ["Ship"], "a brief is opened in the page and its JSON comes back")
        do {
            _ = try await host.openBrief(["id": "mtg_nokey", "keys": [] as [Any]])
            check(false, "a brief for other devices is refused")
        } catch {
            check(error as? CallHost.BridgeError == .refused("no_key_for_device"), "a brief for other devices is refused with its code")
        }
        host.detach()
    }
    static func confirmPanel() async {
        let (host, _, _) = await signedIn()
        _ = try? await host.webView.evaluateJavaScript("window.webkit.messageHandlers.pigeonpost.postMessage({type:'mode',mode:'full'})")
        await wait("full") { host.mode == .full }
        _ = try? await host.webView.evaluateJavaScript("setTimeout(()=>{window.confirmAnswer=confirm('End your stream for everyone?')},0)")
        await wait("confirmation shown") { host.confirmation != nil }
        check(host.confirmation?.message == "End your stream for everyone?", "the page's confirm is shown natively")
        host.answerConfirmation(true)
        await wait("answered") { (try? await host.webView.evaluateJavaScript("window.confirmAnswer")) as? Bool == true }
        check(host.confirmation == nil, "a native yes reaches the page")
        _ = try? await host.webView.evaluateJavaScript("window.webkit.messageHandlers.pigeonpost.postMessage({type:'mode',mode:'hidden'})")
        await wait("hidden") { host.mode == .hidden }
        _ = try? await host.webView.evaluateJavaScript("window.confirmAnswer=confirm('Hidden?')")
        check((try? await host.webView.evaluateJavaScript("window.confirmAnswer")) as? Bool == false && host.confirmation == nil,
              "nothing is asked while the call view is hidden")
        host.detach()
    }
}

@MainActor final class Events: CallPageEvents {
    var live: [[String: Any]] = []
    func callPage(_ type: String, _ message: [String: Any]) { if type == "live" { live.append(message) } }
}
