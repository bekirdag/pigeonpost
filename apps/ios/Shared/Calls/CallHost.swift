//  Voice and video calls on iPhone, iPad and the Mac.
//
//  The calls are the web inbox's own (site-inbox/calls.js), run in an embedded web view rather than
//  rewritten: the same client rings, answers, runs group calls and meeting links, chat, device
//  choice and noise removal, and has already been tested against the call service and the relay.
//  WebKit carries the media — encrypted end to end (DTLS-SRTP), exactly as in Safari.
//
//  The app owns sign-in. The page (native-call.html + native-call.js) is handed the member token and
//  the mailbox in use, asks the app when the token needs renewing, and says how much of the screen
//  the call needs: all of it, a strip while the person reads mail during a call, or none — while
//  idle it stays loaded and invisible, so an incoming call can ring while the app is open.
//
//  Ringing while the app is closed is not this file's job: on a phone it is CallKit and VoIP push
//  (`Pigeonpost/Calls/CallAlerts.swift`), on a Mac a `CALL_RING` notification (`MacCallAlerts`).
//  Both act on the page through `answer`, `decline`, `setMuted` and `hangUp` here, and hear about
//  rings and the call's progress through `events`. See docs/call-alerting.md.
//
//  Not covered: sharing this device's screen — WebKit offers it to Safari but not to an app's web
//  view, so the page finds no `getDisplayMedia` and hides its share button. Others' shared screens
//  are shown.
//
//  On the Mac the window stays one window: the call covers it, or shrinks to a strip along its top
//  edge while mail is read underneath, exactly as on the iPad.

import Observation
import SwiftUI
import WebKit
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

@MainActor
@Observable
final class CallHost: NSObject {
    enum Mode: String { case hidden, mini, full }

    /// The one call page. One, because a ring answered from a lock screen or a notification has to
    /// reach the same page the window shows once it opens — the app may have been launched in the
    /// background with no window at all, and the page is loaded and signed in without one.
    static let shared = CallHost()

    private(set) var mode: Mode = .hidden

    @ObservationIgnored private weak var session: Session?
    @ObservationIgnored private weak var account: Account?
    @ObservationIgnored private weak var inbox: Inbox?
    /// Faces for the call tiles (Phase 1b), when the app has them. Set by the root view.
    @ObservationIgnored weak var avatars: AvatarStore?
    @ObservationIgnored private var ready = false
    @ObservationIgnored private var pending: (peer: String, kind: String)?
    /// Signed in on the page, so it has (or is opening) a session that can accept a ring.
    @ObservationIgnored private var configured = false
    /// Bridge calls made before the page was ready and signed in, run in order once it is.
    @ObservationIgnored private var queued: [String] = []

    /// True on iPhone and iPad: incoming calls are shown by the system (CallKit), so the page keeps
    /// the ring state but shows no ring screen and plays no ringtone of its own. Set before `attach`.
    @ObservationIgnored var nativeRing = false
    /// Who hears about rings and the call's progress (CallKit on a phone, notifications on a Mac).
    @ObservationIgnored weak var events: CallPageEvents?

    /// Whether the app has handed the page a session yet (the window did, or a ring did).
    var isAttached: Bool { session != nil }
    #if os(macOS)
    /// Held while a call is on screen, so the display does not sleep in the middle of one.
    @ObservationIgnored private var awake: NSObjectProtocol?
    #endif

    // The version names the bridge this build expects (Phase 1c: answer, decline, setMuted, hangUp,
    // nativeRing; Phase 1b: avatars); a new value keeps a cached older page from being used.
    static let production = URL(string: "https://inbox.pigeonpost.dev/native-call.html?v=20261021-ring-faces")!

    #if DEBUG
    /// Local call tests only: `-calls-page URL -calls-token T -calls-mailbox ADDRESS [-calls-handle H]
    /// [-call PEER:audio|video]`, against the harness's stand-in postbox and a local call service.
    /// Never compiled into a release.
    private static let debug = UserDefaults.standard
    static var debugEnabled: Bool { debug.string(forKey: "calls-page") != nil }
    static var page: URL { debug.string(forKey: "calls-page").flatMap(URL.init(string:)) ?? production }
    #else
    static let debugEnabled = false
    static var page: URL { production }
    #endif

