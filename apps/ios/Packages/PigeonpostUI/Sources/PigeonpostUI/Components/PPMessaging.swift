//  Messaging: PPTicks, PPMessageBubble, PPTypingIndicator, PPComposer, PPThreadRow,
//  PPConversationHeader, PPAddressChip. Bubble radius 18 with a 6pt corner toward the sender.

import SwiftUI

// MARK: - Ticks

/// Delivery state of a message (`.pp-tick[data-state]`).
public enum PPTickState: String, CaseIterable, Sendable {
    case sending, sent, delivered, seen, working, failed

    var icon: PPIconName {
        switch self {
        case .sending: return .clock
        case .sent: return .check
        case .delivered, .seen: return .checkCheck
        case .working: return .settings
        case .failed: return .alertCircle
        }
    }

    var color: Color {
        switch self {
        case .seen: return PP.Colors.tickSeen
        case .working: return PP.Colors.tickWorking
        case .failed: return PP.Colors.danger
        default: return PP.Colors.tickSent
        }
    }

    /// What VoiceOver reads.
    public var label: String {
        switch self {
        case .sending: return "Sending"
        case .sent: return "Sent"
        case .delivered: return "Delivered"
        case .seen: return "Seen"
        case .working: return "Agent is working"
        case .failed: return "Not sent"
        }
    }

    var size: CGFloat {
        switch self {
        case .sending: return 13
        case .working: return 14
        case .failed: return 15
        default: return 16
        }
    }
}

/// The tick for a message. The working cog turns unless Reduce Motion is on.
public struct PPTicks: View {
    let state: PPTickState
    let tint: Color?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var turning = false

    public init(_ state: PPTickState, tint: Color? = nil) {
        self.state = state
        self.tint = tint
    }

    public var body: some View {
        PPIcon(state.icon, size: state.size, weight: state == .sending ? 2 : 2.2)
            .foregroundStyle(tint ?? state.color)
            .rotationEffect(.degrees(turning ? 360 : 0))
            .frame(width: 16, height: 16)
            .onAppear {
                guard state == .working, !reduceMotion else { return }
                withAnimation(.linear(duration: 2.4).repeatForever(autoreverses: false)) { turning = true }
            }
            .accessibilityElement()
            .accessibilityLabel(Text(state.label))
    }
}

// MARK: - Bubble

public enum PPBubbleRole: String, CaseIterable, Sendable { case mine, theirs, agent }

/// Where a message sits in a run from one sender; the corners toward the sender tighten to 6.
public enum PPBubblePosition: String, CaseIterable, Sendable { case single, first, middle, last }

/// A chat message bubble. Mine: tinted, tail bottom-right. Theirs: surface, tail bottom-left.
/// Agent: theirs plus a 3pt green rail and an "agent" badge with the agent's name.
public struct PPMessageBubble: View {
    let text: String
    let role: PPBubbleRole
    let position: PPBubblePosition
    let time: String
    let tick: PPTickState?
    let edited: Bool
    let agentName: String?
    let forwarded: Bool

    public init(_ text: String, role: PPBubbleRole, position: PPBubblePosition = .single, time: String,
                tick: PPTickState? = nil, edited: Bool = false, agentName: String? = nil, forwarded: Bool = false) {
        self.text = text
        self.role = role
        self.position = position
        self.time = time
        self.tick = tick
        self.edited = edited
        self.agentName = agentName
        self.forwarded = forwarded
    }

    /// The bubble's corner radii for a role and position (exposed for tests and custom shapes).
    public static func radii(role: PPBubbleRole, position: PPBubblePosition) -> RectangleCornerRadii {
        let r = PP.Radius.bubble, t = PP.Radius.bubbletail
        // Every bubble has its tail corner at the bottom on the sender's side; the middle and last
        // bubbles of a run also tighten the top corner on that side.
        let top = position == .middle || position == .last ? t : r
        if role == .mine {
            return RectangleCornerRadii(topLeading: r, bottomLeading: r, bottomTrailing: t, topTrailing: top)
        }
        return RectangleCornerRadii(topLeading: top, bottomLeading: t, bottomTrailing: r, topTrailing: r)
    }

    private var fill: Color { role == .mine ? PP.Colors.bubbleMine : PP.Colors.bubbleTheirs }
    private var border: Color { role == .mine ? PP.Colors.bubbleMineBorder : PP.Colors.bubbleTheirsBorder }
    private var ink: Color { role == .mine ? PP.Colors.bubbleMineText : PP.Colors.bubbleTheirsText }
    private var metaInk: Color { role == .mine ? PP.Colors.bubbleMineText.opacity(0.62) : PP.Colors.textFaint }

