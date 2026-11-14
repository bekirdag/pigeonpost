//  Calls: PPCallButton, PPCallControls (`.pp-callbar`) and PPIncomingCallView (`.pp-call-ring`).

import SwiftUI

public enum PPCallButtonKind: Sendable { case standard, active, off, leave, answer, decline, sharing }

/// A round call control (48pt; 36 small, 64 large). Leave is a red pill with a label.
public struct PPCallButton: View {
    let icon: PPIconName
    let label: String
    let kind: PPCallButtonKind
    let size: CGFloat
    let caption: String?
    let onSurface: Bool
    let action: () -> Void

    public init(_ icon: PPIconName, label: String, kind: PPCallButtonKind = .standard, size: CGFloat = 48,
                caption: String? = nil, onSurface: Bool = false, action: @escaping () -> Void) {
        self.icon = icon
        self.label = label
        self.kind = kind
        self.size = size
        self.caption = caption
        self.onSurface = onSurface
        self.action = action
    }

    private var colors: (Color, Color) {
        switch kind {
        case .standard: return onSurface ? (PP.Colors.surfaceSunken, PP.Colors.text) : (Color.white.opacity(0.12), .white)
        case .active: return (PP.Colors.accent, PP.Colors.onAccent)
        case .off: return onSurface ? (PP.Colors.text, PP.Colors.surface) : (Color(ppHex: 0xF2F4F7), Color(ppHex: 0x12161C))
        case .leave, .decline, .sharing: return (PP.Colors.dangerSolid, .white)
        case .answer: return (PP.Colors.successSolid, .white)
        }
    }

    public var body: some View {
        let (bg, fg) = colors
        let iconSize: CGFloat = size >= 64 ? 28 : (size <= 36 ? 18 : 22)
        let rotate = kind == .leave || kind == .decline
        VStack(spacing: caption == nil ? 0 : 6) {
            Button(action: action) {
                if kind == .leave {
                    HStack(spacing: 8) {
                        PPIcon(.phone, size: iconSize).rotationEffect(.degrees(135))
                        Text(label).font(.geist(14, .semibold))
                    }
                    .padding(.horizontal, 18)
                    .frame(minWidth: 64, minHeight: size)
                } else {
                    PPIcon(icon, size: iconSize)
                        .rotationEffect(.degrees(rotate ? 135 : 0))
                        .frame(width: size, height: size)
                }
            }
            .buttonStyle(PPCallButtonStyle(fill: bg, ink: fg, pill: kind == .leave))
            .accessibilityLabel(Text(label))
            .accessibilityAddTraits(kind == .active ? .isSelected : [])
            .accessibilityValue(kind == .off ? Text("Off") : Text(""))
            if let caption {
                Text(caption)
                    .font(.geist(11.5))
                    .foregroundStyle(onSurface ? PP.Colors.textMuted : Color.white.opacity(0.8))
                    .accessibilityHidden(true)
            }
        }
    }
}

struct PPCallButtonStyle: ButtonStyle {
    let fill: Color
    let ink: Color
    let pill: Bool
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(ink)
            .background(Capsule().fill(fill))
            .brightness(configuration.isPressed ? 0.06 : 0)
            .scaleEffect(configuration.isPressed ? 0.94 : 1)
            .contentShape(Capsule())
            .animation(.easeOut(duration: PP.Duration.fast), value: configuration.isPressed)
    }
}

/// The in-call bar: microphone, camera, speaker, screen share and leave, on a dark glass capsule
/// (or on a surface with `onSurface`). `labeled` adds captions under 56pt buttons (mobile).
public struct PPCallControls: View {
    @Binding var micOn: Bool
    @Binding var cameraOn: Bool
    @Binding var speakerOn: Bool
    @Binding var sharing: Bool
    let labeled: Bool
    let onSurface: Bool
    let showsShare: Bool
    let onLeave: () -> Void

