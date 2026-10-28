//  The group's side of a live stream (Phase 6): the red LIVE banner with Watch, and the Go live
//  sheet. Both hand over to the call web view (`CallHost.watchStream` / `goLive`), which runs the
//  stream itself — full screen, encrypted end to end — exactly as the web inbox does (groups.js
//  `renderLive`, `buildGoLiveSheet`).

import AVFoundation
import SwiftUI

struct GroupLiveBanner: View {
    let group: GroupSummary
    let stream: GroupStream

    @Environment(GroupStore.self) private var groups
    @Environment(Account.self) private var account
    @Environment(CallHost.self) private var calls
    @State private var opening = false

    var body: some View {
        let me = account.me?.address
        let action = LiveStreams.bannerAction(stream, me: me, current: calls.live?.stream)
        Button { Task { await open() } } label: {
            HStack(spacing: 10) {
                Text("LIVE")
                    .font(.geist(11, .bold))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 7).padding(.vertical, 3)
                    .background(Color.red, in: RoundedRectangle(cornerRadius: 5))
                Text(LiveStreams.bannerText(stream, me: me, name: groups.name))
                    .font(.geist(13.5, .medium))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if opening { ProgressView().controlSize(.small) }
                Text(action)
                    .font(.geist(13, .semibold))
                    .foregroundStyle(Theme.accent)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity)
            .background(Theme.field)
            .overlay(alignment: .bottom) { Rectangle().fill(Theme.rule).frame(height: 1) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(opening)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(LiveStreams.bannerText(stream, me: me, name: groups.name) + ". " + action)
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("groupLiveBanner")
    }

    private func open() async {
        guard !opening else { return }
        opening = true
        defer { opening = false }
        AudioRoute.prepareToWatch()
        let refusal = await calls.watchStream(group: group.id, name: group.name, stream: stream.id)
        if let text = LiveStreams.explain(refusal) { groups.say(text) }
        if refusal != nil { groups.streamChanged(group.id) }
    }
}

/// "Go live": a title and what to share, then the call view takes over.
struct GoLiveSheet: View {
    let group: GroupSummary

    @Environment(GroupStore.self) private var groups
    @Environment(CallHost.self) private var calls
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var camera = true
    @State private var screen = false
    @State private var canShare = false
    @State private var starting = false
    @State private var problem: String?
    @FocusState private var titleFocused: Bool

    private var ready: Bool { !LiveStreams.cleanTitle(title).isEmpty && (camera || screen) && !starting }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("What's this stream about?", text: $title)
                        .focused($titleFocused)
                        .accessibilityIdentifier("goLiveTitle")
                        .onSubmit { if ready { start() } }
                } header: { Text("Title") }
                Section {
                    Toggle("Camera and microphone", isOn: $camera)
                        .accessibilityIdentifier("goLiveCamera")
                    if canShare {
                        Toggle("Share my screen", isOn: $screen)
                            .accessibilityIdentifier("goLiveScreen")
                    }
                } header: { Text("Share") } footer: {
                    Text("Everyone in \(group.name) can watch. The stream is end-to-end encrypted and isn't recorded; it can run for up to 4 hours.")
                }
                if let problem {
                    Section { Text(problem).foregroundStyle(Theme.amber).font(.geist(13)) }
                        .accessibilityIdentifier("goLiveProblem")
                }
            }
            .navigationTitle("Go live")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Go live") { start() }
                        .disabled(!ready)
                        .accessibilityIdentifier("goLiveStart")
                }
            }
            .task {
                canShare = await calls.canShareScreen()
                titleFocused = true
            }
        }
        #if os(macOS)
        .frame(minWidth: 420, minHeight: 320)
        #endif
    }

    private func start() {
        guard ready else { return }
        let clean = LiveStreams.cleanTitle(title)
        if clean.isEmpty { problem = "Give your stream a title."; return }
        if !camera && !screen { problem = "Choose camera, screen or both."; return }
        starting = true
        problem = nil
        // Held directly: the sheet is gone before the call view answers.
        let calls = calls, groups = groups, group = group, camera = camera, screen = screen
        Task {
            // Ask in the app's own words before the page opens anything; a refusal stays a refusal.
            guard await MediaAccess.allowed(camera: camera) else {
                starting = false
                problem = LiveStreams.explain("camera_denied")
                return
            }
            AudioRoute.prepareToBroadcast()
            dismiss()
            // The sheet has to be gone before the call view covers the screen.
            try? await Task.sleep(nanoseconds: 450_000_000)
            let refusal = await calls.goLive(group: group.id, name: group.name, title: clean, camera: camera, screen: screen)
            if let text = LiveStreams.explain(refusal) { groups.say(text) }
            groups.streamChanged(group.id)
        }
    }
}

/// The system's camera and microphone permission, asked once, before a stream opens either.
@MainActor
enum MediaAccess {
    static func allowed(camera: Bool) async -> Bool {
        #if DEBUG
        // UI tests and the synthetic stream harness never touch this device's camera or microphone.
        if Fixtures.enabled || CallHost.debugEnabled { return true }
        #endif
        let kinds: [AVMediaType] = camera ? [.audio, .video] : [.audio]
        for kind in kinds {
            switch AVCaptureDevice.authorizationStatus(for: kind) {
            case .authorized: continue
            case .notDetermined:
                guard await AVCaptureDevice.requestAccess(for: kind) else { return false }
            default: return false
            }
        }
        return true
    }
}

/// How the stream sounds on a phone. A viewer only listens: playback, which plays with the ring
/// switch on silent, like any video. Someone on the stage talks: the call's own play-and-record
/// (CallKit takes over the session from there, see CallAlerts). The Mac has no audio session.
enum AudioRoute {
    static func prepareToWatch() {
        #if os(iOS)
        let audio = AVAudioSession.sharedInstance()
        // Never under a call or a stream that is already talking.
        guard audio.category != .playAndRecord else { return }
        try? audio.setCategory(.playback, mode: .moviePlayback, options: [.allowAirPlay, .allowBluetoothA2DP])
        #endif
    }

    static func prepareToBroadcast() {
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setCategory(.playAndRecord, mode: .videoChat,
                                                         options: [.allowBluetooth, .defaultToSpeaker])
        #endif
    }
}
