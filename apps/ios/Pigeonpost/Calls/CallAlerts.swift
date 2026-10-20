//  Incoming calls on the lock screen and the home screen: PushKit and CallKit.
//
//  A call should ring the way a phone call rings — full screen on a locked phone, a banner over
//  whatever app is open, the app's name and icon on the call screen and in Recents — and it should
//  do that with the app killed. On an iPhone or iPad only one thing does that: a VoIP push (PushKit)
//  that wakes the app, which then tells CallKit there is a call. The system draws the call screen;
//  the app never does.
//
//  The rules iOS enforces, and how this file keeps them:
//
//  - Every VoIP push must become a CallKit call before the push's completion handler runs, or iOS
//    ends the app and, if it keeps happening, stops delivering VoIP pushes to it at all. So the
//    report comes first, before anything is checked. A push that turns out to be unusable — not a
//    ring, past its end time, or for an account this phone is not signed in to — is reported and
//    then ended at once.
//  - One room is one call. The CallKit UUID is derived from the room id (`CallRing.uuid`), so the
//    VoIP push and the call page's own ring for the same room (the app was open) are one call; the
//    second report is a duplicate CallKit refuses, not a second call screen.
//
//  Answering: the call itself is the web page's (`CallHost`), so answering means making sure that
//  page is loaded and signed in — the app may have been launched in the background by the push, with
//  no window, so the page is attached here rather than by a view — and then telling it which ring to
//  accept. Declining a ring that came by push uses its ring token over HTTP, which needs no member
//  token and so works from a lock screen whatever state the session is in.
//
//  Audio: CallKit owns the audio session during a call. The session is configured for a call
//  (play-and-record, voice or video chat) when the answer is accepted, and CallKit activates it
//  (`didActivate`). The page is told to answer only after that activation (or, if it never comes, a
//  couple of seconds later), so WebKit's microphone capture starts inside a session that is already
//  live; starting capture first and having CallKit activate the session under it is the usual cause
//  of a call with no audio on a locked phone. The `audio` background mode keeps the media running
//  with the screen off.
//
//  While the system screen rings, the ring's own state is polled every 2 s with the ring token: a
//  caller who hangs up, or a ring answered on another device, ends the call screen here too. The
//  calls service sends no push for that — a VoIP push that does not ring is one iOS punishes.
//
//  Contract: docs/call-alerting.md §5.

import AVFoundation
import CallKit
import PushKit
import UIKit

@MainActor
final class CallAlerts: NSObject {
    static let shared = CallAlerts()

    private let provider: CXProvider
    private let controller = CXCallController()
    private var registry: PKPushRegistry?

    private weak var session: Session?
    private weak var account: Account?
    private weak var inbox: Inbox?
    private var host: CallHost { CallHost.shared }

    private struct Call {
        var ring: CallRing
        var answered = false
        /// The page said the call is connecting or up.
        var started = false
        /// What CallKit and the page last agreed the microphone is.
        var muted = false
        /// Polls the ring's state, and ends the call screen when the ring ends.
        var watch: Task<Void, Never>?
        /// Gives up on an answer the page never acted on.
        var giveUp: Task<Void, Never>?
    }

    private var calls: [UUID: Call] = [:]
    /// CallKit has activated the audio session for the current call.
    private var audioActive = false
    /// Answered, and waiting for the audio session before the page is told.
    private var answering: UUID?

    private override init() {
        provider = CXProvider(configuration: Self.configuration)
        super.init()
        provider.setDelegate(self, queue: nil)
        // The system shows rings on this device; the page keeps the state but stays quiet.
        host.nativeRing = true
        host.events = self
    }

    private static var configuration: CXProviderConfiguration {
        let configuration = CXProviderConfiguration()
        configuration.supportsVideo = true
        // One call at a time, which is all the call page runs.
        configuration.maximumCallGroups = 1
        configuration.maximumCallsPerCallGroup = 1
        configuration.supportedHandleTypes = [.generic]
        configuration.includesCallsInRecents = true
        // The app's mark as a mask: the system tints it and puts it on the call screen's button
        // that opens the app.
        configuration.iconTemplateImageData = UIImage(named: "CallKitIcon")?.pngData()
        // nil is the system ringtone, which is what a call should sound like.
        configuration.ringtoneSound = nil
        return configuration
    }