    public var body: some View {
        let shape = UnevenRoundedRectangle(cornerRadii: Self.radii(role: role, position: position), style: .continuous)
        VStack(alignment: .leading, spacing: 4) {
            if role == .agent {
                HStack(spacing: 6) {
                    PPIcon(.bot, size: 14).foregroundStyle(PP.Colors.success)
                    Text(agentName ?? "Agent").font(.geist(12, .semibold)).foregroundStyle(PP.Colors.text)
                    PPAgentBadge()
                }
            }
            if forwarded {
                HStack(spacing: 4) { PPIcon(.forward, size: 13); Text("Forwarded").italic() }
                    .font(.geist(12)).foregroundStyle(metaInk)
            }
            // Time and ticks sit on the last line, right-aligned, like the web bubble's floated meta.
            ZStack(alignment: .bottomTrailing) {
                // An invisible copy of the meta reserves room on the last line (spaces would be
                // trimmed at a line break; these glyphs are not).
                (Text(text) + Text(verbatim: metaPlaceholder).font(.geist(11.5)).foregroundColor(.clear))
                    .font(.geist(15))
                    .lineSpacing(PP.LineHeight.body - 15 * 1.2)
                    .foregroundStyle(ink)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                meta.offset(x: 4, y: 3)
            }
        }
        .padding(.vertical, 8)
        .padding(.leading, role == .agent ? 18 : 12)
        .padding(.trailing, 12)
        .background(shape.fill(fill))
        .overlay(shape.strokeBorder(border, lineWidth: 1))
        .overlay(alignment: .leading) {
            if role == .agent {
                Capsule().fill(PP.Colors.bubbleAgentRail).frame(width: 3).padding(.vertical, 9).padding(.leading, 7)
            }
        }
        .ppShadow(role == .mine ? .none : .one)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityText))
    }

    private var metaPlaceholder: String {
        "\u{00A0}\u{00A0}\u{00A0}" + (edited ? "edited\u{00A0}" : "") + time.replacingOccurrences(of: " ", with: "\u{00A0}") + (tick == nil ? "" : "\u{00A0}MM")
    }

    private var meta: some View {
        HStack(spacing: 4) {
            if edited { Text("edited").italic() }
            Text(time).monospacedDigit()
            if let tick { PPTicks(tick) }
        }
        .font(.geist(11.5))
        .foregroundStyle(metaInk)
        .frame(height: 16)
    }

    private var accessibilityText: String {
        var parts: [String] = []
        switch role {
        case .mine: parts.append("You")
        case .theirs: break
        case .agent: parts.append("\(agentName ?? "Agent"), agent")
        }
        if forwarded { parts.append("Forwarded") }
        parts.append(text)
        parts.append(time)
        if edited { parts.append("edited") }
        if let tick { parts.append(tick.label) }
        return parts.joined(separator: ", ")
    }
}

/// The lowercase "agent" pill (`.pp-agent-badge`).
public struct PPAgentBadge: View {
    public init() {}
    public var body: some View {
        HStack(spacing: 3) {
            PPIcon(.sparkles, size: 11)
            Text("agent")
        }
        .font(.geist(10.5, .semibold))
        .tracking(0.4)
        .foregroundStyle(PP.Colors.success)
        .padding(.horizontal, 6)
        .frame(height: 18)
        .background(Capsule().fill(PP.Colors.successFill))
        .overlay(Capsule().strokeBorder(PP.Colors.successBorder, lineWidth: 1))
    }
}

/// A message row: avatar slot for theirs/agent, the bubble aligned by role with at least 48pt
/// kept free on the far side (about 78% of a phone-width column).
public struct PPMessageRow: View {
    let bubble: PPMessageBubble
    let avatar: PPAvatar?

    public init(_ bubble: PPMessageBubble, avatar: PPAvatar? = nil) {
        self.bubble = bubble
        self.avatar = avatar
    }

