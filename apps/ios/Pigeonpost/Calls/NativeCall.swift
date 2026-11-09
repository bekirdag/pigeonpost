//  A call answered on the system call screen, with the media run by WebRTC in this process.
//
//  Why it exists: the call page's media runs in WebKit's own processes, outside the audio session
//  CallKit activates for this app. A call answered from a locked iPhone connected and carried nothing
//  (2026-10-30, 2026-11-04, 2026-11-09: the page's mic read "live", no audio left, and the voice it
//  received was never heard). Here capture and playback are ours, so they run in CallKit's session.
//
//  It speaks exactly what `site-inbox/calls.js` speaks: the same event stream and signals, the same
//  three m-lines in the same order (audio, camera, screen) and the same negotiated data channel, so
//  the far end cannot tell it from a page. The newcomer offers, and this end is always the newcomer
//  when it answers; a later joiner offers to it.
//
//  A room already on the media server (`needs_e2ee`) goes back to the page, which can do frame
//  encryption. Everything else about a call starts from `CallAlerts`.

import AVFoundation
import Observation
import UIKit
import WebRTC

@MainActor
protocol NativeCallEvents: AnyObject {
    func nativeCallStarted(_ call: NativeCall)
    func nativeCallEnded(_ call: NativeCall, reason: NativeCall.EndReason)
    /// This room needs the page; nothing has been accepted.
    func nativeCallNeedsPage(_ call: NativeCall)
    func nativeCallMuted(_ muted: Bool)
}

/// The call on screen, for `NativeCallOverlay`.
@MainActor @Observable
final class NativeCallRoom {
    static let shared = NativeCallRoom()
    var call: NativeCall?
}

@MainActor @Observable
final class NativeCall: NSObject {
    enum EndReason { case remoteEnded, failed }

    struct Tokens {
        let current: () async throws -> String
        let renew: () async throws -> String
    }

    /// Someone else in the call, as the screen shows them.
    @MainActor @Observable
    final class Person: Identifiable {
        let id: String
        var name: String
        var muted = false
        var cameraOff = true
        var sharing = false
        var connected = false
        var camera: RTCVideoTrack?
        var screen: RTCVideoTrack?
        var code: String?

        init(id: String, name: String) {
            self.id = id
            self.name = name
        }

        var video: RTCVideoTrack? { sharing ? (screen ?? camera) : (cameraOff ? nil : camera) }
    }

    let title: String
    let ring: CallRing
    private(set) var people: [Person] = []
    private(set) var muted = false
    private(set) var speaker: Bool
    private(set) var cameraRunning = false
    private(set) var wantsCamera: Bool
    private(set) var startedAt: Date?
    private(set) var notice: String?
    var localVideo: RTCVideoTrack? { cameraRunning ? cameraTrack : nil }

    @ObservationIgnored weak var events: NativeCallEvents?

    private final class Peer {
        let person: NativeCall.Person
        var pc: RTCPeerConnection?
        var channel: RTCDataChannel?
        var delegate: PeerDelegate?
        var slots: [RTCRtpTransceiver] = []
        let initiator: Bool
        var ready: Task<RTCPeerConnection?, Never>?
        var makingOffer = false
        var offerAgain = false
        var pendingIce: [[String: Any]] = []
        var connected = false
        var restarts = 0
        var relayOnly = false
        var watchdog: Task<Void, Never>?
        var drop: Task<Void, Never>?
        var local: [String: Int] = [:]
        var remote: [String: Int] = [:]

        init(person: NativeCall.Person, initiator: Bool) {
            self.person = person
            self.initiator = initiator
        }
    }

