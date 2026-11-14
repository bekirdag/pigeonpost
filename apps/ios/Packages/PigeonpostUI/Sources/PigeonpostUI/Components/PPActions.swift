//  Actions: PPButton and PPIconButton (`.pp-btn`, `.pp-icon-btn`).

import SwiftUI

public enum PPButtonVariant: String, CaseIterable, Sendable {
    case primary, secondary, outline, ghost, link, danger, dangerOutline, success
}

public enum PPControlSize: String, CaseIterable, Sendable {
    /// 30pt — dense toolbars.
    case small
    /// 38pt — the default.
    case medium
    /// 46pt — primary actions on mobile.
    case large
    /// 44pt — icon buttons in mobile bars.
    case touch

    var height: CGFloat {
        switch self {
        case .small: return PP.Control.s
        case .medium: return PP.Control.m
        case .large: return PP.Control.l
        case .touch: return PP.Control.touchMin
        }
    }

    var radius: CGFloat {
        switch self {
        case .small: return PP.Radius.s
        case .medium: return PP.Radius.m
        case .large, .touch: return PP.Radius.l
        }
    }

    var paddingX: CGFloat {
        switch self {
        case .small: return 10
        case .medium: return 14
        case .large, .touch: return 20
        }
    }

    var fontSize: CGFloat { self == .small ? 13 : (self == .medium ? 14 : 15) }
    var iconSize: CGFloat { self == .small ? 16 : (self == .medium ? 16 : 18) }
}

struct PPButtonColors {
    let fill: Color
    let border: Color
    let ink: Color
    let shadow: Bool

    static func of(_ variant: PPButtonVariant, pressed: Bool) -> PPButtonColors {
        switch variant {
        case .primary:
            let c = pressed ? PP.Colors.accentPressed : PP.Colors.accent
            return .init(fill: c, border: c, ink: PP.Colors.onAccent, shadow: true)
        case .secondary:
            return .init(fill: PP.Colors.accentSubtle, border: PP.Colors.accentSubtleBorder, ink: PP.Colors.textLink, shadow: false)
        case .outline:
            return .init(fill: pressed ? PP.Colors.surfaceHover : .clear, border: PP.Colors.borderStrong, ink: PP.Colors.text, shadow: false)
        case .ghost:
            return .init(fill: pressed ? PP.Colors.surfaceHover : .clear, border: .clear, ink: PP.Colors.textBody, shadow: false)
        case .link:
            return .init(fill: .clear, border: .clear, ink: PP.Colors.textLink, shadow: false)
        case .danger:
            return .init(fill: PP.Colors.dangerSolid, border: PP.Colors.dangerSolid, ink: PP.Colors.onSolid, shadow: true)
        case .dangerOutline:
            return .init(fill: pressed ? PP.Colors.dangerFill : .clear, border: PP.Colors.dangerBorder, ink: PP.Colors.danger, shadow: false)
        case .success:
            return .init(fill: PP.Colors.successSolid, border: PP.Colors.successSolid, ink: PP.Colors.onSolid, shadow: true)
        }
    }
}

/// A labelled button. `isLoading` hides the label behind a spinner and sets the accessibility
/// value to "Loading"; the button keeps its width.
public struct PPButton: View {
    let title: String
    let icon: PPIconName?
    let trailingIcon: PPIconName?
    let variant: PPButtonVariant
    let size: PPControlSize
    let fullWidth: Bool
    let isLoading: Bool
    let action: () -> Void

    @Environment(\.isEnabled) private var isEnabled

    public init(_ title: String, icon: PPIconName? = nil, trailingIcon: PPIconName? = nil,
                variant: PPButtonVariant = .outline, size: PPControlSize = .medium,
                fullWidth: Bool = false, isLoading: Bool = false, action: @escaping () -> Void) {
        self.title = title
        self.icon = icon
        self.trailingIcon = trailingIcon
        self.variant = variant
        self.size = size
        self.fullWidth = fullWidth
        self.isLoading = isLoading
        self.action = action
    }