    @ObservationIgnored lazy var webView: WKWebView = {
        let configuration = WKWebViewConfiguration()
        configuration.mediaTypesRequiringUserActionForPlayback = []
        configuration.userContentController.add(WeakHandler(self), name: "pigeonpost")
        #if canImport(UIKit)
        configuration.allowsInlineMediaPlayback = true
        #endif
        let view = WKWebView(frame: .zero, configuration: configuration)
        #if canImport(UIKit)
        view.isOpaque = false
        view.backgroundColor = .clear
        view.scrollView.backgroundColor = .clear
        view.scrollView.isScrollEnabled = false
        view.scrollView.contentInsetAdjustmentBehavior = .never
        view.accessibilityIdentifier = "callView"
        #else
        // Transparent while idle: the window's own content shows through until a call needs it.
        view.setValue(false, forKey: "drawsBackground")
        view.underPageBackgroundColor = .clear
        view.setAccessibilityIdentifier("callView")
        #endif
        view.navigationDelegate = self
        view.uiDelegate = self
        #if DEBUG
        if #available(iOS 16.4, macOS 13.3, *) { view.isInspectable = Self.debugEnabled }
        #endif
        return view
    }()

    /// Load the page (once) and tell it who is signed in. Called whenever the mailbox changes.
    func attach(session: Session, account: Account, inbox: Inbox) {
        self.session = session
        self.account = account
        self.inbox = inbox
        if webView.url == nil { webView.load(URLRequest(url: Self.page)) }
        Task { await configure() }
    }

    func detach() {
        evaluate("window.PigeonpostNative && PigeonpostNative.signOut()")
        configured = false
        queued = []
        mode = .hidden
    }

    /// Accept this ring as soon as the page has a session. With a ring token it works even if the
    /// page never saw the ring itself (the app was woken by a push); without one (the ring came over
    /// the page's own event stream) the page accepts through its session.
    func answer(room: String, ringToken: String?) {
        run("PigeonpostNative.answer(\(Self.json(room)), \(ringToken.map(Self.json) ?? "null"))")
    }

    /// Decline from the page's own session — for a ring with no token. A ring that came with a token
    /// is declined over HTTP instead (`RingEndpoint.decline`), which needs no session.
    func decline(room: String, ringToken: String?) {
        run("PigeonpostNative.decline(\(Self.json(room)), \(ringToken.map(Self.json) ?? "null"))")
    }

    func setMuted(_ muted: Bool) {
        run("PigeonpostNative.setMuted(\(muted ? "true" : "false"))")
    }

    func hangUp() {
        run("PigeonpostNative.hangUp()")
    }

    /// The call page's origin: where the ring-token endpoints live.
    var pageURL: URL { Self.page }

    /// Now if the page is signed in, otherwise once it is. Guarded on the function existing so an
    /// older page (served live, so possibly behind the app) fails quietly rather than throwing.
    private func run(_ script: String) {
        let name = script.prefix { $0 != "(" }
        let guarded = "window.PigeonpostNative && typeof \(name) === 'function' && \(script)"
        if ready && configured { evaluate(guarded) } else { queued.append(guarded) }
    }

    /// Call whoever `peer` is, by voice or with video.
    func call(_ peer: String, video: Bool) {
        let kind = video ? "video" : "audio"
        guard ready else { pending = (peer, kind); return }
        evaluate("PigeonpostNative.call(\(Self.json(peer)), \(Self.json(kind)))")
    }

    /// Start or join a group's call (docs/groups.md §6) through the call page's own group client.
    /// The calls service rechecks membership; this only asks.
    func callGroup(_ id: String, name: String, video: Bool, join: Bool) {
        guard ready else {
            inbox?.toast = "Calling isn't available right now. Try again in a moment."
            return
        }
        let kind = video ? "video" : "audio"
        let method = join ? "joinGroup" : "placeGroup"
        evaluate("window.PigeonpostCalls && PigeonpostCalls.\(method) && PigeonpostCalls.\(method)(\(Self.json(kind)), {id: \(Self.json(id)), name: \(Self.json(name))})")
    }

    private func configure() async {
        guard ready, let session, let account, var me = account.me else { return }
        var token: String?
        #if DEBUG
        if let debugToken = Self.debug.string(forKey: "calls-token"), let address = Self.debug.string(forKey: "calls-mailbox") {
            token = debugToken
            me = Mailbox(address: address, handle: Self.debug.string(forKey: "calls-handle"), label: nil)
        } else {
            token = try? await session.token()
        }
        #else
        token = try? await session.token()
        #endif
        var known: [String: String] = [:]
        for conversation in inbox?.conversations ?? [] { known[conversation.peer] = conversation.name }
        let options: [String: Any] = [
            "token": token ?? NSNull(),
            "mailbox": ["address": me.address, "handle": me.handle as Any? ?? NSNull()] as [String: Any],
            "known": known,
            "nativeRing": nativeRing,
            // `data:` URLs of the faces already fetched, painted over the tiles' initials by a page
            // that knows how (native-call.js `decorateAvatar`); an older page ignores the field.
            "avatars": avatars?.dataURLs(for: Array(known.keys) + [me.handle ?? me.address]) ?? [:],
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: options),
              let json = String(data: data, encoding: .utf8) else { return }
        // The page may have reloaded while a token was being fetched.
        guard ready else { return }
        evaluate("PigeonpostNative.configure(\(json))")
        configured = token != nil
        if configured, !queued.isEmpty {
            let waiting = queued
            queued = []
            waiting.forEach(evaluate)
        }
    }