    @ObservationIgnored private let base: URL
    @ObservationIgnored private let tokens: Tokens
    @ObservationIgnored private let urls: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 3600
        configuration.timeoutIntervalForResource = 86400
        return URLSession(configuration: configuration)
    }()
    @ObservationIgnored private var session: String?
    @ObservationIgnored private var roomID: String
    @ObservationIgnored private var you: String?
    @ObservationIgnored private var peers: [String: Peer] = [:]
    @ObservationIgnored private var ice: [RTCIceServer]?
    @ObservationIgnored private var stream: Task<Void, Never>?
    @ObservationIgnored private var accepted = false
    @ObservationIgnored private var began = false
    @ObservationIgnored private var closed = false
    @ObservationIgnored private var observers: [NSObjectProtocol] = []

    @ObservationIgnored private let micTrack: RTCAudioTrack
    @ObservationIgnored private let videoSource: RTCVideoSource
    @ObservationIgnored private let cameraTrack: RTCVideoTrack
    @ObservationIgnored private let capturer: RTCCameraVideoCapturer
    @ObservationIgnored private var front = true

    private static let factory: RTCPeerConnectionFactory = {
        RTCInitializeSSL()
        return RTCPeerConnectionFactory(encoderFactory: RTCDefaultVideoEncoderFactory(), decoderFactory: RTCDefaultVideoDecoderFactory())
    }()
    private static let noConstraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
    private static let reconnectGrace: UInt64 = 15_000_000_000
    private static let watchdogDelay: UInt64 = 10_000_000_000

    /// Made inside the CallKit answer action, before it is fulfilled: audio has to be under manual
    /// control before CallKit activates the session.
    init(ring: CallRing, title: String, base: URL, tokens: Tokens) {
        self.ring = ring
        self.title = title
        self.base = base
        self.tokens = tokens
        roomID = ring.room
        speaker = ring.isVideo
        wantsCamera = ring.isVideo
        Self.configureAudio(video: ring.isVideo)
        let factory = Self.factory
        micTrack = factory.audioTrack(with: factory.audioSource(with: Self.noConstraints), trackId: "mic")
        videoSource = factory.videoSource()
        cameraTrack = factory.videoTrack(with: videoSource, trackId: "cam")
        capturer = RTCCameraVideoCapturer(delegate: videoSource)
        super.init()
        watchTheApp()
    }

    private static func configureAudio(video: Bool) {
        let audio = RTCAudioSession.sharedInstance()
        audio.useManualAudio = true
        audio.isAudioEnabled = false
        let configuration = RTCAudioSessionConfiguration.webRTC()
        configuration.category = AVAudioSession.Category.playAndRecord.rawValue
        configuration.mode = (video ? AVAudioSession.Mode.videoChat : AVAudioSession.Mode.voiceChat).rawValue
        configuration.categoryOptions = video ? [.allowBluetooth, .defaultToSpeaker] : [.allowBluetooth]
        RTCAudioSessionConfiguration.setWebRTC(configuration)
    }

    /// CallKit activated the audio session: only now may WebRTC start the audio unit.
    static func audioActivated(_ session: AVAudioSession) {
        let audio = RTCAudioSession.sharedInstance()
        audio.audioSessionDidActivate(session)
        audio.isAudioEnabled = true
    }

    static func audioDeactivated(_ session: AVAudioSession) {
        let audio = RTCAudioSession.sharedInstance()
        audio.audioSessionDidDeactivate(session)
        audio.isAudioEnabled = false
    }

    // ---- starting ----------------------------------------------------------------------------

    /// Open the event stream and accept once it has a session.
    func begin() {
        guard !began, !closed else { return }
        began = true
        if AVAudioApplication.shared.recordPermission == .undetermined {
            AVAudioApplication.requestRecordPermission { _ in }
        }
        NSLog("pigeonpost: native call begins room=\(ring.room)")
        startCameraIfPossible()
        stream = Task { [weak self] in await self?.runEvents() }
    }

    private func runEvents() async {
        var backoff: UInt64 = 1
        var renewed = false
        while !closed {
            do {
                var request = URLRequest(url: base.appendingPathComponent("events"))
                request.setValue("Bearer " + (try await tokens.current()), forHTTPHeaderField: "authorization")
                request.setValue("text/event-stream", forHTTPHeaderField: "accept")
                let (bytes, response) = try await urls.bytes(for: request)
                if closed { return }
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if status == 401, !renewed { renewed = true; _ = try await tokens.renew(); continue }
                guard status == 200 else { throw URLError(.badServerResponse) }
                renewed = false
                backoff = 1
                var parser = NativeCallProtocol.EventParser()
                for try await byte in bytes {
                    if closed { return }
                    if let event = parser.feed(byte) { handle(event) }
                }
            } catch {
                if closed { return }
            }
            if closed { return }
            session = nil
            try? await Task.sleep(nanoseconds: backoff * 1_000_000_000)
            backoff = min(backoff * 2, 30)
        }
    }

    private func handle(_ event: NativeCallProtocol.ServerEvent) {
        guard let value = try? JSONSerialization.jsonObject(with: Data(event.data.utf8)) as? [String: Any] else { return }
        let here = (value["room"] as? String) == roomID
        switch event.name {
        case "ready":
            guard let id = value["session"] as? String else { return }
            session = id
            if !accepted {
                accepted = true
                Task { await accept() }
            } else if let you {
                // The stream dropped and came back. The media never depended on it.
                Task { _ = try? await post("rooms/\(roomID)/resume", ["session": id, "participant": you]) }
            }
        case "joined":
            if here { joined(value) }
        case "participant_joined":
            guard here, let member = NativeCallProtocol.RoomView(["room": roomID, "participants": [value]])?.members.first else { return }
            ensurePeer(member, initiator: false)
        case "participant_left":
            guard here, let id = value["participant"] as? String, let peer = peers[id] else { return }
            close(peer)
        case "signal":
            guard here, let from = value["from"] as? String, let type = value["type"] as? String else { return }
            Task { await signal(from: from, type: type, data: value["data"]) }
        case "ring_ended":
            if here, peers.isEmpty, you == nil { finish(.remoteEnded) }
        case "ended":
            if here { finish(.remoteEnded) }
        default:
            break
        }
    }

    private func accept() async {
        guard let session else { return }
        var body: [String: Any] = ["session": session, "e2ee": false]
        if let token = ring.ringToken { body["ring_token"] = token }
        do {
            let view = try await post("rooms/\(roomID)/accept", body)
            joined(view)
        } catch let refused as Refused where refused.code == "needs_e2ee" {
            NSLog("pigeonpost: room needs the page room=\(ring.room)")
            let events = self.events
            shutDown()
            events?.nativeCallNeedsPage(self)
        } catch {
            NSLog("pigeonpost: native answer refused room=\(ring.room) \((error as? Refused)?.code ?? "network")")
            finish(.failed)
        }
    }

    private func joined(_ value: [String: Any]) {
        guard let view = NativeCallProtocol.RoomView(value), !closed else { return }
        if view.topology == "sfu" {
            let events = self.events
            shutDown()
            events?.nativeCallNeedsPage(self)
            return
        }
        roomID = view.room
        if you == nil {
            you = view.you
            events?.nativeCallStarted(self)
        }
        for member in view.others { ensurePeer(member, initiator: true) }
    }

    // ---- HTTP --------------------------------------------------------------------------------

    private struct Refused: Error { let code: String }

    @discardableResult
    private func post(_ path: String, _ body: [String: Any], renewed: Bool = false) async throws -> [String: Any] {
        try await send("POST", url: base.appendingPathComponent(path), body: body, renewed: renewed)
    }

    private func send(_ method: String, url: URL, body: [String: Any]?, renewed: Bool = false) async throws -> [String: Any] {
        var request = URLRequest(url: url, timeoutInterval: 20)
        request.httpMethod = method
        request.setValue("Bearer " + (try await tokens.current()), forHTTPHeaderField: "authorization")
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "content-type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await urls.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        if status == 401, !renewed {
            _ = try await tokens.renew()
            return try await send(method, url: url, body: body, renewed: true)
        }
        let value = (try? JSONSerialization.jsonObject(with: data) as? [String: Any]) ?? [:]
        guard (200..<300).contains(status) else { throw Refused(code: (value["error"] as? String) ?? "http_\(status)") }
        return value
    }

    private func iceServers() async -> [RTCIceServer] {
        if let ice { return ice }
        var found: [RTCIceServer] = []
        if let session, var parts = URLComponents(url: base.appendingPathComponent("turn"), resolvingAgainstBaseURL: true) {
            parts.queryItems = [URLQueryItem(name: "session", value: session)]
            if let url = parts.url, let value = try? await send("GET", url: url, body: nil),
               let servers = value["iceServers"] as? [[String: Any]] {
                for server in servers {
                    let list = (server["urls"] as? [String]) ?? (server["urls"] as? String).map { [$0] } ?? []
                    if !list.isEmpty {
                        found.append(RTCIceServer(urlStrings: list, username: server["username"] as? String, credential: server["credential"] as? String))
                    }
                }
            }
        }
        ice = found
        return found
    }

    private func signal(to peer: Peer, _ type: String, _ data: Any) async {
        guard let session else { return }
        _ = try? await post("rooms/\(roomID)/signal", ["session": session, "to": peer.person.id, "type": type, "data": data])
    }

    // ---- connections -------------------------------------------------------------------------

    @discardableResult
    private func ensurePeer(_ member: NativeCallProtocol.RoomView.Member, initiator: Bool) -> Peer {
        if let peer = peers[member.id] { return peer }
        let person = Person(id: member.id, name: member.name)
        let peer = Peer(person: person, initiator: initiator)
        peers[member.id] = peer
        people.append(person)
        if initiator { peer.ready = Task { await self.connection(for: peer, initiator: true) } }
        return peer
    }

    private func connection(for peer: Peer, initiator: Bool) async -> RTCPeerConnection? {
        let configuration = RTCConfiguration()
        configuration.iceServers = await iceServers()
        configuration.iceTransportPolicy = peer.relayOnly ? .relay : .all
        configuration.bundlePolicy = .maxBundle
        configuration.rtcpMuxPolicy = .require
        configuration.sdpSemantics = .unifiedPlan
        let delegate = PeerDelegate(owner: self, id: peer.person.id)
        guard !closed, peers[peer.person.id] === peer,
              let pc = Self.factory.peerConnection(with: configuration, constraints: Self.noConstraints, delegate: delegate) else { return nil }
        peer.delegate = delegate
        peer.pc = pc
        // A negotiated channel exists on both ends without either having to announce it.
        let channelSetup = RTCDataChannelConfiguration()
        channelSetup.isNegotiated = true
        channelSetup.channelId = 0
        channelSetup.isOrdered = true
        let channel = pc.dataChannel(forLabel: "pigeonpost", configuration: channelSetup)
        channel?.delegate = delegate
        peer.channel = channel
        if initiator {
            let both = RTCRtpTransceiverInit()
            both.direction = .sendRecv
            peer.slots = [RTCRtpMediaType.audio, .video, .video].compactMap { pc.addTransceiver(of: $0, init: both) }
            attachTracks(peer)
        }
        return pc
    }

    private func attachTracks(_ peer: Peer) {
        guard peer.slots.count >= 2 else { return }
        peer.slots[0].sender.track = micTrack
        peer.slots[1].sender.track = cameraTrack
    }

    private func negotiate(_ peer: Peer) async {
        guard let pc = peer.pc, peer.initiator else { return }
        if peer.makingOffer || pc.signalingState != .stable { peer.offerAgain = true; return }
        peer.makingOffer = true
        defer { peer.makingOffer = false }
        guard let offer = await describe(pc, offer: true), pc.signalingState == .stable else { peer.offerAgain = true; return }
        let tuned = RTCSessionDescription(type: .offer, sdp: NativeCallProtocol.tuneOpus(offer.sdp))
        guard await setLocal(pc, tuned), let sdp = pc.localDescription?.sdp else { return }
        await signal(to: peer, "offer", ["type": "offer", "sdp": sdp])
        armWatchdog(peer)
    }

    private func signal(from: String, type: String, data: Any?) async {
        let peer = peers[from] ?? ensurePeer(.init(id: from, name: "Someone"), initiator: false)
        switch type {
        case "mute":
            if !muted { setMuted(true); notice = "\(peer.person.name) muted you." }
        case "ice":
            guard let data = data as? [String: Any] else { return }
            guard let pc = peer.pc, pc.remoteDescription != nil else { peer.pendingIce.append(data); return }
            await add(data, to: pc)
        case "offer":
            guard !peer.initiator, let data = data as? [String: Any], let sdp = data["sdp"] as? String else { return }
            if peer.ready == nil { peer.ready = Task { await self.connection(for: peer, initiator: false) } }
            guard let pc = await peer.ready?.value else { return }
            guard await setRemote(pc, RTCSessionDescription(type: .offer, sdp: sdp)) else { return }
            if peer.slots.isEmpty {
                peer.slots = pc.transceivers
                for slot in peer.slots { var failure: NSError?; slot.setDirection(.sendRecv, error: &failure) }
            }
            attachTracks(peer)
            await flushIce(peer, pc)
            guard let answer = await describe(pc, offer: false) else { return }
            let tuned = RTCSessionDescription(type: .answer, sdp: NativeCallProtocol.tuneOpus(answer.sdp))
            guard await setLocal(pc, tuned), let local = pc.localDescription?.sdp else { return }
            await signal(to: peer, "answer", ["type": "answer", "sdp": local])
            armWatchdog(peer)
        case "answer":
            guard peer.initiator, let pc = peer.pc, pc.signalingState == .haveLocalOffer,
                  let data = data as? [String: Any], let sdp = data["sdp"] as? String else { return }
            guard await setRemote(pc, RTCSessionDescription(type: .answer, sdp: sdp)) else { return }
            await flushIce(peer, pc)
            if peer.offerAgain { peer.offerAgain = false; await negotiate(peer) }
        default:
            break
        }
    }

    private func flushIce(_ peer: Peer, _ pc: RTCPeerConnection) async {
        let waiting = peer.pendingIce
        peer.pendingIce = []
        for candidate in waiting { await add(candidate, to: pc) }
    }

    private func add(_ data: [String: Any], to pc: RTCPeerConnection) async {
        guard let sdp = data["candidate"] as? String, !sdp.isEmpty else { return }
        if let type = NativeCallProtocol.candidateType(sdp), let peer = peers.values.first(where: { $0.pc === pc }) {
            peer.remote[type, default: 0] += 1
        }
        let index = (data["sdpMLineIndex"] as? NSNumber)?.int32Value ?? 0
        let candidate = RTCIceCandidate(sdp: sdp, sdpMLineIndex: index, sdpMid: data["sdpMid"] as? String)
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            pc.add(candidate) { _ in done.resume() }
        }
    }

    private func describe(_ pc: RTCPeerConnection, offer: Bool) async -> RTCSessionDescription? {
        await withCheckedContinuation { (done: CheckedContinuation<RTCSessionDescription?, Never>) in
            if offer { pc.offer(for: Self.noConstraints) { sdp, _ in done.resume(returning: sdp) } }
            else { pc.answer(for: Self.noConstraints) { sdp, _ in done.resume(returning: sdp) } }
        }
    }

    private func setLocal(_ pc: RTCPeerConnection, _ sdp: RTCSessionDescription) async -> Bool {
        await withCheckedContinuation { (done: CheckedContinuation<Bool, Never>) in
            pc.setLocalDescription(sdp) { error in done.resume(returning: error == nil) }
        }
    }

    private func setRemote(_ pc: RTCPeerConnection, _ sdp: RTCSessionDescription) async -> Bool {
        await withCheckedContinuation { (done: CheckedContinuation<Bool, Never>) in
            pc.setRemoteDescription(sdp) { error in done.resume(returning: error == nil) }
        }
    }

    // A pair that has not connected gets fresh candidates, then the relay alone. As calls.js does,
    // only the side that started the connection restarts it.
    private func armWatchdog(_ peer: Peer) {
        guard peer.watchdog == nil, !peer.connected else { return }
        peer.watchdog = Task { [weak self, weak peer] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: Self.watchdogDelay)
                guard !Task.isCancelled, let self, let peer, !peer.connected, let pc = peer.pc else { return }
                self.report(peer, state: "stalled", route: nil)
                if peer.initiator, peer.restarts < 4 {
                    peer.restarts += 1
                    if peer.restarts >= 2, !peer.relayOnly {
                        peer.relayOnly = true
                        let configuration = pc.configuration
                        configuration.iceTransportPolicy = .relay
                        pc.setConfiguration(configuration)
                    }
                    pc.restartIce()
                }
            }
        }
    }

    private func close(_ peer: Peer) {
        peer.watchdog?.cancel()
        peer.drop?.cancel()
        peer.channel?.close()
        peer.pc?.close()
        peer.pc = nil
        peers[peer.person.id] = nil
        people.removeAll { $0.id == peer.person.id }
    }

    // ---- what PeerDelegate reports -----------------------------------------------------------

    fileprivate func peerNeedsNegotiation(_ id: String) {
        guard let peer = peers[id], peer.initiator else { return }
        Task { await negotiate(peer) }
    }

    fileprivate func peerCandidate(_ id: String, _ candidate: RTCIceCandidate) {
        guard let peer = peers[id] else { return }
        if let type = NativeCallProtocol.candidateType(candidate.sdp) { peer.local[type, default: 0] += 1 }
        var data: [String: Any] = ["candidate": candidate.sdp, "sdpMLineIndex": candidate.sdpMLineIndex]
        if let mid = candidate.sdpMid { data["sdpMid"] = mid }
        Task { await signal(to: peer, "ice", data) }
    }

    fileprivate func peerTrack(_ id: String, _ transceiver: RTCRtpTransceiver) {
        guard let peer = peers[id], let pc = peer.pc,
              let track = transceiver.receiver.track as? RTCVideoTrack else { return }
        let slot = pc.transceivers.firstIndex { $0.mid == transceiver.mid }
        if slot == 2 { peer.person.screen = track } else { peer.person.camera = track }
    }

    fileprivate func peerState(_ id: String, _ state: RTCPeerConnectionState) {
        guard let peer = peers[id], let pc = peer.pc else { return }
        switch state {
        case .connected:
            peer.watchdog?.cancel()
            peer.watchdog = nil
            peer.drop?.cancel()
            peer.drop = nil
            peer.person.connected = true
            guard !peer.connected else { return }
            peer.connected = true
            if startedAt == nil { startedAt = Date() }
            peer.person.code = NativeCallProtocol.securityCode(localSDP: pc.localDescription?.sdp, remoteSDP: pc.remoteDescription?.sdp)
            Task { [weak self] in
                try? await Task.sleep(nanoseconds: 2_500_000_000)
                await self?.reportConnected(peer)
            }
            scheduleMediaReports(peer)
        case .disconnected, .failed:
            peer.person.connected = false
            if peer.initiator, peer.restarts < 3, state == .failed || !peer.connected {
                peer.restarts += 1
                pc.restartIce()
            }
            guard peer.drop == nil else { return }
            peer.drop = Task { [weak self, weak peer] in
                try? await Task.sleep(nanoseconds: Self.reconnectGrace)
                guard !Task.isCancelled, let self, let peer, peer.pc?.connectionState != .connected else { return }
                self.close(peer)
                if self.peers.isEmpty { self.hangUp(); self.events?.nativeCallEnded(self, reason: .failed) }
            }
        default:
            break
        }
    }

    fileprivate func peerChannelOpen(_ id: String) {
        if let peer = peers[id] { sendState(peer) }
    }

    fileprivate func peerMessage(_ id: String, _ data: Data) {
        guard data.count <= 8000, let peer = peers[id],
              let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any], value["t"] as? String == "state" else { return }
        peer.person.muted = (value["muted"] as? Bool) ?? false
        peer.person.cameraOff = (value["cameraOff"] as? Bool) ?? false
        peer.person.sharing = (value["screen"] as? Bool) ?? false
    }

    private func sendState(_ peer: Peer) {
        guard let channel = peer.channel, channel.readyState == .open,
              let data = try? JSONSerialization.data(withJSONObject: ["t": "state", "muted": muted, "cameraOff": !cameraRunning, "screen": false, "aec": true] as [String: Any]) else { return }
        channel.sendData(RTCDataBuffer(data: data, isBinary: false))
    }

    private func broadcastState() { peers.values.forEach(sendState) }

    // ---- the service log ---------------------------------------------------------------------

    private func report(_ peer: Peer, state: String, route: String?) {
        guard let session else { return }
        let body: [String: Any] = ["session": session, "to": peer.person.id, "state": state, "local": peer.local, "remote": peer.remote,
                                   "relay_only": peer.relayOnly, "restarts": peer.restarts, "route": route ?? NSNull()]
        Task { _ = try? await post("rooms/\(roomID)/report", body) }
    }

    private func reportConnected(_ peer: Peer) async {
        guard let pc = peer.pc else { return }
        let stats = await statistics(pc)
        var route: String?
        if let transport = stats.values.first(where: { $0.type == "transport" }),
           let pairID = transport.values["selectedCandidatePairId"] as? String, let pair = stats[pairID],
           let localID = pair.values["localCandidateId"] as? String, let remoteID = pair.values["remoteCandidateId"] as? String {
            let relay = [localID, remoteID].contains { stats[$0]?.values["candidateType"] as? String == "relay" }
            route = relay ? "relay" : "direct"
        }
        report(peer, state: "connected", route: route)
    }

    private func statistics(_ pc: RTCPeerConnection) async -> [String: RTCStatistics] {
        await withCheckedContinuation { (done: CheckedContinuation<[String: RTCStatistics], Never>) in
            pc.statistics { done.resume(returning: $0.statistics) }
        }
    }

    private struct Reading {
        var sentAudio = 0.0, sentVideo = 0.0, gotAudio = 0.0, energy = 0.0, frames = 0.0
    }

    private func reading(_ pc: RTCPeerConnection) async -> Reading {
        var r = Reading()
        for stat in await statistics(pc).values {
            let number = { (key: String) in (stat.values[key] as? NSNumber)?.doubleValue ?? 0 }
            let kind = stat.values["kind"] as? String
            if stat.type == "outbound-rtp" {
                if kind == "audio" { r.sentAudio += number("bytesSent") } else { r.sentVideo += number("bytesSent") }
            } else if stat.type == "inbound-rtp" {
                if kind == "audio" { r.gotAudio += number("bytesReceived"); r.energy += number("totalAudioEnergy") }
                else { r.frames += number("framesDecoded") }
            }
        }
        return r
    }

    /// The same two readings the page reports, so a call can be diagnosed the same way.
    private func scheduleMediaReports(_ peer: Peer) {
        for at in [6, 24] {
            Task { [weak self, weak peer] in
                try? await Task.sleep(nanoseconds: UInt64(at) * 1_000_000_000)
                guard let pc = peer?.pc, let a = await self?.reading(pc) else { return }
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                guard let self, let peer, peer.pc === pc, let session = self.session else { return }
                let b = await self.reading(pc)
                let media: [String: Any] = [
                    "at": at, "host": "ios-native", "vis": UIApplication.shared.applicationState == .active ? "visible" : "hidden",
                    "mic": self.muted ? "off" : "live", "sent": "raw", "cam": self.cameraRunning ? "live" : (self.wantsCamera ? "later" : "none"),
                    "out_audio": b.sentAudio > a.sentAudio, "out_video": b.sentVideo > a.sentVideo, "in_audio": b.gotAudio > a.gotAudio,
                    "in_energy": b.energy > a.energy, "in_video": b.frames > a.frames, "voice": b.gotAudio > a.gotAudio ? "playing" : "stalled", "revived": [String](),
                ]
                _ = try? await self.post("rooms/\(self.roomID)/report", ["session": session, "to": peer.person.id, "state": "media", "media": media])
            }
        }
    }

    // ---- the controls ------------------------------------------------------------------------

    func setMuted(_ value: Bool, tellCallKit: Bool = true) {
        guard muted != value else { return }
        muted = value
        micTrack.isEnabled = !value
        broadcastState()
        if tellCallKit { events?.nativeCallMuted(value) }
    }

    func toggleSpeaker() {
        speaker.toggle()
        try? AVAudioSession.sharedInstance().overrideOutputAudioPort(speaker ? .speaker : .none)
        applyProximity()
    }

    func toggleCamera() {
        wantsCamera.toggle()
        if wantsCamera { startCameraIfPossible() } else { stopCamera() }
        applyProximity()
    }

    func flipCamera() {
        front.toggle()
        guard cameraRunning else { return }
        capturer.stopCapture { [weak self] in
            Task { @MainActor in
                self?.cameraRunning = false
                self?.startCameraIfPossible()
            }
        }
    }

    private func startCameraIfPossible() {
        // The camera cannot run from the lock screen or the background; it starts when the app opens.
        guard wantsCamera, !cameraRunning, !closed, UIApplication.shared.applicationState == .active else { return }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            break
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                guard granted else { return }
                Task { @MainActor [weak self] in self?.startCameraIfPossible() }
            }
            return
        default:
            wantsCamera = false
            notice = "The camera is turned off for Pigeonpost in Settings."
            return
        }
        let position: AVCaptureDevice.Position = front ? .front : .back
        guard let device = RTCCameraVideoCapturer.captureDevices().first(where: { $0.position == position }),
              let format = Self.bestFormat(for: device) else { return }
        cameraRunning = true
        capturer.startCapture(with: device, format: format, fps: 30) { [weak self] error in
            Task { @MainActor in
                guard let self else { return }
                if error != nil { self.cameraRunning = false }
                self.broadcastState()
            }
        }
    }

    private func stopCamera() {
        guard cameraRunning else { return }
        cameraRunning = false
        capturer.stopCapture()
        broadcastState()
    }

    private static func bestFormat(for device: AVCaptureDevice) -> AVCaptureDevice.Format? {
        let formats = RTCCameraVideoCapturer.supportedFormats(for: device)
        let usable = formats.filter { format in
            let size = CMVideoFormatDescriptionGetDimensions(format.formatDescription)
            return size.height <= 720 && format.videoSupportedFrameRateRanges.contains { $0.maxFrameRate >= 30 }
        }
        return usable.max { a, b in
            CMVideoFormatDescriptionGetDimensions(a.formatDescription).height < CMVideoFormatDescriptionGetDimensions(b.formatDescription).height
        } ?? formats.first
    }

    private func applyProximity() {
        UIDevice.current.isProximityMonitoringEnabled = !closed && !speaker && !wantsCamera
        UIApplication.shared.isIdleTimerDisabled = !closed && wantsCamera
    }

    private func watchTheApp() {
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.startCameraIfPossible()
                self?.applyProximity()
            }
        })
        observers.append(center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.stopCamera() }
        })
        applyProximity()
    }

    // ---- ending ------------------------------------------------------------------------------

    /// This person hung up. Kept alive long enough to tell the room: the app may be suspended as
    /// soon as the system call screen goes.
    func hangUp() {
        guard !closed else { return }
        if let session {
            let room = roomID
            let background = UIApplication.shared.beginBackgroundTask(withName: "leave-call", expirationHandler: nil)
            Task {
                _ = try? await post("rooms/\(room)/leave", ["session": session])
                UIApplication.shared.endBackgroundTask(background)
            }
        }
        shutDown()
    }

    private func finish(_ reason: EndReason) {
        guard !closed else { return }
        shutDown()
        events?.nativeCallEnded(self, reason: reason)
    }

    func shutDown() {
        guard !closed else { return }
        closed = true
        stream?.cancel()
        peers.values.forEach(close)
        stopCamera()
        RTCAudioSession.sharedInstance().isAudioEnabled = false
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        applyProximity()
        if NativeCallRoom.shared.call === self { NativeCallRoom.shared.call = nil }
        NSLog("pigeonpost: native call closed room=\(ring.room)")
    }
}