    public var body: some View {
        Button(action: { if !isLoading { action() } }) {
            HStack(spacing: 8) {
                if let icon { PPIcon(icon, size: size.iconSize) }
                Text(title).lineLimit(1)
                if let trailingIcon { PPIcon(trailingIcon, size: size.iconSize) }
            }
            .opacity(isLoading ? 0 : 1)
            .overlay { if isLoading { ProgressView().controlSize(.small).tint(PPButtonColors.of(variant, pressed: false).ink) } }
        }
        .buttonStyle(PPButtonStyle(variant: variant, size: size, fullWidth: fullWidth, enabled: isEnabled))
        .accessibilityValue(isLoading ? Text("Loading") : Text(""))
        .accessibilityAddTraits(isLoading ? .updatesFrequently : [])
    }
}

/// The `.pp-btn` look as a ButtonStyle, for buttons with custom labels.
public struct PPButtonStyle: ButtonStyle {
    let variant: PPButtonVariant
    let size: PPControlSize
    let fullWidth: Bool
    let enabled: Bool

    public init(variant: PPButtonVariant = .outline, size: PPControlSize = .medium, fullWidth: Bool = false, enabled: Bool = true) {
        self.variant = variant
        self.size = size
        self.fullWidth = fullWidth
        self.enabled = enabled
    }

    public func makeBody(configuration: Configuration) -> some View {
        let c = PPButtonColors.of(variant, pressed: configuration.isPressed)
        let shape = RoundedRectangle(cornerRadius: size.radius, style: .continuous)
        let isLink = variant == .link
        return configuration.label
            .font(.geist(size.fontSize, .medium))
            .foregroundStyle(c.ink)
            .padding(.horizontal, isLink ? 2 : size.paddingX)
            .frame(minHeight: isLink ? nil : size.height)
            .frame(maxWidth: fullWidth ? .infinity : nil)
            .background(shape.fill(c.fill))
            .overlay(shape.strokeBorder(c.border, lineWidth: 1))
            .contentShape(shape)
            .ppShadow(c.shadow && enabled ? .one : .none)
            .underline(isLink && configuration.isPressed)
            .opacity(enabled ? 1 : 0.5)
            .offset(y: configuration.isPressed && !isLink ? 1 : 0)
            .animation(.easeOut(duration: PP.Duration.fast), value: configuration.isPressed)
    }
}

public enum PPIconButtonVariant: String, CaseIterable, Sendable { case standard, ghost, primary, secondary, danger }

/// A square (or round) icon-only button. `label` is required: it is the accessibility label.
public struct PPIconButton: View {
    let icon: PPIconName
    let label: String
    let variant: PPIconButtonVariant
    let size: PPControlSize
    let round: Bool
    let isOn: Bool?
    let badge: Int?
    let action: () -> Void

    @Environment(\.isEnabled) private var isEnabled

    public init(_ icon: PPIconName, label: String, variant: PPIconButtonVariant = .standard,
                size: PPControlSize = .medium, round: Bool = false, isOn: Bool? = nil, badge: Int? = nil,
                action: @escaping () -> Void) {
        self.icon = icon
        self.label = label
        self.variant = variant
        self.size = size
        self.round = round
        self.isOn = isOn
        self.badge = badge
        self.action = action
    }

    private var iconSize: CGFloat {
        switch size {
        case .small: return 16
        case .medium: return 18
        case .large: return 20
        case .touch: return 22
        }
    }

    public var body: some View {
        Button(action: action) {
            PPIcon(icon, size: iconSize)
        }
        .buttonStyle(PPIconButtonStyle(variant: variant, size: size, round: round, isOn: isOn == true, enabled: isEnabled))
        .overlay(alignment: .topTrailing) {
            if let badge {
                Text(badge > 99 ? "99+" : "\(badge)")
                    .font(.geist(10.5, .semibold))
                    .monospacedDigit()
                    .foregroundStyle(PP.Colors.onSolid)
                    .padding(.horizontal, 5)
                    .frame(minWidth: 18, minHeight: 18)
                    .background(Capsule().fill(PP.Colors.dangerSolid))
                    .overlay(Capsule().stroke(PP.Colors.surface, lineWidth: 2))
                    .offset(x: 5, y: -5)
                    .accessibilityHidden(true)
            }
        }
        .accessibilityLabel(Text(label))
        .accessibilityValue(badge.map { Text("\($0) new") } ?? Text(""))
        .accessibilityAddTraits(isOn == true ? .isSelected : [])
    }
}

