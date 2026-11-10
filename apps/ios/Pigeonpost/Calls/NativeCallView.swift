//  The screen for a call answered natively (`NativeCall`). It sits over the whole app, like the
//  call page's overlay, and appears when the app is opened; the system call screen covers the lock
//  screen until then.

import SwiftUI
import WebRTC

struct NativeCallOverlay: View {
    private var room: NativeCallRoom { .shared }

    var body: some View {
        if let call = room.call { NativeCallScreen(call: call) }
    }
}

private struct NativeCallScreen: View {
    let call: NativeCall

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            people.ignoresSafeArea()
            VStack(spacing: 0) {
                header
                Spacer()
                controls
            }
            .padding(.vertical, 12)
            selfView
        }
        .preferredColorScheme(.dark)
        .accessibilityIdentifier("native-call")
    }

    @ViewBuilder private var people: some View {
        switch call.people.count {
        case 0:
            PersonTile(name: call.title, track: nil, fill: true)
        case 1:
            PersonTile(name: call.people[0].name, track: call.people[0].video, fill: !call.people[0].sharing)
        default:
            let columns = [GridItem(.adaptive(minimum: 160), spacing: 2)]
            ScrollView {
                LazyVGrid(columns: columns, spacing: 2) {
                    ForEach(call.people) { person in
                        PersonTile(name: person.name, track: person.video, fill: true).frame(height: 240).clipped()
                    }
                }
            }
        }
    }

    private var header: some View {
        VStack(spacing: 4) {
            Text(call.people.count > 1 ? "\(call.people.count + 1) people" : (call.people.first?.name ?? call.title))
                .font(.headline)
            status.font(.subheadline).foregroundStyle(.white.opacity(0.8))
            if call.people.count == 1, let code = call.people[0].code {
                Text("Security code \(code)").font(.caption.monospacedDigit()).foregroundStyle(.white.opacity(0.6))
            }
            if let notice = call.notice {
                Text(notice).font(.caption).foregroundStyle(.white.opacity(0.8))
            }
        }
        .foregroundStyle(.white)
        .shadow(radius: 3)
        .padding(.horizontal)
    }

    @ViewBuilder private var status: some View {
        if let started = call.startedAt {
            if call.people.count == 1, call.people[0].muted {
                Text("Muted")
            } else {
                TimelineView(.periodic(from: started, by: 1)) { context in
                    Text(Self.clock(context.date.timeIntervalSince(started)))
                }
            }
        } else {
            Text("Connecting…")
        }
    }

    private static func clock(_ elapsed: TimeInterval) -> String {
        let total = max(0, Int(elapsed))
        return total >= 3600 ? String(format: "%d:%02d:%02d", total / 3600, total / 60 % 60, total % 60)
            : String(format: "%d:%02d", total / 60, total % 60)
    }

    private var controls: some View {
        HStack(spacing: 18) {
            control(call.muted ? "mic.slash.fill" : "mic.fill", call.muted ? "Unmute" : "Mute", on: call.muted) {
                call.setMuted(!call.muted)
            }
            control(call.speaker ? "speaker.wave.2.fill" : "speaker.fill", call.speaker ? "Use the earpiece" : "Use the speaker", on: call.speaker) {
                call.toggleSpeaker()
            }
            control(call.wantsCamera ? "video.fill" : "video.slash.fill", call.wantsCamera ? "Turn the camera off" : "Turn the camera on", on: !call.wantsCamera) {
                call.toggleCamera()
            }
            if call.cameraRunning {
                control("arrow.triangle.2.circlepath.camera.fill", "Switch camera", on: false) { call.flipCamera() }
            }
            Button {
                CallAlerts.shared.hangUpNativeCall()
            } label: {
                Image(systemName: "phone.down.fill")
                    .font(.title3)
                    .foregroundStyle(.white)
                    .frame(width: 60, height: 60)
                    .background(Circle().fill(.red))
            }
            .accessibilityLabel("End call")
        }
        .padding(.bottom, 8)
    }

    private func control(_ symbol: String, _ label: String, on: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.title3)
                .foregroundStyle(on ? .black : .white)
                .frame(width: 54, height: 54)
                .background(Circle().fill(on ? Color.white : Color.white.opacity(0.22)))
        }
        .accessibilityLabel(label)
    }

    @ViewBuilder private var selfView: some View {
        if let track = call.localVideo {
            VStack {
                HStack {
                    Spacer()
                    TrackView(track: track, fill: true, mirrored: true)
                        .frame(width: 104, height: 150)
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                        .overlay(RoundedRectangle(cornerRadius: 12).stroke(.white.opacity(0.4)))
                }
                Spacer()
            }
            .padding(.top, 70)
            .padding(.trailing, 14)
            .accessibilityLabel("Your camera")
        }
    }
}

private struct PersonTile: View {
    let name: String
    let track: RTCVideoTrack?
    let fill: Bool

    var body: some View {
        ZStack {
            Color(white: 0.12)
            if let track {
                TrackView(track: track, fill: fill, mirrored: false)
            } else {
                Text(String(name.trimmingCharacters(in: CharacterSet(charactersIn: "/@ ")).prefix(1)).uppercased())
                    .font(.system(size: 54, weight: .medium))
                    .foregroundStyle(.white)
                    .frame(width: 128, height: 128)
                    .background(Circle().fill(.white.opacity(0.16)))
            }
        }
    }
}

private struct TrackView: UIViewRepresentable {
    let track: RTCVideoTrack
    let fill: Bool
    let mirrored: Bool

    final class Coordinator { var track: RTCVideoTrack? }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> RTCMTLVideoView {
        let view = RTCMTLVideoView(frame: .zero)
        view.transform = mirrored ? CGAffineTransform(scaleX: -1, y: 1) : .identity
        return view
    }

    func updateUIView(_ view: RTCMTLVideoView, context: Context) {
        view.videoContentMode = fill ? .scaleAspectFill : .scaleAspectFit
        guard context.coordinator.track !== track else { return }
        context.coordinator.track?.remove(view)
        track.add(view)
        context.coordinator.track = track
    }

    static func dismantleUIView(_ view: RTCMTLVideoView, coordinator: Coordinator) {
        coordinator.track?.remove(view)
    }
}