/// WebRTC calls this on its own thread; everything is handed to the call on the main actor.
private final class PeerDelegate: NSObject, RTCPeerConnectionDelegate, RTCDataChannelDelegate {
    private weak var owner: NativeCall?
    private let id: String

    init(owner: NativeCall, id: String) {
        self.owner = owner
        self.id = id
    }

    private func main(_ work: @escaping @MainActor (NativeCall, String) -> Void) {
        let id = id
        DispatchQueue.main.async { [weak owner] in
            MainActor.assumeIsolated { if let owner { work(owner, id) } }
        }
    }

    func peerConnectionShouldNegotiate(_ peerConnection: RTCPeerConnection) { main { $0.peerNeedsNegotiation($1) } }
    func peerConnection(_ peerConnection: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) { main { $0.peerCandidate($1, candidate) } }
    func peerConnection(_ peerConnection: RTCPeerConnection, didStartReceivingOn transceiver: RTCRtpTransceiver) { main { $0.peerTrack($1, transceiver) } }
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCPeerConnectionState) { main { $0.peerState($1, newState) } }

    func peerConnection(_ peerConnection: RTCPeerConnection, didChange stateChanged: RTCSignalingState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceConnectionState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceGatheringState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didOpen dataChannel: RTCDataChannel) {}

    func dataChannelDidChangeState(_ dataChannel: RTCDataChannel) {
        if dataChannel.readyState == .open { main { $0.peerChannelOpen($1) } }
    }

    func dataChannel(_ dataChannel: RTCDataChannel, didReceiveMessageWith buffer: RTCDataBuffer) {
        guard !buffer.isBinary else { return }
        let data = buffer.data
        main { $0.peerMessage($1, data) }
    }
}