struct PPIconButtonStyle: ButtonStyle {
    let variant: PPIconButtonVariant
    let size: PPControlSize
    let round: Bool
    let isOn: Bool
    let enabled: Bool

    func makeBody(configuration: Configuration) -> some View {
        let pressed = configuration.isPressed
        var fill: Color, border: Color, ink: Color, shadow = false
        switch variant {
        case .standard: fill = pressed ? PP.Colors.surfaceHover : PP.Colors.surface; border = PP.Colors.borderStrong; ink = PP.Colors.textBody; shadow = true
        case .ghost: fill = pressed ? PP.Colors.surfaceHover : .clear; border = .clear; ink = PP.Colors.textMuted
        case .primary: fill = pressed ? PP.Colors.accentPressed : PP.Colors.accent; border = fill; ink = PP.Colors.onAccent; shadow = true
        case .secondary: fill = PP.Colors.accentSubtle; border = PP.Colors.accentSubtleBorder; ink = PP.Colors.textLink
        case .danger: fill = PP.Colors.dangerSolid; border = fill; ink = PP.Colors.onSolid
        }
        if isOn { fill = PP.Colors.surfaceSelected; border = PP.Colors.accentSubtleBorder; ink = PP.Colors.textLink }
        let radius = round ? size.height / 2 : size.radius
        let shape = RoundedRectangle(cornerRadius: radius, style: .continuous)
        return configuration.label
            .foregroundStyle(ink)
            .frame(width: size.height, height: size.height)
            .background(shape.fill(fill))
            .overlay(shape.strokeBorder(border, lineWidth: 1))
            .contentShape(shape)
            .ppShadow(shadow && enabled ? .one : .none)
            .opacity(enabled ? 1 : 0.5)
            .offset(y: pressed ? 1 : 0)
            .frame(minWidth: 44, minHeight: 44)
    }
}

#Preview("Buttons") {
    ScrollView {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 8) {
                PPButton("Send reply", icon: .send, variant: .primary) {}
                PPButton("Add contact", icon: .userPlus, variant: .secondary) {}
            }
            HStack(spacing: 8) {
                PPButton("Archive", variant: .outline) {}
                PPButton("Skip", variant: .ghost) {}
                PPButton("View plans", variant: .link) {}
            }
            HStack(spacing: 8) {
                PPButton("Block /spam/bot", icon: .shield, variant: .danger) {}
                PPButton("Remove", variant: .dangerOutline) {}
            }
            HStack(spacing: 8) {
                PPButton("Small", size: .small) {}
                PPButton("Medium", size: .medium) {}
                PPButton("Large", variant: .primary, size: .large) {}
            }
            PPButton("Top up $20.00", icon: .wallet, variant: .primary, size: .large, fullWidth: true, isLoading: true) {}
            PPButton("Answer call", icon: .phone, variant: .success, size: .large, fullWidth: true) {}
            PPButton("Disabled", variant: .primary) {}.disabled(true)
            HStack(spacing: 8) {
                PPIconButton(.search, label: "Search") {}
                PPIconButton(.bell, label: "Notifications", variant: .ghost, badge: 3) {}
                PPIconButton(.plus, label: "New message", variant: .primary, round: true) {}
                PPIconButton(.pin, label: "Pin conversation", isOn: true) {}
                PPIconButton(.moreHorizontal, label: "More", variant: .ghost, size: .touch) {}
            }
        }
        .padding(20)
    }
    .background(PP.Colors.bg)
    .pigeonpostTheme()
}