    /// The app's own objects, so a ring answered from a lock screen can sign the page in. Handed over
    /// once, by the app at launch.
    func adopt(session: Session, account: Account, inbox: Inbox) {
        guard self.session == nil else { return }
        self.session = session
        self.account = account
        self.inbox = inbox
    }

    /// Ask PushKit for a VoIP token. Called from `application(_:didFinishLaunchingWithOptions:)`:
    /// when the app is launched by a VoIP push, PushKit delivers that push only once a registry with
    /// a delegate exists, and this is the earliest point there is one.
    func start() {
        guard registry == nil else { return }
        let registry = PKPushRegistry(queue: .main)
        registry.delegate = self
        registry.desiredPushTypes = [.voIP]
        self.registry = registry
        #if DEBUG
        simulateIfAsked()
        #endif
    }

    // ---- incoming ----------------------------------------------------------------------------

    /// A VoIP push. Reported to CallKit first, whatever it contains; `completion` runs once it has
    /// been.
    func incoming(push payload: [AnyHashable: Any], completion: @escaping () -> Void) {
        guard let ring = CallRing(push: payload) else {
            // Not a ring this app can use. iOS still requires a call for the push: report one and
            // end it in the same breath, so the person sees at most a flicker in Recents.
            let uuid = UUID()
            let update = CXCallUpdate()
            update.remoteHandle = CXHandle(type: .generic, value: "Pigeonpost")
            update.localizedCallerName = "Pigeonpost"
            NSLog("pigeonpost: unusable VoIP push reported and ended")
            provider.reportNewIncomingCall(with: uuid, update: update) { [provider] _ in
                provider.reportCall(with: uuid, endedAt: nil, reason: .failed)
                completion()
            }
            return
        }
        report(ring, completion: completion)
    }

    /// Put a ring on the system call screen — or, when the same room is already there, merge what
    /// this route knows (a push brings the ring token a page ring lacks).
    private func report(_ ring: CallRing, completion: (() -> Void)? = nil) {
        let uuid = ring.uuid
        if var known = calls[uuid] {
            if known.ring.ringToken == nil, let token = ring.ringToken {
                known.ring.ringToken = token
                calls[uuid] = known
                if !known.answered { watch(uuid) }
            }
            // A VoIP push for a call already on screen is still reported: iOS checks every push
            // against a report, and a duplicate UUID is refused without a second call appearing.
            NSLog("pigeonpost: same ring again room=\(ring.room), merged (token=\(calls[uuid]?.ring.ringToken != nil))")
            if let completion {
                #if DEBUG
                if Self.standIn { return completion() }
                #endif
                provider.reportNewIncomingCall(with: uuid, update: update(for: ring)) { _ in completion() }
            }
            return
        }
        calls[uuid] = Call(ring: ring)
        #if DEBUG
        if Self.standIn {
            completion?()
            NSLog("pigeonpost: call reported (stand-in) room=\(ring.room) token=\(ring.ringToken != nil) name=\(update(for: ring).localizedCallerName ?? "-")")
            vet(uuid)
            runScript(uuid)
            return
        }
        #endif
        provider.reportNewIncomingCall(with: uuid, update: update(for: ring)) { error in
            Task { @MainActor in
                completion?()
                if let error {
                    // Do Not Disturb, a blocked caller, or a call already in progress: the system
                    // said no, and there is nothing on screen to end.
                    NSLog("pigeonpost: call not shown: \(error.localizedDescription)")
                    if (error as? CXErrorCodeIncomingCallError)?.code != .callUUIDAlreadyExists {
                        self.forget(uuid)
                    }
                    return
                }
                NSLog("pigeonpost: call reported room=\(ring.room) token=\(ring.ringToken != nil)")
                self.vet(uuid)
            }
        }
    }