    public init(micOn: Binding<Bool>, cameraOn: Binding<Bool>, speakerOn: Binding<Bool> = .constant(false),
                sharing: Binding<Bool> = .constant(false), labeled: Bool = false, onSurface: Bool = false,
                showsShare: Bool = true, onLeave: @escaping () -> Void) {
        self._micOn = micOn
        self._cameraOn = cameraOn
        self._speakerOn = speakerOn
        self._sharing = sharing
        self.labeled = labeled
        self.onSurface = onSurface
        self.showsShare = showsShare
        self.onLeave = onLeave
    }

    public var body: some View {
        let size: CGFloat = labeled ? 56 : 48
        HStack(alignment: .top, spacing: labeled ? 14 : 10) {
            PPCallButton(micOn ? .mic : .micOff, label: micOn ? "Mute" : "Unmute", kind: micOn ? .standard : .off, size: size,
                         caption: labeled ? (micOn ? "Mute" : "Unmute") : nil, onSurface: onSurface) { micOn.toggle() }
            PPCallButton(cameraOn ? .video : .videoOff, label: cameraOn ? "Turn camera off" : "Turn camera on", kind: cameraOn ? .standard : .off,
                         size: size, caption: labeled ? "Camera" : nil, onSurface: onSurface) { cameraOn.toggle() }
            PPCallButton(.volume2, label: "Speaker", kind: speakerOn ? .active : .standard, size: size,
                         caption: labeled ? "Speaker" : nil, onSurface: onSurface) { speakerOn.toggle() }
            if showsShare {
                PPCallButton(.screenShare, label: sharing ? "Stop sharing" : "Share screen", kind: sharing ? .sharing : .standard, size: size,
                             caption: labeled ? "Share" : nil, onSurface: onSurface) { sharing.toggle() }
            }
            if labeled {
                PPCallButton(.phone, label: "Leave call", kind: .decline, size: size, caption: "Leave", onSurface: onSurface, action: onLeave)
            } else {
                PPCallButton(.phoneOff, label: "Leave", kind: .leave, size: size, onSurface: onSurface, action: onLeave)
            }
        }
        .padding(labeled ? EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14) : EdgeInsets(top: 8, leading: 8, bottom: 8, trailing: 8))
        .background {
            let shape = RoundedRectangle(cornerRadius: labeled ? PP.Radius.x2xl : 999, style: .continuous)
            if onSurface {
                shape.fill(PP.Colors.surface).overlay(shape.strokeBorder(PP.Colors.border))
            } else {
                shape.fill(.ultraThinMaterial).environment(\.colorScheme, .dark)
                    .overlay(shape.fill(Color(ppHex: 0x141820, opacity: 0.72)))
                    .overlay(shape.strokeBorder(Color.white.opacity(0.08)))
            }
        }
        .ppShadow(onSurface ? .two : .three)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Call controls"))
    }
}

public enum PPCallKind: Sendable { case voice, video }

/// The full-screen incoming call: kind, avatar, name, handle, quick actions and the
/// decline / answer pair (64pt). The answer button bobs unless Reduce Motion is on.
public struct PPIncomingCallView: View {
    let peer: String
    let name: String?
    let kind: PPCallKind
    let subtitle: String?
    let onDecline: () -> Void
    let onAnswer: () -> Void
    let onMessage: (() -> Void)?
    let onRemind: (() -> Void)?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var bob = false

    public init(peer: String, name: String? = nil, kind: PPCallKind = .voice, subtitle: String? = nil,
                onDecline: @escaping () -> Void, onAnswer: @escaping () -> Void,
                onMessage: (() -> Void)? = nil, onRemind: (() -> Void)? = nil) {
        self.peer = peer
        self.name = name
        self.kind = kind
        self.subtitle = subtitle
        self.onDecline = onDecline
        self.onAnswer = onAnswer
        self.onMessage = onMessage
        self.onRemind = onRemind
    }

