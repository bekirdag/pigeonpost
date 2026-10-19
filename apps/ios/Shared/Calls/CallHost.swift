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
//  Not covered: ringing while the app is closed (on a phone that needs CallKit and VoIP push), and
//  sharing this device's screen — WebKit offers it to Safari but not to an app's web view, so the
//  page finds no `getDisplayMedia` and hides its share button. Others' shared screens are shown.
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

    private(set) var mode: Mode = .hidden

    @ObservationIgnored private weak var session: Session?
    @ObservationIgnored private weak var account: Account?
    @ObservationIgnored private weak var inbox: Inbox?
    @ObservationIgnored private var ready = false
    @ObservationIgnored private var pending: (peer: String, kind: String)?
    #if os(macOS)
    /// Held while a call is on screen, so the display does not sleep in the middle of one.
    @ObservationIgnored private var awake: NSObjectProtocol?
    #endif

    static let production = URL(string: "https://inbox.pigeonpost.dev/native-call.html?v=20261019-native")!

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
        mode = .hidden
    }

    /// Call whoever `peer` is, by voice or with video.
    func call(_ peer: String, video: Bool) {
        let kind = video ? "video" : "audio"
        guard ready else { pending = (peer, kind); return }
        evaluate("PigeonpostNative.call(\(Self.json(peer)), \(Self.json(kind)))")
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
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: options),
              let json = String(data: data, encoding: .utf8) else { return }
        evaluate("PigeonpostNative.configure(\(json))")
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
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        ready = false
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