    public var body: some View {
        HStack(alignment: .bottom, spacing: PP.Space.s2) {
            if bubble.role == .mine { Spacer(minLength: 48) }
            if bubble.role != .mine {
                Group {
                    if let avatar, bubble.position == .single || bubble.position == .last { avatar } else { Color.clear }
                }
                .frame(width: 28, height: 28)
            }
            bubble.fixedSize(horizontal: false, vertical: true)
                .layoutPriority(1)
            if bubble.role != .mine { Spacer(minLength: 48) }
        }
        .frame(maxWidth: .infinity, alignment: bubble.role == .mine ? .trailing : .leading)
    }
}

// MARK: - Typing

/// Three bouncing dots in a theirs-bubble (`.pp-typing`), or inline in a caption line.
public struct PPTypingIndicator: View {
    let inline: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(inline: Bool = false) { self.inline = inline }

    public var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 30, paused: reduceMotion)) { ctx in
            let t = ctx.date.timeIntervalSinceReferenceDate
            HStack(spacing: inline ? 3 : 4) {
                ForEach(0..<3, id: \.self) { i in
                    let p = reduceMotion ? 0.3 : ((t + Double(2 - i) * -0.15).truncatingRemainder(dividingBy: 1.2) + 1.2).truncatingRemainder(dividingBy: 1.2) / 1.2
                    let lift = p < 0.6 ? sin(p / 0.6 * .pi) : 0
                    Circle()
                        .fill(inline ? AnyShapeStyle(.foreground) : AnyShapeStyle(PP.Colors.textFaint))
                        .frame(width: inline ? 5 : 7, height: inline ? 5 : 7)
                        .opacity(0.45 + 0.55 * lift)
                        .offset(y: -4 * lift)
                }
            }
        }
        .padding(.horizontal, inline ? 0 : 14)
        .frame(height: inline ? 8 : 36)
        .background {
            if !inline {
                UnevenRoundedRectangle(cornerRadii: .init(topLeading: PP.Radius.bubble, bottomLeading: PP.Radius.bubbletail, bottomTrailing: PP.Radius.bubble, topTrailing: PP.Radius.bubble), style: .continuous)
                    .fill(PP.Colors.bubbleTheirs)
                    .overlay(UnevenRoundedRectangle(cornerRadii: .init(topLeading: PP.Radius.bubble, bottomLeading: PP.Radius.bubbletail, bottomTrailing: PP.Radius.bubble, topTrailing: PP.Radius.bubble), style: .continuous).strokeBorder(PP.Colors.bubbleTheirsBorder))
            }
        }
        .accessibilityElement()
        .accessibilityLabel(Text("Typing"))
    }
}

// MARK: - Composer

/// The request kinds an agent understands, shown above the composer field.
public enum PPComposerMode: String, CaseIterable, Identifiable, Sendable {
    case message, task, question
    public var id: String { rawValue }
    var title: String { self == .message ? "Message" : (self == .task ? "Do this work" : "Ask a question") }
    var icon: PPIconName { self == .message ? .messageSquare : (self == .task ? .zap : .helpCircle) }
}

/// The message composer (`.pp-composer`): radius 24 box, attach button, growing field, 40pt send.
/// Send is disabled while the text is blank; `isSending` spins the icon.
public struct PPComposer: View {
    @Binding var text: String
    let placeholder: String
    let isSending: Bool
    let modes: Binding<PPComposerMode>?
    let onAttach: (() -> Void)?
    let onSend: () -> Void
    @FocusState private var focused: Bool

    public init(text: Binding<String>, placeholder: String = "Message", isSending: Bool = false,
                mode: Binding<PPComposerMode>? = nil, onAttach: (() -> Void)? = nil, onSend: @escaping () -> Void) {
        self._text = text
        self.placeholder = placeholder
        self.isSending = isSending
        self.modes = mode
        self.onAttach = onAttach
        self.onSend = onSend
    }