    private func renew() async {
        #if DEBUG
        if let debugToken = Self.debug.string(forKey: "calls-token") {
            evaluate("PigeonpostNative.renewed(\(Self.json(debugToken)))")
            return
        }
        #endif
        let fresh = try? await session?.renew()
        evaluate("PigeonpostNative.renewed(\(fresh.map(Self.json) ?? "null"))")
    }

    fileprivate func received(_ body: Any) {
        guard let message = body as? [String: Any], let type = message["type"] as? String else { return }
        switch type {
        case "ready":
            ready = true
            Task {
                await configure()
                if let pending {
                    self.pending = nil
                    call(pending.peer, video: pending.kind == "video")
                }
                #if DEBUG
                // `-calls-report YES`: the call client's own `inspect()` on standard output every few
                // seconds, so a local test can see this side's connection and what it receives.
                if Self.debug.bool(forKey: "calls-report") { report() }
                if let auto = Self.debug.string(forKey: "call"), let colon = auto.lastIndex(of: ":") {
                    try? await Task.sleep(nanoseconds: 1_500_000_000)
                    call(String(auto[..<colon]), video: auto[auto.index(after: colon)...] == "video")
                    // `-calls-then JS`: run on the page once the call has had time to connect.
                    if let then = Self.debug.string(forKey: "calls-then") {
                        try? await Task.sleep(nanoseconds: 14_000_000_000)
                        evaluate(then)
                    }
                }
                #endif
            }
        case "mode":
            let next = Mode(rawValue: message["mode"] as? String ?? "") ?? .hidden
            withAnimation(.easeOut(duration: 0.2)) { mode = next }
            // A call keeps the screen awake.
            #if canImport(UIKit)
            UIApplication.shared.isIdleTimerDisabled = next != .hidden
            #else
            if next == .hidden, let awake {
                ProcessInfo.processInfo.endActivity(awake)
                self.awake = nil
            } else if next != .hidden, awake == nil {
                awake = ProcessInfo.processInfo.beginActivity(options: [.idleDisplaySleepDisabled, .userInitiated],
                                                              reason: "A call is in progress")
            }
            #endif
        case "renew":
            Task { await renew() }
        case "toast":
            if let text = message["text"] as? String, !text.isEmpty { inbox?.toast = text }
        case "ring", "ring_ended", "call", "muted":
            events?.callPage(type, message)
        default:
            break
        }
    }

    #if DEBUG
    @ObservationIgnored private var reporting = false
    private func report() {
        guard !reporting else { return }
        reporting = true
        Task { @MainActor in
            while true {
                let script = "JSON.stringify({share: Boolean(navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia), mode: \(Self.json(mode.rawValue)), calls: window.PigeonpostCalls && PigeonpostCalls.inspect()})"
                if let line = try? await webView.evaluateJavaScript(script) as? String {
                    FileHandle.standardOutput.write(Data("calls-report \(line)\n".utf8))
                }
                try? await Task.sleep(nanoseconds: 3_000_000_000)
            }
        }
    }
    #endif

    #if DEBUG
    /// Local tests only: run a line on the call page (a tap on its own controls).
    func debugRun(_ script: String) { evaluate(script) }
    #endif

    private func evaluate(_ script: String) {
        webView.evaluateJavaScript(script, completionHandler: nil)
    }

    private static func json(_ value: String) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: [value])) ?? Data("[\"\"]".utf8)
        let array = String(data: data, encoding: .utf8) ?? "[\"\"]"
        return String(array.dropFirst().dropLast())
    }
}

