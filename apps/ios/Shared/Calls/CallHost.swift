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
    struct ScheduledJoin {
        let mailbox: String
        let code: String
        let occurrenceId: String
        let title: String
        let video: Bool
        init?(mailbox: String, code: String, occurrenceId: String, title: String, video: Bool) {
            let alphabet = Set("ABCDEFGHJKMNPQRSTUVWXYZ23456789")
            guard code.count == 10, code.allSatisfy(alphabet.contains), !mailbox.isEmpty,
                  !occurrenceId.isEmpty, occurrenceId.utf8.count <= 256,
                  !occurrenceId.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { return nil }
            self.mailbox = mailbox; self.code = code; self.occurrenceId = occurrenceId
            self.title = String(title.prefix(200)); self.video = video
        }
    }

    /// The one call page. One, because a ring answered from a lock screen or a notification has to
    /// reach the same page the window shows once it opens — the app may have been launched in the
    /// background with no window at all, and the page is loaded and signed in without one.
    static let shared = CallHost()

    private(set) var mode: Mode = .hidden
    private(set) var preparingScheduled = false

    /// Phase 6: the live stream this device is in, as the page reports it (`PigeonpostLive.current()`).
    struct LiveState: Equatable {
        let stream: String
        let group: String
        /// host, speaker, invited or viewer.
        let role: String
        /// Publishing this device's microphone (and camera): the streamer, or a speaker on the stage.
        var publishing: Bool { role == "host" || role == "speaker" }
    }
    private(set) var live: LiveState?
    /// Group names for the streams this device opened, for whoever shows the stream outside the page.
    @ObservationIgnored private(set) var liveGroupName: String?
    @ObservationIgnored private var liveWatch: Task<Void, Never>?

    /// A `window.confirm` from the page (live.js asks before ending a stream for everyone), shown as
    /// a native alert. WebKit answers `false` to every confirm an app does not handle.
    struct Confirmation: Identifiable {
        let id = UUID()
        let message: String
        fileprivate let answer: (Bool) -> Void
    }
    var confirmation: Confirmation?
    @ObservationIgnored private var scheduledOperation: UUID?

    @ObservationIgnored private weak var session: Session?
    @ObservationIgnored private weak var account: Account?
    @ObservationIgnored private weak var inbox: Inbox?
    /// Faces for the call tiles (Phase 1b), when the app has them. Set by the root view.
    @ObservationIgnored weak var avatars: AvatarStore?
    @ObservationIgnored private var ready = false
    @ObservationIgnored private var pending: (peer: String, kind: String)?
    /// Signed in on the page, so it has (or is opening) a session that can accept a ring.
    @ObservationIgnored private var configured = false
    @ObservationIgnored private var mailbox: String?
    @ObservationIgnored private var binding = UUID()
    @ObservationIgnored private var pageGeneration = UUID()
    @ObservationIgnored private var configurationRun = UUID()
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
    // nativeRing; Phase 1b: avatars; Phase 6: watchStream, goLive, stopStream; Phase 7: the notes
    // key); a new value keeps a cached older page from being used.
    static let production = URL(string: "https://inbox.pigeonpost.dev/native-call.html?v=20261027-live1")!

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
        guard session.status == .signedIn, let address = account.me?.address else { detach(); return }
        let changed = self.session !== session || self.account !== account || mailbox != address
        if changed {
            binding = UUID()
            configurationRun = UUID()
            configured = false
            scheduledOperation = nil
            preparingScheduled = false
            if self.session != nil {
                evaluate("window.PigeonpostNative && PigeonpostNative.signOut()")
                queued = []
                pending = nil
                mode = .hidden
                endLive()
            }
        }
        self.session = session
        self.account = account
        self.inbox = inbox
        mailbox = address
        if webView.url == nil { webView.load(URLRequest(url: Self.page)) }
        Task { await configure() }
    }

    func detach() {
        binding = UUID()
        configurationRun = UUID()
        evaluate("window.PigeonpostNative && PigeonpostNative.signOut()")
        configured = false
        queued = []
        pending = nil
        scheduledOperation = nil
        preparingScheduled = false
        mailbox = nil
        session = nil
        account = nil
        inbox = nil
        mode = .hidden
        endLive()
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
        if ready && configured && identityIsCurrent { evaluate(guarded) }
        else if session == nil || identityIsCurrent { queued.append(guarded) }
    }

    /// Call whoever `peer` is, by voice or with video.
    func call(_ peer: String, video: Bool) {
        let kind = video ? "video" : "audio"
        guard identityIsCurrent else { return }
        guard ready, configured else { pending = (peer, kind); return }
        evaluate("PigeonpostNative.call(\(Self.json(peer)), \(Self.json(kind)))")
    }

    /// Start or join a group's call (docs/groups.md §6) through the call page's own group client.
    /// The calls service rechecks membership; this only asks.
    func callGroup(_ id: String, name: String, video: Bool, join: Bool) {
        guard ready, configured, identityIsCurrent else {
            inbox?.toast = "Calling isn't available right now. Try again in a moment."
            return
        }
        let kind = video ? "video" : "audio"
        let method = join ? "joinGroup" : "placeGroup"
        evaluate("window.PigeonpostCalls && PigeonpostCalls.\(method) && PigeonpostCalls.\(method)(\(Self.json(kind)), {id: \(Self.json(id)), name: \(Self.json(name))})")
    }

    /// Called after the calendar's sheets are dismissed. Keep the reviewed mailbox and opaque
    /// occurrence id even when an instance was moved; the server decides whether it is open.
    func joinScheduled(_ request: ScheduledJoin) async {
        guard identityIsCurrent, mailbox == request.mailbox else {
            inbox?.toast = "Calling isn’t ready. Reopen your inbox and try again."
            return
        }
        guard scheduledOperation == nil, mode == .hidden else {
            inbox?.toast = "Finish the current call first."
            return
        }
        let operation = UUID(), currentBinding = binding
        scheduledOperation = operation
        preparingScheduled = true
        defer {
            if scheduledOperation == operation { scheduledOperation = nil; preparingScheduled = false }
        }
        let deadline = Date().addingTimeInterval(15)
        while !ready || !configured {
            guard !Task.isCancelled, scheduledOperation == operation, binding == currentBinding,
                  identityIsCurrent, mailbox == request.mailbox else { return }
            guard Date() < deadline else { inbox?.toast = "Calling isn’t ready yet. Try again in a moment."; return }
            try? await Task.sleep(for: .milliseconds(50))
        }
        guard !Task.isCancelled, scheduledOperation == operation, binding == currentBinding,
              identityIsCurrent, mailbox == request.mailbox else { return }
        let currentPage = pageGeneration
        do {
            let result = try await webView.callAsyncJavaScript("""
                if (!window.PigeonpostNative || typeof PigeonpostNative.joinScheduled !== 'function')
                    return {state:'error',code:'bridge_unavailable'};
                return await PigeonpostNative.joinScheduled(code, options);
                """, arguments: ["code": request.code, "options": ["as": request.mailbox,
                    "occurrenceId": request.occurrenceId, "title": request.title,
                    "kind": request.video ? "video" : "audio"]], in: nil, contentWorld: .page)
            guard scheduledOperation == operation, current(currentBinding, page: currentPage, address: request.mailbox),
                  let response = result as? [String: Any] else { return }
            switch response["state"] as? String {
            case "joined", "waiting", "cancelled": break
            case "choose_occurrence": inbox?.toast = "Choose a specific occurrence from your calendar to join."
            default:
                switch response["code"] as? String {
                case "bridge_unavailable": inbox?.toast = "Calling needs an updated call page. Close and reopen the app, then try again."
                case "timeout", "network": inbox?.toast = "Couldn’t reach the meeting. Check your connection and try again."
                case "identity_changed": inbox?.toast = "Your inbox changed. Open the event again to join."
                default: inbox?.toast = (response["message"] as? String).map { String($0.prefix(400)) } ?? "Couldn’t join the meeting. Try again from the event."
                }
            }
        } catch {
            guard scheduledOperation == operation, current(currentBinding, page: currentPage, address: request.mailbox) else { return }
            inbox?.toast = "Couldn’t open the meeting. Try again in a moment."
        }
    }

    // ---- Phase 6: live streams in groups (docs/streaming.md) -----------------------------------

    /// Watch a group's live stream in the call view. Answers nil when the page took it (the stream
    /// view is on screen, or explains itself there), otherwise a refusal code for `LiveStreams.explain`.
    func watchStream(group: String, name: String, stream: String) async -> String? {
        if let live, live.stream == stream {
            evaluate("window.PigeonpostLive && PigeonpostLive.show()")
            return nil
        }
        return await liveCall("""
            if (!window.PigeonpostNative || typeof PigeonpostNative.watchStream !== 'function') return {ok:false, code:'bridge_unavailable'};
            return await PigeonpostNative.watchStream(group, stream);
            """, arguments: ["group": ["id": group, "name": String(name.prefix(80))], "stream": stream], group: group, name: name)
    }

    /// Go live in a group. The page opens the camera and microphone itself (the system asks first,
    /// in the app's own words); `screen` is offered only where the web view can share one.
    func goLive(group: String, name: String, title: String, camera: Bool, screen: Bool) async -> String? {
        await liveCall("""
            if (!window.PigeonpostNative || typeof PigeonpostNative.goLive !== 'function') return {ok:false, code:'bridge_unavailable'};
            return await PigeonpostNative.goLive(group, options);
            """, arguments: ["group": ["id": group, "name": String(name.prefix(80))],
                             "options": ["title": String(title.prefix(100)), "camera": camera, "screen": screen]],
            group: group, name: name)
    }

    /// End this device's part in the stream: the streamer's ends it for everyone, anyone else leaves.
    func stopStream() {
        evaluate("window.PigeonpostNative && typeof PigeonpostNative.stopStream === 'function' && PigeonpostNative.stopStream()")
    }

    /// Whether this web view can share a screen (never on iPhone or iPad; a Mac's may).
    func canShareScreen() async -> Bool {
        guard ready else { return false }
        let answer = try? await webView.evaluateJavaScript("Boolean(navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia)")
        return answer as? Bool == true
    }

    private func liveCall(_ script: String, arguments: [String: Any], group: String, name: String) async -> String? {
        // Not attached yet (the app is still opening the call view) reads as not ready, not signed out.
        guard session != nil else { return "not_ready" }
        guard identityIsCurrent else { return "signed_out" }
        guard ready, configured else { return "not_ready" }
        guard mode == .hidden, !preparingScheduled, live == nil else { return "busy" }
        let currentBinding = binding, currentPage = pageGeneration
        guard let address = mailbox else { return "signed_out" }
        liveGroupName = name
        let result: Any?
        do {
            result = try await webView.callAsyncJavaScript(script, arguments: arguments, in: nil, contentWorld: .page)
        } catch {
            return current(currentBinding, page: currentPage, address: address) ? "bridge_unavailable" : nil
        }
        guard current(currentBinding, page: currentPage, address: address) else { return nil }
        let response = result as? [String: Any]
        if response?["ok"] as? Bool == true {
            watchLive()
            return nil
        }
        // A stream view the page already closed with its own explanation; anything else is ours.
        let code = (response?["code"] as? String).map { String($0.prefix(64)) } ?? "error"
        return code
    }

    /// While a stream is on: follow the page's own state. The app has no group event stream, so a
    /// stage invitation (the calls service moves the viewer's role to `invited` within its 10 s
    /// check) is handed to the page here, once per invitation, as groups.js hands it the event.
    private func watchLive() {
        liveWatch?.cancel()
        let currentBinding = binding, currentPage = pageGeneration
        guard let address = mailbox else { return }
        liveWatch = Task { [weak self] in
            var lastRole: String?
            var misses = 0
            while !Task.isCancelled {
                guard let self, self.current(currentBinding, page: currentPage, address: address) else { break }
                let answer = try? await self.webView.evaluateJavaScript(
                    "JSON.stringify(window.PigeonpostLive ? PigeonpostLive.current() : null)") as? String
                guard !Task.isCancelled, self.current(currentBinding, page: currentPage, address: address) else { break }
                let now = answer.flatMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) } as? [String: Any]
                if let id = now?["id"] as? String, let group = now?["groupId"] as? String, let role = now?["role"] as? String {
                    misses = 0
                    if role == "invited", lastRole != "invited" {
                        // Addressed to the mailbox the page itself is signed in as.
                        _ = try? await self.webView.callAsyncJavaScript("""
                            const me = window.PigeonpostCallsHost && PigeonpostCallsHost.mailbox();
                            if (me && window.PigeonpostLive && PigeonpostLive.onGroupStream)
                              PigeonpostLive.onGroupStream({stream_id: stream, stage: {address: me.address, state: "invited"}});
                            """, arguments: ["stream": id], in: nil, contentWorld: .page)
                    }
                    lastRole = role
                    self.setLive(LiveState(stream: id, group: group, role: role))
                } else {
                    // A stream still joining has no id yet on the page; give it a moment.
                    misses += 1
                    if self.live != nil || misses > 20 { break }
                }
                try? await Task.sleep(nanoseconds: 1_500_000_000)
            }
            self?.endLive()
        }
    }

    private func setLive(_ next: LiveState?) {
        guard live != next else { return }
        live = next
        var message: [String: Any] = ["state": next == nil ? "off" : "on"]
        if let next {
            message["stream"] = next.stream
            message["group"] = next.group
            message["role"] = next.role
            message["name"] = liveGroupName ?? "Group"
        }
        events?.callPage("live", message)
    }

    private func endLive() {
        liveWatch?.cancel()
        liveWatch = nil
        setLive(nil)
    }

    // ---- Phase 7: this device's meeting notes key (docs/meeting-summaries.md §3) ------------------

    struct NotesKey: Equatable {
        let id: String
        let publicKey: String
        let label: String
    }

    enum BridgeError: Error, Equatable {
        case unavailable
        case refused(String)
    }

    /// The call page's notes key: the same key the page's consent sheet sends with a member's
    /// Agree, kept in the page's own storage on this device. Only its public half leaves the page.
    func notesKey() async throws -> NotesKey {
        try await waitForPage()
        let result = try? await webView.callAsyncJavaScript("""
            if (!window.PigeonpostNotesKeys) return {error:'bridge_unavailable'};
            try {
              const k = await PigeonpostNotesKeys.get();
              return {key_id: k.key_id, public_key: k.public_key, label: PigeonpostNotesKeys.label()};
            } catch (e) { return {error: String((e && (e.code || e.message)) || 'key_unavailable').slice(0, 64)}; }
            """, arguments: [:], in: nil, contentWorld: .page)
        guard let answer = result as? [String: Any] else { throw BridgeError.unavailable }
        if let error = answer["error"] as? String { throw error == "bridge_unavailable" ? BridgeError.unavailable : BridgeError.refused(error) }
        guard let id = answer["key_id"] as? String, let key = answer["public_key"] as? String else { throw BridgeError.unavailable }
        return NotesKey(id: id, publicKey: key, label: answer["label"] as? String ?? "App")
    }

    /// Open a stored brief with this device's notes key, inside the page; the plaintext JSON comes
    /// back. `meeting` is `GET /v1/meetings/{id}` (id, iv, ciphertext, keys).
    func openBrief(_ meeting: [String: Any]) async throws -> Data {
        try await waitForPage()
        let result = try? await webView.callAsyncJavaScript("""
            if (!window.PigeonpostNotesKeys || typeof PigeonpostNotesKeys.open !== 'function') return {error:'bridge_unavailable'};
            try { return {brief: JSON.stringify(await PigeonpostNotesKeys.open(meeting))}; }
            catch (e) { return {error: String((e && e.code) || 'decrypt_failed').slice(0, 64)}; }
            """, arguments: ["meeting": meeting], in: nil, contentWorld: .page)
        guard let answer = result as? [String: Any] else { throw BridgeError.unavailable }
        if let error = answer["error"] as? String { throw error == "bridge_unavailable" ? BridgeError.unavailable : BridgeError.refused(error) }
        guard let brief = answer["brief"] as? String else { throw BridgeError.unavailable }
        return Data(brief.utf8)
    }

    /// The page loaded (signed in or not: the notes key belongs to this device, not a mailbox).
    private func waitForPage() async throws {
        if webView.url == nil, session != nil { webView.load(URLRequest(url: Self.page)) }
        let deadline = Date().addingTimeInterval(15)
        while !ready {
            guard Date() < deadline, !Task.isCancelled else { throw BridgeError.unavailable }
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    func answerConfirmation(_ yes: Bool) {
        let pending = confirmation
        confirmation = nil
        pending?.answer(yes)
    }

    func cancelScheduledJoin() {
        guard scheduledOperation != nil else { return }
        scheduledOperation = nil
        preparingScheduled = false
        evaluate("window.PigeonpostNative && PigeonpostNative.hangUp()")
    }

    private func configure() async {
        guard ready, identityIsCurrent, let session, let account, var me = account.me else { return }
        let currentBinding = binding, currentPage = pageGeneration, address = me.address
        let run = UUID()
        configurationRun = run
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
        guard current(currentBinding, page: currentPage, address: address), configurationRun == run else { return }
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
        let applied = try? await webView.callAsyncJavaScript("""
            if (!window.PigeonpostNative || typeof PigeonpostNative.configure !== 'function') return false;
            PigeonpostNative.configure(options);
            return true;
            """, arguments: ["options": options], in: nil, contentWorld: .page)
        guard current(currentBinding, page: currentPage, address: address), configurationRun == run,
              applied as? Bool == true else { return }
        configured = token != nil
        if configured, !queued.isEmpty {
            let waiting = queued
            queued = []
            waiting.forEach(evaluate)
        }
        if configured, let pending {
            self.pending = nil
            call(pending.peer, video: pending.kind == "video")
        }
    }

    private var identityIsCurrent: Bool {
        session?.status == .signedIn && mailbox != nil && account?.me?.address == mailbox
    }

    private func current(_ binding: UUID, page: UUID, address: String) -> Bool {
        ready && identityIsCurrent && self.binding == binding && pageGeneration == page && mailbox == address
    }

    private func renew() async {
        guard ready, identityIsCurrent, let session, let address = mailbox else { return }
        let currentBinding = binding, currentPage = pageGeneration
        #if DEBUG
        if let debugToken = Self.debug.string(forKey: "calls-token") {
            evaluate("PigeonpostNative.renewed(\(Self.json(debugToken)))")
            return
        }
        #endif
        let fresh = try? await session.renew()
        guard current(currentBinding, page: currentPage, address: address) else { return }
        evaluate("PigeonpostNative.renewed(\(fresh.map(Self.json) ?? "null"))")
    }

    fileprivate func received(_ body: Any) {
        guard let message = body as? [String: Any], let type = message["type"] as? String else { return }
        switch type {
        case "ready":
            ready = true
            Task {
                await configure()
                #if DEBUG
                // `-calls-report YES`: the call client's own `inspect()` on standard output every few
                // seconds, so a local test can see this side's connection and what it receives.
                if Self.debug.bool(forKey: "calls-report") { report() }
                // `-calls-live watch:GROUP:STREAM` or `-calls-live golive:GROUP:TITLE`: the group
                // screen's Watch and Go live buttons, through the same bridge calls, for the local
                // stream harness (apps/ios/Tests/streams-native.mjs).
                if let auto = Self.debug.string(forKey: "calls-live") {
                    let parts = auto.split(separator: ":", maxSplits: 2).map(String.init)
                    try? await Task.sleep(nanoseconds: 1_500_000_000)
                    if parts.count == 3 {
                        let code = parts[0] == "watch"
                            ? await watchStream(group: parts[1], name: "Crew", stream: parts[2])
                            : await goLive(group: parts[1], name: "Crew", title: parts[2], camera: true, screen: false)
                        FileHandle.standardOutput.write(Data("calls-live \(parts[0]) \(code ?? "ok")\n".utf8))
                        NSLog("pigeonpost: calls-live \(parts[0]) \(code ?? "ok")")
                    }
                }
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
                let script = "JSON.stringify({share: Boolean(navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia), mode: \(Self.json(mode.rawValue)), calls: window.PigeonpostCalls && PigeonpostCalls.inspect(), live: window.PigeonpostLive ? PigeonpostLive.inspect() : null})"
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
        pageGeneration = UUID()
        ready = false
        configured = false
        endLive()
        answerConfirmation(false)
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        pageGeneration = UUID()
        ready = false
        configured = false
        endLive()
        answerConfirmation(false)
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

    /// `window.confirm` from the call page itself: a native alert. One at a time; anything else is a no.
    func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor (Bool) -> Void) {
        guard frame.isMainFrame, Self.trusted(frame.securityOrigin), confirmation == nil, mode != .hidden else {
            return completionHandler(false)
        }
        confirmation = Confirmation(message: String(message.prefix(300)), answer: completionHandler)
    }

    /// `window.alert`: said as the app's own toast; the page goes on at once.
    func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor () -> Void) {
        if frame.isMainFrame, Self.trusted(frame.securityOrigin), !message.isEmpty { inbox?.toast = String(message.prefix(300)) }
        completionHandler()
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
        MainActor.assumeIsolated {
            guard message.frameInfo.isMainFrame, CallHost.trusted(message.frameInfo.securityOrigin) else { return }
            host?.received(body)
        }
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
            .allowsHitTesting(calls.mode != .hidden || calls.preparingScheduled)
            .accessibilityHidden(calls.mode == .hidden && !calls.preparingScheduled)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .overlay {
                if calls.preparingScheduled, calls.mode == .hidden {
                    VStack(spacing: 16) {
                        ProgressView("Preparing call…")
                        Button("Cancel") { calls.cancelScheduledJoin() }.buttonStyle(.bordered)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity).background(Theme.ground)
                    .accessibilityIdentifier("scheduled-call-preparing")
                }
            }
            .alert(calls.confirmation?.message ?? "",
                   isPresented: Binding(get: { calls.confirmation != nil }, set: { if !$0 { calls.answerConfirmation(false) } })) {
                Button("OK", role: .destructive) { calls.answerConfirmation(true) }
                    .accessibilityIdentifier("call-confirm-ok")
                Button("Cancel", role: .cancel) { calls.answerConfirmation(false) }
            }
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