    var canSend: Bool { !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !isSending }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: PP.Radius.x2xl, style: .continuous)
        VStack(alignment: .leading, spacing: 6) {
            if let modes {
                HStack(spacing: 2) {
                    ForEach(PPComposerMode.allCases) { m in
                        Button { modes.wrappedValue = m } label: {
                            HStack(spacing: 6) { PPIcon(m.icon, size: 15); Text(m.title) }
                                .font(.geist(13.5, .medium))
                                .foregroundStyle(modes.wrappedValue == m ? PP.Colors.text : PP.Colors.textMuted)
                                .padding(.horizontal, 10)
                                .frame(height: 30)
                                .background(RoundedRectangle(cornerRadius: PP.Radius.s).fill(modes.wrappedValue == m ? PP.Colors.surfaceSelected : .clear))
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(modes.wrappedValue == m ? .isSelected : [])
                    }
                }
            }
            HStack(alignment: .bottom, spacing: 2) {
                if let onAttach {
                    Button(action: onAttach) { PPIcon(.paperclip, size: 21) }
                        .buttonStyle(PPRoundToolStyle())
                        .accessibilityLabel(Text("Attach a file"))
                }
                TextField("", text: $text, prompt: Text(placeholder).foregroundStyle(PP.Colors.textFaint), axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(.geist(15))
                    .foregroundStyle(PP.Colors.text)
                    .lineLimit(1...7)
                    .padding(.vertical, 9)
                    .padding(.horizontal, 6)
                    .frame(minHeight: 40)
                    .focused($focused)
                    .accessibilityLabel(Text(placeholder))
                    .onSubmit { if canSend { onSend() } }
                Button(action: onSend) {
                    PPIcon(.send, size: 19, weight: 2.2)
                        .rotationEffect(.degrees(isSending ? 360 : 0))
                        .animation(isSending ? .linear(duration: 1).repeatForever(autoreverses: false) : .default, value: isSending)
                }
                .buttonStyle(PPSendButtonStyle(enabled: canSend))
                .disabled(!canSend)
                .accessibilityLabel(Text(isSending ? "Sending" : "Send"))
            }
        }
        .padding(6)
        .background(shape.fill(PP.Colors.surface))
        .overlay(shape.strokeBorder(focused ? PP.Colors.accent : PP.Colors.border, lineWidth: 1))
        .ppFocusRing(focused, in: shape)
        .ppShadow(.one)
        .animation(.easeOut(duration: PP.Duration.fast), value: focused)
    }
}

struct PPRoundToolStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(configuration.isPressed ? PP.Colors.textLink : PP.Colors.textMuted)
            .frame(width: 40, height: 40)
            .background(Circle().fill(configuration.isPressed ? PP.Colors.surfaceHover : .clear))
            .scaleEffect(configuration.isPressed ? 0.92 : 1)
            .contentShape(Circle())
    }
}

struct PPSendButtonStyle: ButtonStyle {
    let enabled: Bool
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(enabled ? PP.Colors.onAccent : PP.Colors.textFaint)
            .frame(width: 40, height: 40)
            .background(Circle().fill(enabled ? (configuration.isPressed ? PP.Colors.accentHover : PP.Colors.accent) : PP.Colors.surfaceSunken))
            .overlay(Circle().strokeBorder(enabled ? .clear : PP.Colors.border, lineWidth: 1))
            .shadow(color: enabled ? PP.Colors.accent.opacity(0.3) : .clear, radius: 6, y: 4)
            .scaleEffect(configuration.isPressed ? 0.94 : 1)
            .contentShape(Circle())
    }
}

// MARK: - Thread row

/// One conversation in the inbox list (`.pp-thread`): avatar, name + handle, time, preview with
/// tick, unread count, pinned/muted, agent and typing states. Radius 14 when selected.
public struct PPThreadRow: View {
    let peer: String
    let name: String?
    let preview: String
    let time: String
    let unread: Int
    let tick: PPTickState?
    let isAgent: Bool
    let isTyping: Bool
    let isPinned: Bool
    let isMuted: Bool
    let isSelected: Bool
    let presence: PPPresence?
    let held: Bool

    public init(peer: String, name: String? = nil, preview: String, time: String, unread: Int = 0,
                tick: PPTickState? = nil, isAgent: Bool = false, isTyping: Bool = false, isPinned: Bool = false,
                isMuted: Bool = false, isSelected: Bool = false, presence: PPPresence? = nil, held: Bool = false) {
        self.peer = peer
        self.name = name
        self.preview = preview
        self.time = time
        self.unread = unread
        self.tick = tick
        self.isAgent = isAgent
        self.isTyping = isTyping
        self.isPinned = isPinned
        self.isMuted = isMuted
        self.isSelected = isSelected
        self.presence = presence
        self.held = held
    }

    var shownName: String { name ?? PPPeer.displayName(peer) }