    private func update(for ring: CallRing) -> CXCallUpdate {
        let update = CXCallUpdate()
        update.remoteHandle = CXHandle(type: .generic, value: ring.handleValue)
        update.localizedCallerName = ring.callerName(known: knownName(ring)) { PeerFace.displayName($0) }
        update.hasVideo = ring.isVideo
        update.supportsHolding = false
        update.supportsGrouping = false
        update.supportsUngrouping = false
        update.supportsDTMF = false
        return update
    }

    /// The name this person's own inbox shows for the caller, when the caller is a conversation.
    private func knownName(_ ring: CallRing) -> String? {
        guard let conversations = inbox?.conversations else { return nil }
        let keys = [ring.fromHandle, ring.fromAddress].compactMap { $0 }
        return conversations.first { keys.contains($0.peer) }?.name
    }

    /// After the report: drop what should not ring, and watch what should.
    private func vet(_ uuid: UUID) {
        guard let call = calls[uuid] else { return }
        if call.ring.isExpired() {
            end(uuid, .unanswered)
            return
        }
        watch(uuid)
        // Only a push can be for somebody else; the page's own ring came over this account's session.
        guard call.ring.ringToken != nil else { return }
        Task {
            if await !self.belongsHere(call.ring) { self.end(uuid, .failed) }
        }
    }

    /// Whether this device is signed in to the account that owns the called mailbox. When that
    /// cannot be told (offline, or the app's objects are not up yet) the ring is allowed: the postbox
    /// only pushes to devices registered for the account.
    private func belongsHere(_ ring: CallRing) async -> Bool {
        guard let session, let account else { return true }
        if Fixtures.enabled { return true }
        guard session.status == .signedIn else { return false }
        if account.mailboxes.isEmpty { await account.loadIdentities() }
        if account.mailboxes.isEmpty { return true }
        return ring.isFor(mailboxes: account.mailboxes.map { ($0.address, $0.handle) })
    }