    public var body: some View {
        VStack(spacing: 0) {
            VStack(spacing: 10) {
                HStack(spacing: 6) {
                    PPIcon(kind == .video ? .video : .phoneIncoming, size: 15)
                    Text(kind == .video ? "Pigeonpost video call" : "Pigeonpost voice call")
                }
                .font(.geist(13))
                .foregroundStyle(PP.Colors.textMuted)
                .padding(.bottom, 18)
                PPAvatar(peer: peer, name: name, size: 96)
                Text(name ?? PPPeer.displayName(peer))
                    .font(.geist(30, .semibold)).tracking(-0.3)
                    .foregroundStyle(PP.Colors.text)
                    .multilineTextAlignment(.center)
                HStack(spacing: 4) {
                    Text(peer).font(.geistMono(14)).foregroundStyle(PP.Colors.textBody)
                    if let subtitle { Text("· \(subtitle)").font(.geist(14)).foregroundStyle(PP.Colors.textMuted) }
                }
            }
            .padding(.top, 72)
            .accessibilityElement(children: .combine)
            .accessibilityLabel(Text("Incoming \(kind == .video ? "video" : "voice") call from \(name ?? PPPeer.displayName(peer)), \(peer)"))
            Spacer()
            if onMessage != nil || onRemind != nil {
                HStack(spacing: 48) {
                    if let onRemind { quick(.clock, "Remind me", onRemind) }
                    if let onMessage { quick(.messageSquare, "Message", onMessage) }
                }
                .padding(.bottom, 36)
            }
            HStack(alignment: .top) {
                Spacer()
                PPCallButton(.phone, label: "Decline", kind: .decline, size: 64, caption: "Decline", onSurface: true, action: onDecline)
                Spacer()
                PPCallButton(kind == .video ? .video : .phone, label: "Answer", kind: .answer, size: 64, caption: "Answer", onSurface: true, action: onAnswer)
                    .offset(y: bob ? -4 : 0)
                    .onAppear {
                        guard !reduceMotion else { return }
                        withAnimation(.easeInOut(duration: 0.8).repeatForever(autoreverses: true)) { bob = true }
                    }
                Spacer()
            }
            .padding(.bottom, 44)
        }
        .padding(.horizontal, 28)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background {
            ZStack {
                PP.Colors.bg
                RadialGradient(colors: [PP.Colors.accent.opacity(0.30), .clear], center: UnitPoint(x: 0.5, y: 0.2), startRadius: 0, endRadius: 420)
            }
            .ignoresSafeArea()
        }
    }

    private func quick(_ icon: PPIconName, _ label: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 6) {
                PPIcon(icon, size: 22)
                    .frame(width: 48, height: 48)
                    .background(Circle().fill(PP.Colors.surfaceSunken))
                Text(label).font(.geist(13))
            }
            .foregroundStyle(PP.Colors.textBody)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(label))
    }
}

#Preview("Incoming call") {
    PPIncomingCallView(peer: "/ada", name: "Ada Yılmaz", kind: .video, subtitle: "Wodo", onDecline: {}, onAnswer: {}, onMessage: {}, onRemind: {})
        .pigeonpostTheme()
}

#Preview("Call controls") {
    struct Demo: View {
        @State var mic = true
        @State var cam = false
        @State var speaker = true
        @State var share = false
        var body: some View {
            VStack(spacing: 24) {
                PPCallControls(micOn: $mic, cameraOn: $cam, speakerOn: $speaker, sharing: $share, onLeave: {})
                PPCallControls(micOn: $mic, cameraOn: $cam, speakerOn: $speaker, labeled: true, showsShare: false, onLeave: {})
                PPCallControls(micOn: $mic, cameraOn: $cam, onSurface: true, onLeave: {})
                    .colorScheme(.light)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Color(ppHex: 0x0B0E12))
        }
    }
    return Demo().pigeonpostTheme()
}
