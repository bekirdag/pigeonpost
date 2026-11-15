//  Feedback: PPBanner (`.pp-alert`) and PPToast (`.pp-toast`) with a `.ppToast(_:)` presenter.

import SwiftUI

public enum PPFeedbackTone: String, CaseIterable, Sendable {
    case neutral, info, success, warning, danger

    var icon: PPIconName {
        switch self {
        case .neutral, .info: return .info
        case .success: return .circleCheck
        case .warning: return .alertTriangle
        case .danger: return .alertCircle
        }
    }

    var ink: Color {
        switch self {
        case .neutral: return PP.Colors.textMuted
        case .info: return PP.Colors.info
        case .success: return PP.Colors.success
        case .warning: return PP.Colors.warning
        case .danger: return PP.Colors.danger
        }
    }

    var fill: Color {
        switch self {
        case .neutral: return PP.Colors.surfaceSunken
        case .info: return PP.Colors.infoFill
        case .success: return PP.Colors.successFill
        case .warning: return PP.Colors.warningFill
        case .danger: return PP.Colors.dangerFill
        }
    }

    var border: Color {
        switch self {
        case .neutral: return PP.Colors.border
        case .info: return PP.Colors.infoBorder
        case .success: return PP.Colors.successBorder
        case .warning: return PP.Colors.warningBorder
        case .danger: return PP.Colors.dangerBorder
        }
    }
}

/// An inline alert: icon, title, text, optional action and dismiss. Radius 14.
public struct PPBanner: View {
    let tone: PPFeedbackTone
    let title: String?
    let message: String
    let actionTitle: String?
    let action: (() -> Void)?
    let onDismiss: (() -> Void)?

    public init(_ tone: PPFeedbackTone = .info, title: String? = nil, message: String,
                actionTitle: String? = nil, action: (() -> Void)? = nil, onDismiss: (() -> Void)? = nil) {
        self.tone = tone
        self.title = title
        self.message = message
        self.actionTitle = actionTitle
        self.action = action
        self.onDismiss = onDismiss
    }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: PP.Radius.l, style: .continuous)
        HStack(alignment: .top, spacing: PP.Space.s3) {
            PPIcon(tone.icon, size: 20).foregroundStyle(tone.ink)
            VStack(alignment: .leading, spacing: 2) {
                if let title { Text(title).font(.geist(13.5, .semibold)).foregroundStyle(PP.Colors.text) }
                Text(message).ppText(.bodyS).foregroundStyle(PP.Colors.textBody)
                if let actionTitle, let action {
                    PPButton(actionTitle, variant: .outline, size: .small, action: action).padding(.top, PP.Space.s2)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if let onDismiss {
                Button(action: onDismiss) { PPIcon(.x, size: 16) }
                    .buttonStyle(.plain)
                    .foregroundStyle(PP.Colors.textMuted)
                    .frame(width: 28, height: 28)
                    .contentShape(Rectangle())
                    .padding(.top, -4)
                    .accessibilityLabel(Text("Dismiss"))
            }
        }
        .padding(.vertical, 14)
        .padding(.horizontal, PP.Space.s4)
        .background(shape.fill(tone.fill))
        .overlay(shape.strokeBorder(tone.border, lineWidth: 1))
        .accessibilityElement(children: .contain)
    }
}

/// What a toast says. Pass one to `.ppToast(_:)`.
public struct PPToastItem: Identifiable, Equatable {
    public let id = UUID()
    public var tone: PPFeedbackTone
    public var title: String
    public var message: String?
    public var actionTitle: String?
    public var duration: TimeInterval
    public var action: (() -> Void)?

    public init(_ title: String, message: String? = nil, tone: PPFeedbackTone = .neutral, actionTitle: String? = nil,
                duration: TimeInterval = 5, action: (() -> Void)? = nil) {
        self.title = title
        self.message = message
        self.tone = tone
        self.actionTitle = actionTitle
        self.duration = duration
        self.action = action
    }

    public static func == (a: PPToastItem, b: PPToastItem) -> Bool { a.id == b.id }
}