extension CallHost: WKNavigationDelegate, WKUIDelegate {
    /// The page, and nothing else. A link out — the meeting link's own page, a help link — opens in
    /// the browser rather than replacing the call client.
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void) {
        guard let url = action.request.url else { return decisionHandler(.cancel) }
        if url.host == Self.page.host, url.path == Self.page.path { return decisionHandler(.allow) }
        if action.navigationType == .linkActivated {
            #if canImport(UIKit)
            UIApplication.shared.open(url)
            #else
            NSWorkspace.shared.open(url)
            #endif
        }
        decisionHandler(.cancel)
    }

    /// A reload (the web view's process was reclaimed) starts again from a page that knows nothing.
    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        ready = false
        configured = false
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        ready = false
        configured = false
        mode = .hidden
        webView.load(URLRequest(url: Self.page))
    }

    /// The microphone and camera are asked for by the system, once, with the app's own words; the
    /// page itself does not ask again on every call. Only the call page's own origin is granted.
    func webView(_ webView: WKWebView, requestMediaCapturePermissionFor origin: WKSecurityOrigin,
                 initiatedByFrame frame: WKFrameInfo, type: WKMediaCaptureType,
                 decisionHandler: @escaping @MainActor (WKPermissionDecision) -> Void) {
        decisionHandler(Self.trusted(origin) ? .grant : .deny)
    }

    /// The call page's origin — scheme, host and port — and nothing else.
    static func trusted(_ origin: WKSecurityOrigin) -> Bool {
        let port = Self.page.port ?? (Self.page.scheme == "https" ? 443 : 80)
        let originPort = origin.port == 0 ? (origin.protocol == "https" ? 443 : 80) : origin.port
        return origin.protocol == Self.page.scheme && origin.host == Self.page.host && originPort == port
    }
}

/// What the page says about rings and calls, for whoever shows them outside the page.
///
/// `ring` and `ring_ended` (a ring arrived on the event stream, or stopped), `call` (`state`:
/// connecting, active, ended) and `muted`. The message is passed as the page sent it.
@MainActor
protocol CallPageEvents: AnyObject {
    func callPage(_ type: String, _ message: [String: Any])
}

/// The content controller holds its handlers strongly; this keeps that from holding the host.
private final class WeakHandler: NSObject, WKScriptMessageHandler {
    weak var host: CallHost?
    init(_ host: CallHost) { self.host = host }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        let body = message.body
        MainActor.assumeIsolated { host?.received(body) }
    }
}

/// The call, over the app: the whole screen, a strip at the top, or nothing that can be touched.
struct CallOverlay: View {
    @Environment(CallHost.self) private var calls

    var body: some View {
        let mini = calls.mode == .mini
        CallWebView(webView: calls.webView)
            .frame(maxWidth: .infinity, maxHeight: mini ? 64 : .infinity)
            .opacity(calls.mode == .hidden ? 0.01 : 1)
            .allowsHitTesting(calls.mode != .hidden)
            .accessibilityHidden(calls.mode == .hidden)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            #if os(iOS)
            // The call itself runs edge to edge and keeps clear of the notch and the home indicator
            // in its own stylesheet; the minimised strip sits under the status bar.
            .ignoresSafeArea(.all, edges: mini ? [] : .all)
            #endif
    }
}

#if canImport(UIKit)
private struct CallWebView: UIViewRepresentable {
    let webView: WKWebView
    func makeUIView(context: Context) -> WKWebView { webView }
    func updateUIView(_ view: WKWebView, context: Context) {}
}
#else
private struct CallWebView: NSViewRepresentable {
    let webView: WKWebView
    func makeNSView(context: Context) -> WKWebView { webView }
    func updateNSView(_ view: WKWebView, context: Context) {}
}
#endif

/// The two buttons a conversation's bar gets.
struct CallButtons: View {
    let peer: String
    @Environment(CallHost.self) private var calls

    var body: some View {
        #if os(macOS)
        // Tapped images rather than `Button`s, as the find bar beside them does: a `Button` in the
        // Mac's toolbar has repeatedly cost the conversation list its width.
        HStack(spacing: 14) {
            control("phone", "Voice call", "voiceCall") { calls.call(peer, video: false) }
            control("video", "Video call", "videoCall") { calls.call(peer, video: true) }
        }
        .padding(.horizontal, 6)
        #else
        HStack(spacing: 4) {
            Button { calls.call(peer, video: false) } label: { Image(systemName: "phone") }
                .accessibilityLabel("Voice call")
                .accessibilityIdentifier("voiceCall")
            Button { calls.call(peer, video: true) } label: { Image(systemName: "video") }
                .accessibilityLabel("Video call")
                .accessibilityIdentifier("videoCall")
        }
        .disabled(calls.mode != .hidden)
        #endif
    }

    #if os(macOS)
    private func control(_ symbol: String, _ label: String, _ id: String, action: @escaping () -> Void) -> some View {
        let idle = calls.mode == .hidden
        return Image(systemName: symbol)
            .font(.geist(14, .medium))
            .foregroundStyle(idle ? Theme.secondary : Theme.rule)
            .frame(width: 22, height: 22)
            .contentShape(Rectangle())
            .onTapGesture { if idle { action() } }
            .help(label)
            .accessibilityLabel(label)
            .accessibilityIdentifier(id)
            .accessibilityAddTraits(.isButton)
    }
    #endif
}