    public var body: some View {
        HStack(spacing: 12) {
            PPAvatar(peer: peer, name: name, size: 44, presence: presence)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(shownName).font(.geist(15, .semibold)).foregroundStyle(PP.Colors.text).lineLimit(1)
                    if isAgent { PPIcon(.bot, size: 14).foregroundStyle(PP.Colors.success) }
                    if name != nil {
                        Text(peer).font(.geistMono(12)).foregroundStyle(PP.Colors.textFaint).lineLimit(1).layoutPriority(-1)
                    }
                    if held { PPHeldBadge() }
                    Spacer(minLength: 4)
                    Text(time).font(.geist(12, unread > 0 ? .semibold : .regular)).monospacedDigit()
                        .foregroundStyle(unread > 0 ? PP.Colors.textLink : PP.Colors.textFaint)
                }
                HStack(spacing: 6) {
                    if isTyping {
                        HStack(spacing: 6) { Text("typing"); PPTypingIndicator(inline: true) }
                            .font(.geist(13.5, .medium)).foregroundStyle(PP.Colors.success)
                    } else {
                        HStack(spacing: 2) {
                            if let tick { PPTicks(tick) }
                            Text(preview).lineLimit(1)
                        }
                        .font(.geist(13.5, unread > 0 ? .medium : .regular))
                        .foregroundStyle(unread > 0 ? PP.Colors.textBody : PP.Colors.textMuted)
                    }
                    Spacer(minLength: 4)
                    if isMuted { PPIcon(.bellOff, size: 15).foregroundStyle(PP.Colors.textFaint) }
                    if isPinned { PPIcon(.pin, size: 15).foregroundStyle(PP.Colors.textFaint) }
                    if unread > 0 {
                        Text(unread > 99 ? "99+" : "\(unread)")
                            .font(.geist(11.5, .semibold)).monospacedDigit()
                            .foregroundStyle(PP.Colors.onAccent)
                            .padding(.horizontal, 6)
                            .frame(minWidth: 20, minHeight: 20)
                            .background(Capsule().fill(isMuted ? PP.Colors.textFaint : PP.Colors.accent))
                    }
                }
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: PP.Radius.l, style: .continuous).fill(isSelected ? PP.Colors.surfaceSelected : .clear))
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityText))
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
    }

    private var accessibilityText: String {
        var parts = [shownName]
        if isAgent { parts.append("agent") }
        if unread > 0 { parts.append("\(unread) unread") }
        parts.append(isTyping ? "typing" : preview)
        parts.append(time)
        if isPinned { parts.append("pinned") }
        if isMuted { parts.append("muted") }
        if held { parts.append("held for review") }
        return parts.joined(separator: ", ")
    }
}

/// The amber "held" chip for a request waiting for your review.
public struct PPHeldBadge: View {
    public init() {}
    public var body: some View {
        HStack(spacing: 4) {
            Circle().frame(width: 5, height: 5)
            Text("HELD")
        }
        .font(.geist(10.5, .bold))
        .tracking(0.5)
        .foregroundStyle(PP.Colors.warning)
        .padding(.horizontal, 6)
        .frame(height: 18)
        .background(RoundedRectangle(cornerRadius: PP.Radius.xs).fill(PP.Colors.warningFill))
    }
}

// MARK: - Conversation header

/// The bar above a conversation: back, avatar + name + status line, call / video / more.
public struct PPConversationHeader: View {
    let peer: String
    let name: String?
    let status: String
    let isOnline: Bool
    let isVerified: Bool
    let onBack: (() -> Void)?
    let onCall: (() -> Void)?
    let onVideo: (() -> Void)?
    let onMore: (() -> Void)?

    public init(peer: String, name: String? = nil, status: String, isOnline: Bool = false, isVerified: Bool = false,
                onBack: (() -> Void)? = nil, onCall: (() -> Void)? = nil, onVideo: (() -> Void)? = nil, onMore: (() -> Void)? = nil) {
        self.peer = peer
        self.name = name
        self.status = status
        self.isOnline = isOnline
        self.isVerified = isVerified
        self.onBack = onBack
        self.onCall = onCall
        self.onVideo = onVideo
        self.onMore = onMore
    }