    /// Poll the ring every 2 s while the call screen rings, and end it at its end time.
    private func watch(_ uuid: UUID) {
        calls[uuid]?.watch?.cancel()
        let started = Date()
        calls[uuid]?.watch = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, let call = self.calls[uuid], !call.answered else { return }
                // The calls service rings for 45 s; a ring with no end time of its own is given 60.
                if call.ring.isExpired() || (call.ring.expiresAt == nil && Date().timeIntervalSince(started) > 60) {
                    self.end(uuid, .unanswered)
                    return
                }
                if let token = call.ring.ringToken,
                   case .ended(let why)? = await RingEndpoint.fetchState(page: self.host.pageURL, room: call.ring.room, ringToken: token),
                   let now = self.calls[uuid], !now.answered {
                    self.end(uuid, why)
                    return
                }
                try? await Task.sleep(nanoseconds: 2_000_000_000)
            }
        }
    }

    /// The ring or call ended somewhere other than this device's call screen. Reported, not
    /// requested: a `CXEndCallAction` would come straight back to `perform` and decline or hang up a
    /// second time.
    private func end(_ uuid: UUID, _ why: RingEnd) {
        guard let call = calls[uuid] else { return }
        NSLog("pigeonpost: call ended room=\(call.ring.room) reason=\(why)")
        provider.reportCall(with: uuid, endedAt: Date(), reason: Self.reason(why))
        forget(uuid)
        #if DEBUG
        if Self.standIn { NSLog("pigeonpost: call screen gone (stand-in) reason=\(Self.reason(why).rawValue)") }
        #endif
    }

    static func reason(_ why: RingEnd) -> CXCallEndedReason {
        switch why {
        case .answeredElsewhere: return .answeredElsewhere
        case .declinedElsewhere: return .declinedElsewhere
        case .remoteEnded: return .remoteEnded
        case .unanswered: return .unanswered
        case .failed: return .failed
        }
    }

    private func forget(_ uuid: UUID) {
        calls[uuid]?.watch?.cancel()
        calls[uuid]?.giveUp?.cancel()
        calls[uuid] = nil
        if answering == uuid { answering = nil }
    }

    // ---- answering ---------------------------------------------------------------------------

    private func answer(_ action: CXAnswerCallAction) {
        guard answerCall(action.callUUID) else { return action.fail() }
        // CallKit wants the answer acknowledged promptly; the page's own progress is reported back
        // through `callPage`, and an answer it never acts on is given up after 40 s.
        action.fulfill()
    }

    @discardableResult
    private func answerCall(_ uuid: UUID) -> Bool {
        guard var call = calls[uuid] else { return false }
        call.answered = true
        call.watch?.cancel()
        call.watch = nil
        calls[uuid] = call
        prepareAudio(video: call.ring.isVideo)
        answering = uuid
        calls[uuid]?.giveUp = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 40_000_000_000)
            guard !Task.isCancelled, let self, let call = self.calls[uuid], !call.started else { return }
            self.host.hangUp()
            self.end(uuid, .failed)
        }
        Task {
            guard await self.prepareHost() else {
                self.end(uuid, .failed)
                return
            }
            // If CallKit has not activated the session in two seconds it is not going to (the
            // simulator never does); answer anyway rather than leave the caller waiting.
            if !self.audioActive { try? await Task.sleep(nanoseconds: 2_000_000_000) }
            self.deliverAnswer()
        }
        return true
    }

    /// Tell the page to accept, once: on audio activation or after the wait above, whichever is first.
    private func deliverAnswer() {
        guard let uuid = answering, let call = calls[uuid] else { return }
        answering = nil
        NSLog("pigeonpost: call answered room=\(call.ring.room) audio=\(audioActive)")
        host.answer(room: call.ring.room, ringToken: call.ring.ringToken)
    }

    /// The call page, loaded and signed in. When the app was woken by the push there is no window
    /// yet, so nothing else has attached it; the session comes back from the Keychain (readable
    /// after first unlock, so a locked phone can still answer) and the mailboxes are loaded here.
    private func prepareHost() async -> Bool {
        if host.isAttached { return true }
        guard let session, let account, let inbox else { return false }
        if Fixtures.enabled && !CallHost.debugEnabled { return false }
        guard session.status == .signedIn else { return false }
        if account.me == nil { await account.loadIdentities() }
        guard account.me != nil else { return false }
        host.attach(session: session, account: account, inbox: inbox)
        return true
    }

    /// Ready the shared audio session for a call. CallKit activates it; WebKit's capture and playback
    /// run inside it. Speaker by default for video, as every video calling app does.
    private func prepareAudio(video: Bool) {
        let audio = AVAudioSession.sharedInstance()
        do {
            try audio.setCategory(.playAndRecord, mode: video ? .videoChat : .voiceChat,
                                  options: video ? [.allowBluetooth, .defaultToSpeaker] : [.allowBluetooth])
        } catch {
            NSLog("pigeonpost: call audio session not configured: \(error.localizedDescription)")
        }
    }

    // ---- ending ------------------------------------------------------------------------------

    private func endAction(_ action: CXEndCallAction) {
        endHere(action.callUUID)
        action.fulfill()
    }

    /// Declined or hung up on this device's call screen.
    private func endHere(_ uuid: UUID) {
        guard let call = calls[uuid] else { return }
        NSLog("pigeonpost: call ended here room=\(call.ring.room) answered=\(call.answered)")
        forget(uuid)
        if call.answered {
            host.hangUp()
        } else if let token = call.ring.ringToken {
            // Declined from the call screen. Kept alive in the background long enough to say so:
            // the app may be suspended the moment the call screen goes.
            let page = host.pageURL
            let room = call.ring.room
            // Ended below in every case; the request gives up after 10 s, well inside the time the
            // system allows.
            let task = UIApplication.shared.beginBackgroundTask(withName: "decline", expirationHandler: nil)
            Task {
                let ok = await RingEndpoint.decline(page: page, room: room, ringToken: token)
                NSLog(ok ? "pigeonpost: declined room=\(room)" : "pigeonpost: decline not delivered; the ring will time out")
                UIApplication.shared.endBackgroundTask(task)
            }
        } else {
            host.decline(room: call.ring.room, ringToken: nil)
        }
    }

    private func muteAction(_ action: CXSetMutedCallAction) {
        guard calls[action.callUUID] != nil else { return action.fail() }
        setMuted(action.callUUID, action.isMuted)
        action.fulfill()
    }

    private func setMuted(_ uuid: UUID, _ muted: Bool) {
        guard var call = calls[uuid] else { return }
        // Equal when the page asked for this change itself (see `callPage`): nothing to pass back.
        if call.muted != muted {
            NSLog("pigeonpost: call muted=\(muted) from the call screen")
            call.muted = muted
            calls[uuid] = call
            host.setMuted(muted)
        }
    }

    // ---- DEBUG -------------------------------------------------------------------------------

    #if DEBUG
    /// The simulator's CallKit has no call screen: iOS 18's simulator ends every incoming call within
    /// a quarter of a second ("Disconnecting call because there wont be a UI to host the call"). With
    /// `-callkit-stand-in YES` (simulator, DEBUG only) the report is recorded here instead of made, and
    /// `-callkit-script "answer@3,mute@20,unmute@25,pagemute@28,end@30"` plays the person's taps — the same
    /// handlers CallKit's actions call — at those seconds after the ring.
    static let standIn: Bool = {
        #if targetEnvironment(simulator)
        return UserDefaults.standard.bool(forKey: "callkit-stand-in")
        #else
        return false
        #endif
    }()

    private func runScript(_ uuid: UUID) {
        guard let script = UserDefaults.standard.string(forKey: "callkit-script") else { return }
        for step in script.split(separator: ",") {
            let parts = step.split(separator: "@")
            guard parts.count == 2, let seconds = Double(parts[1]) else { continue }
            let what = String(parts[0])
            Task {
                try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
                NSLog("pigeonpost: stand-in tap \(what)")
                switch what {
                case "answer": self.answerCall(uuid)
                case "end", "decline": self.endHere(uuid)
                case "mute": self.setMuted(uuid, true)
                case "unmute": self.setMuted(uuid, false)
                // The page's own mute button, for the other direction (page → call screen).
                case "pagemute": self.host.debugRun("document.getElementById('call-mute').click()")
                default: break
                }
            }
        }
    }

    /// The simulator never receives a VoIP push. `-simulate-voip '<payload JSON>'` hands one to the
    /// same handler PushKit calls, at launch, so the CallKit flow can be driven there.
    /// `"expires_at": "+45"` means 45 seconds from now. Never compiled into a release.
    private func simulateIfAsked() {
        // Read from the command line itself: UserDefaults parses a `{…}` argument as a property list.
        let args = CommandLine.arguments
        guard let flag = args.firstIndex(of: "-simulate-voip"), flag + 1 < args.count,
              case let text = args[flag + 1],
              var payload = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any] else { return }
        if let relative = payload["expires_at"] as? String, relative.hasPrefix("+"), let seconds = Double(relative.dropFirst()) {
            payload["expires_at"] = Date().timeIntervalSince1970 + seconds
        }
        // Straight away, as PushKit delivers a launching push: on the main queue, right after the
        // registry exists. `-simulate-voip-delay S` waits first.
        let delay = UserDefaults.standard.double(forKey: "simulate-voip-delay")
        let deliver = { [payload] in
            NSLog("pigeonpost: simulated VoIP push")
            self.incoming(push: payload) { NSLog("pigeonpost: simulated VoIP push reported") }
        }
        if delay <= 0 { return deliver() }
        Task {
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            deliver()
        }
    }
    #endif
}