/// A raised notice: icon, title, text, an optional text action ("Undo") and close.
public struct PPToast: View {
    let item: PPToastItem
    let onClose: (() -> Void)?

    public init(_ item: PPToastItem, onClose: (() -> Void)? = nil) {
        self.item = item
        self.onClose = onClose
    }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: PP.Radius.l, style: .continuous)
        HStack(alignment: .top, spacing: PP.Space.s3) {
            PPIcon(item.tone.icon, size: 20).foregroundStyle(item.tone.ink)
            VStack(alignment: .leading, spacing: 1) {
                Text(item.title).font(.geist(13.5, item.message == nil ? .medium : .semibold)).foregroundStyle(PP.Colors.text)
                if let message = item.message { Text(message).font(.geist(13.5)).foregroundStyle(PP.Colors.textMuted) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if let actionTitle = item.actionTitle, let action = item.action {
                Button(actionTitle) { action(); onClose?() }
                    .buttonStyle(.plain)
                    .font(.geist(13, .semibold))
                    .foregroundStyle(PP.Colors.textLink)
                    .padding(.horizontal, 10)
                    .frame(minHeight: 30)
                    .frame(maxHeight: .infinity)
            }
            if let onClose {
                Button(action: onClose) { PPIcon(.x, size: 16) }
                    .buttonStyle(.plain)
                    .foregroundStyle(PP.Colors.textMuted)
                    .frame(width: 28, height: 28)
                    .contentShape(Rectangle())
                    .accessibilityLabel(Text("Close"))
            }
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 12))
        .background(shape.fill(PP.Colors.surfaceRaised))
        .overlay(shape.strokeBorder(PP.Colors.border, lineWidth: 1))
        .ppShadow(.three)
        .accessibilityElement(children: .contain)
        .accessibilityAddTraits(.isStaticText)
    }
}

public extension View {
    /// Shows `item` as a toast at the bottom, auto-dismissing after its duration (paused for
    /// VoiceOver users, who get an announcement instead).
    func ppToast(_ item: Binding<PPToastItem?>) -> some View {
        modifier(PPToastPresenter(item: item))
    }
}

struct PPToastPresenter: ViewModifier {
    @Binding var item: PPToastItem?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.accessibilityVoiceOverEnabled) private var voiceOver

    func body(content: Content) -> some View {
        content.overlay(alignment: .bottom) {
            if let current = item {
                PPToast(current) { item = nil }
                    .padding(.horizontal, PP.Space.s4)
                    .padding(.bottom, PP.Space.s4)
                    .frame(maxWidth: 520)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
                    .task(id: current.id) {
                        #if canImport(UIKit)
                        AccessibilityNotification.Announcement(current.title).post()
                        #endif
                        guard !voiceOver else { return }
                        try? await Task.sleep(nanoseconds: UInt64(current.duration * 1_000_000_000))
                        if item?.id == current.id { item = nil }
                    }
            }
        }
        .animation(.easeOut(duration: PP.Duration.base), value: item)
    }
}

#Preview("Feedback") {
    ScrollView {
        VStack(spacing: 14) {
            PPBanner(.info, title: "Agent waiting for approval", message: "/bekir/agent1 wants to send files to /wodo/hr. Review it before it goes out.", actionTitle: "Review request", action: {})
            PPBanner(.success, message: "Your Nest is running. It wakes when a message arrives.")
            PPBanner(.warning, title: "Credits running low", message: "$2.40 left. Agents stop replying at $0.", onDismiss: {})
            PPBanner(.danger, title: "Payment failed", message: "We could not charge the card ending 4242. Your Nest stops on 2 Dec.")
            PPToast(PPToastItem("Message archived", tone: .neutral, actionTitle: "Undo", action: {}), onClose: {})
            PPToast(PPToastItem("Copied /ada to the clipboard", tone: .success))
            PPToast(PPToastItem("Could not reach /k/9mjp…", message: "We will retry for 24 hours.", tone: .danger), onClose: {})
        }
        .padding(20)
    }
    .background(PP.Colors.bg)
    .pigeonpostTheme()
}