    public var body: some View {
        HStack(spacing: 8) {
            if let onBack { headerButton(.chevronLeft, "Back", onBack) }
            HStack(spacing: 12) {
                PPAvatar(peer: peer, name: name, size: 38, presence: isOnline ? .online : nil)
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 6) {
                        Text(name ?? PPPeer.displayName(peer)).font(.geist(16, .semibold)).foregroundStyle(PP.Colors.text).lineLimit(1)
                            .layoutPriority(1)
                        if isVerified { PPIcon(.badgeCheck, size: 16).foregroundStyle(PP.Colors.accent).accessibilityLabel(Text("Verified")) }
                    }
                    HStack(spacing: 6) {
                        Text(peer).font(.geistMono(12)).foregroundStyle(PP.Colors.textMuted).lineLimit(1)
                        Text("·").foregroundStyle(PP.Colors.textFaint)
                        Text(status).foregroundStyle(isOnline ? PP.Colors.success : PP.Colors.textMuted).lineLimit(1)
                    }
                    .font(.geist(12))
                }
            }
            .layoutPriority(1)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 4)
            HStack(spacing: 0) {
                if let onCall { headerButton(.phone, "Voice call", onCall) }
                if let onVideo { headerButton(.video, "Video call", onVideo) }
                if let onMore { headerButton(.moreVertical, "More", onMore) }
            }
        }
        .padding(.vertical, 8)
        .padding(.leading, onBack == nil ? 16 : 6)
        .padding(.trailing, 8)
        .frame(minHeight: 56)
        .background(PP.Colors.surface)
        .overlay(alignment: .bottom) { Rectangle().fill(PP.Colors.border).frame(height: 1) }
    }

    private func headerButton(_ icon: PPIconName, _ label: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) { PPIcon(icon, size: 20) }
            .buttonStyle(PPHeaderButtonStyle())
            .accessibilityLabel(Text(label))
    }
}

struct PPHeaderButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(configuration.isPressed ? PP.Colors.text : PP.Colors.textMuted)
            .frame(width: 40, height: 44)
            .background(RoundedRectangle(cornerRadius: PP.Radius.m).fill(configuration.isPressed ? PP.Colors.surfaceHover : .clear).frame(height: 38))
            .contentShape(Rectangle())
    }
}

// MARK: - Address chip

public enum PPAddressKind: String, CaseIterable, Sendable { case person, key, org, agent, error }

/// A handle or key address in Geist Mono (`.pp-addr`), with verified mark and copy / remove.
public struct PPAddressChip: View {
    let address: String
    let kind: PPAddressKind
    let verified: Bool
    let size: PPControlSize
    let onCopy: (() -> Void)?
    let onRemove: (() -> Void)?

    public init(_ address: String, kind: PPAddressKind = .person, verified: Bool = false, size: PPControlSize = .medium,
                onCopy: (() -> Void)? = nil, onRemove: (() -> Void)? = nil) {
        self.address = address
        self.kind = kind
        self.verified = verified
        self.size = size
        self.onCopy = onCopy
        self.onRemove = onRemove
    }

    private var height: CGFloat { size == .small ? 22 : (size == .large ? 34 : 28) }
    private var fontSize: CGFloat { size == .small ? 12 : (size == .large ? 14 : 13) }

    private var colors: (Color, Color, Color) {
        switch kind {
        case .person: return (PP.Colors.surfaceSunken, PP.Colors.border, PP.Colors.text)
        case .key: return (PP.Colors.accentSubtle, PP.Colors.accentSubtleBorder, PP.Colors.textLink)
        case .org: return (PP.Colors.warningFill, PP.Colors.warningBorder, PP.Colors.text)
        case .agent: return (PP.Colors.successFill, PP.Colors.successBorder, PP.Colors.text)
        case .error: return (PP.Colors.dangerFill, PP.Colors.dangerBorder, PP.Colors.danger)
        }
    }

    public var body: some View {
        let (bg, bd, ink) = colors
        HStack(spacing: 4) {
            if kind == .key { PPIcon(.keyRound, size: 14) }
            if kind == .agent { PPIcon(.bot, size: 14).foregroundStyle(PP.Colors.success) }
            Text(address).lineLimit(1).truncationMode(.middle)
            if verified { PPIcon(.badgeCheck, size: 15).foregroundStyle(kind == .org ? PP.Colors.warningSolid : PP.Colors.accent).accessibilityLabel(Text("Verified")) }
            if let onCopy { chipButton(.copy, "Copy \(address)", onCopy) }
            if let onRemove { chipButton(.x, "Remove \(address)", onRemove) }
        }
        .font(.geistMono(fontSize, .medium))
        .foregroundStyle(ink)
        .padding(.leading, size == .small ? 8 : (size == .large ? 12 : 10))
        .padding(.trailing, onCopy == nil && onRemove == nil ? 10 : 4)
        .frame(height: height)
        .background(Capsule().fill(bg))
        .overlay(Capsule().strokeBorder(bd, lineWidth: 1))
        .accessibilityElement(children: .contain)
    }