// ---- the page --------------------------------------------------------------------------------

extension CallAlerts: CallPageEvents {
    func callPage(_ type: String, _ message: [String: Any]) {
        switch type {
        case "ring":
            // The app is open and the page heard the ring first (or alone). With `nativeRing` the
            // page shows nothing; the system call screen — a banner in the foreground — does.
            if let ring = CallRing(page: message) { report(ring) }
        case "ring_ended":
            guard let room = message["room"] as? String else { return }
            let uuid = CallRing.uuid(forRoom: room)
            // Answered here: the ring ending is this device's own doing.
            guard let call = calls[uuid], !call.answered else { return }
            end(uuid, RingEnd(reason: message["reason"] as? String))
        case "call":
            guard let room = message["room"] as? String else { return }
            let uuid = CallRing.uuid(forRoom: room)
            guard calls[uuid] != nil else { return }
            switch message["state"] as? String {
            case "connecting", "active":
                calls[uuid]?.started = true
                calls[uuid]?.giveUp?.cancel()
            case "ended":
                end(uuid, .remoteEnded)
            default:
                break
            }
        case "muted":
            // Muted with the page's own button: tell CallKit, so its button agrees. The state is
            // recorded first, so the action coming back is recognised and not passed to the page.
            guard let muted = message["muted"] as? Bool,
                  let active = calls.first(where: { $0.value.answered }), active.value.muted != muted else { return }
            let uuid = active.key
            calls[uuid]?.muted = muted
            NSLog("pigeonpost: call muted=\(muted) from the page")
            #if DEBUG
            if Self.standIn { return }
            #endif
            controller.request(CXTransaction(action: CXSetMutedCallAction(call: uuid, muted: muted))) { error in
                if let error { NSLog("pigeonpost: mute not mirrored to the call screen: \(error.localizedDescription)") }
            }
        default:
            break
        }
    }
}