    private func chipButton(_ icon: PPIconName, _ label: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) { PPIcon(icon, size: 13) }
            .buttonStyle(.plain)
            .foregroundStyle(PP.Colors.textFaint)
            .frame(width: 22, height: 22)
            .contentShape(Circle())
            .accessibilityLabel(Text(label))
    }
}

// MARK: - Previews

#Preview("Conversation") {
    struct Demo: View {
        @State var draft = ""
        @State var mode = PPComposerMode.message
        var body: some View {
            VStack(spacing: 0) {
                PPConversationHeader(peer: "/ada", name: "Ada Yılmaz", status: "online", isOnline: true, isVerified: true,
                                     onBack: {}, onCall: {}, onVideo: {}, onMore: {})
                ScrollView {
                    VStack(spacing: 2) {
                        PPMessageRow(PPMessageBubble("Can your agent pull last month's invoices from /wodo/finance?", role: .theirs, position: .first, time: "14:02"),
                                     avatar: PPAvatar(peer: "/ada", name: "Ada Yılmaz", size: 28))
                        PPMessageRow(PPMessageBubble("I need them before the 16:00 call.", role: .theirs, position: .last, time: "14:02"),
                                     avatar: PPAvatar(peer: "/ada", name: "Ada Yılmaz", size: 28))
                        PPMessageRow(PPMessageBubble("Sure, asking it now.", role: .mine, time: "14:03", tick: .seen)).padding(.top, 8)
                        PPMessageRow(PPMessageBubble("Found 14 invoices for October, $12,480.00 in total. I put them in a shared folder and flagged two that are overdue.",
                                                     role: .agent, time: "14:05", agentName: "/bekir/agent1"),
                                     avatar: PPAvatar(initials: "", tone: .green, size: 28, icon: .bot)).padding(.top, 8)
                        PPMessageRow(PPMessageBubble("Thanks, both look right.", role: .mine, position: .first, time: "14:06", tick: .delivered))
                            .padding(.top, 8)
                        PPMessageRow(PPMessageBubble("Sending the summary to Emre too.", role: .mine, position: .last, time: "14:06", tick: .sending, edited: true))
                        HStack { PPTypingIndicator(); Spacer() }.padding(.leading, 36).padding(.top, 8)
                    }
                    .padding(16)
                }
                .background(PP.Colors.bg)
                PPComposer(text: $draft, placeholder: "Message /ada", mode: $mode, onAttach: {}, onSend: { draft = "" })
                    .padding(10)
                    .background(PP.Colors.bg)
            }
        }
    }
    return Demo().pigeonpostTheme()
}

#Preview("Inbox rows") {
    VStack(spacing: 1) {
        PPThreadRow(peer: "/ada", name: "Ada Yılmaz", preview: "I need them before the 16:00 call.", time: "14:02", unread: 2, presence: .online)
        PPThreadRow(peer: "/bekir/agent1", preview: "Found 14 invoices for October", time: "14:05", isAgent: true, isTyping: true)
        PPThreadRow(peer: "/wodo/hr", preview: "Your leave request was approved.", time: "Mon", tick: .seen, isPinned: true, isSelected: true)
        PPThreadRow(peer: "/emre", name: "Emre Dağ", preview: "Voice note · 0:42", time: "Sun", unread: 5, isMuted: true)
        PPThreadRow(peer: "/k/9mjp4x2rkd8wq", preview: "Wants to send you 3 files", time: "Sat", held: true)
    }
    .padding(6)
    .background(PP.Colors.bgSubtle)
    .pigeonpostTheme()
}

#Preview("Addresses and ticks") {
    VStack(alignment: .leading, spacing: 12) {
        HStack { PPAddressChip("/ada", verified: true, onCopy: {}); PPAddressChip("/wodo/hr", kind: .org, verified: true) }
        HStack { PPAddressChip("/bekir/agent1", kind: .agent); PPAddressChip("/k/9mjp4x2rkd8wq3", kind: .key, size: .small) }
        PPAddressChip("/no-such-handle", kind: .error, onRemove: {})
        HStack(spacing: 14) { ForEach(PPTickState.allCases, id: \.self) { PPTicks($0) } }
    }
    .padding(20)
    .background(PP.Colors.surface)
    .pigeonpostTheme(PPTheme(colorScheme: .dark))
}