// ---- PushKit ---------------------------------------------------------------------------------

extension CallAlerts: PKPushRegistryDelegate {
    nonisolated func pushRegistry(_ registry: PKPushRegistry, didUpdate credentials: PKPushCredentials, for type: PKPushType) {
        guard type == .voIP else { return }
        let token = credentials.token
        MainActor.assumeIsolated {
            _ = Task { await PushDelegate.service.adopt(voipToken: token) }
        }
    }

    nonisolated func pushRegistry(_ registry: PKPushRegistry, didInvalidatePushTokenFor type: PKPushType) {
        guard type == .voIP else { return }
        MainActor.assumeIsolated {
            _ = Task { await PushDelegate.service.adopt(voipToken: nil) }
        }
    }

    /// Delivered on the main queue (the registry was made with it), so the report happens before
    /// this returns to the run loop.
    nonisolated func pushRegistry(_ registry: PKPushRegistry, didReceiveIncomingPushWith payload: PKPushPayload,
                                  for type: PKPushType, completion: @escaping () -> Void) {
        let info = payload.dictionaryPayload
        MainActor.assumeIsolated {
            guard type == .voIP else { return completion() }
            self.incoming(push: info, completion: completion)
        }
    }
}

// ---- CallKit ---------------------------------------------------------------------------------

extension CallAlerts: CXProviderDelegate {
    nonisolated func providerDidReset(_ provider: CXProvider) {
        MainActor.assumeIsolated {
            if calls.values.contains(where: \.answered) { host.hangUp() }
            for uuid in calls.keys { forget(uuid) }
            audioActive = false
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXAnswerCallAction) {
        MainActor.assumeIsolated { answer(action) }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
        MainActor.assumeIsolated { endAction(action) }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXSetMutedCallAction) {
        MainActor.assumeIsolated { muteAction(action) }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXSetHeldCallAction) {
        // Holding is not something the call page can do.
        action.fail()
    }

    /// CallKit has made the audio session live. Only now is the page told to answer, so its
    /// microphone capture starts inside a running session (see the note at the top).
    nonisolated func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
        MainActor.assumeIsolated {
            audioActive = true
            deliverAnswer()
        }
    }

    nonisolated func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
        MainActor.assumeIsolated { audioActive = false }
    }
}
