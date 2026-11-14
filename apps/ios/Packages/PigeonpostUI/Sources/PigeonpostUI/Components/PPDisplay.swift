//  Display: PPAvatar, PPPresenceDot, PPBadge, PPTag, PPCard, PPListRow, PPSectionHeader,
//  PPEmptyState, PPProgress, PPMeter, PPSkeleton.

import SwiftUI

// MARK: - Avatar

public enum PPPresence: String, CaseIterable, Sendable { case online, away, busy, offline }

/// `.pp-presence`: a 10pt dot ringed in the surface colour.
public struct PPPresenceDot: View {
    let presence: PPPresence
    let size: CGFloat
    let ring: Color

    public init(_ presence: PPPresence, size: CGFloat = 10, ring: Color = PP.Colors.surface) {
        self.presence = presence
        self.size = size
        self.ring = ring
    }

    public var body: some View {
        Group {
            switch presence {
            case .online: Circle().fill(PP.Colors.successSolid)
            case .away: Circle().fill(PP.Colors.warningSolid)
            case .busy: Circle().fill(PP.Colors.dangerSolid)
            case .offline: Circle().fill(PP.Colors.surface).overlay(Circle().strokeBorder(PP.Colors.textFaint, lineWidth: 2))
            }
        }
        .frame(width: size, height: size)
        .padding(2)
        .background(Circle().fill(ring))
        .accessibilityLabel(Text(presence.rawValue.capitalized))
    }
}

/// An initials avatar (`.pp-initials`). Give it a peer address and it derives the initials and the
/// tone the web and native apps use; or pass `initials` / `tone` / `icon` / `image` yourself.
public struct PPAvatar: View {
    let initials: String
    let tone: PPTone
    let size: CGFloat
    let icon: PPIconName?
    let image: Image?
    let presence: PPPresence?
    let square: Bool

    /// 20, 28, 36 (default), 44, 64, 96 are the kit sizes.
    public init(peer: String?, name: String? = nil, size: CGFloat = 36, presence: PPPresence? = nil, image: Image? = nil) {
        self.initials = PPPeer.initials(peer, name: name)
        self.tone = PPTone.forPeer(peer)
        self.size = size
        self.icon = nil
        self.image = image
        self.presence = presence
        self.square = false
    }

    public init(initials: String, tone: PPTone = .blue, size: CGFloat = 36, icon: PPIconName? = nil,
                presence: PPPresence? = nil, square: Bool = false, image: Image? = nil) {
        self.initials = initials
        self.tone = tone
        self.size = size
        self.icon = icon
        self.image = image
        self.presence = presence
        self.square = square
    }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: square ? size * 0.28 : size / 2, style: .continuous)
        ZStack {
            shape.fill(tone.fill)
            if tone == .gray { shape.strokeBorder(PP.Colors.border, lineWidth: 1) }
            if let image {
                image.resizable().scaledToFill().clipShape(shape)
            } else if let icon {
                PPIcon(icon, size: size * 0.52).foregroundStyle(tone.ink)
            } else {
                Text(initials)
                    .font(.geist(size * 0.36, .semibold))
                    .tracking(size * 0.36 * 0.02)
                    .foregroundStyle(tone.ink)
                    .minimumScaleFactor(0.6)
            }
        }
        .frame(width: size, height: size)
        .overlay(alignment: .bottomTrailing) {
            if let presence {
                PPPresenceDot(presence, size: size >= 44 ? 12 : 10)
                    .offset(x: size >= 44 ? -1 : 1, y: size >= 44 ? -1 : 1)
            }
        }
        .accessibilityHidden(presence == nil)
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Badge and tag

public enum PPStatusTone: String, CaseIterable, Sendable {
    case neutral, info, accent, success, warning, danger, violet

    var fg: Color {
        switch self {
        case .neutral: return PP.Colors.textMuted
        case .info: return PP.Colors.info
        case .accent: return PP.Colors.textLink
        case .success: return PP.Colors.success
        case .warning: return PP.Colors.warning
        case .danger: return PP.Colors.danger
        case .violet: return .ppDynamic(light: 0x3D2978, dark: 0x927ECB)
        }
    }

    var bg: Color {
        switch self {
        case .neutral: return PP.Colors.surfaceSunken
        case .info: return PP.Colors.infoFill
        case .accent: return PP.Colors.accentSubtle
        case .success: return PP.Colors.successFill
        case .warning: return PP.Colors.warningFill
        case .danger: return PP.Colors.dangerFill
        case .violet: return .ppDynamic(light: 0xF1EEFD, dark: 0x27273D)
        }
    }

    var border: Color {
        switch self {
        case .neutral: return PP.Colors.border
        case .info: return PP.Colors.infoBorder
        case .accent: return PP.Colors.accentSubtleBorder
        case .success: return PP.Colors.successBorder
        case .warning: return PP.Colors.warningBorder
        case .danger: return PP.Colors.dangerBorder
        case .violet: return .ppDynamic(light: 0xDDD6FB, dark: 0x423D67)
        }
    }

    var dot: Color {
        switch self {
        case .neutral: return PP.Colors.textFaint
        case .info: return PP.Colors.infoSolid
        case .accent: return PP.Colors.accent
        case .success: return PP.Colors.successSolid
        case .warning: return PP.Colors.warningSolid
        case .danger: return PP.Colors.dangerSolid
        case .violet: return PP.Palette.violet400
        }
    }

    var solid: Color {
        switch self {
        case .neutral: return PP.Palette.gray600
        case .info, .accent: return PP.Colors.accent
        case .success: return PP.Palette.green700
        case .warning: return PP.Palette.amber600
        case .danger: return PP.Palette.red600
        case .violet: return PP.Palette.violet600
        }
    }
}

/// A status pill (`.pp-badge`), 22pt high (18 when `small`).
public struct PPBadge: View {
    let text: String
    let tone: PPStatusTone
    let icon: PPIconName?
    let dot: Bool
    let solid: Bool
    let small: Bool

    public init(_ text: String, tone: PPStatusTone = .neutral, icon: PPIconName? = nil, dot: Bool = false, solid: Bool = false, small: Bool = false) {
        self.text = text
        self.tone = tone
        self.icon = icon
        self.dot = dot
        self.solid = solid
        self.small = small
    }

    public var body: some View {
        HStack(spacing: small ? 4 : 6) {
            if dot { Circle().fill(solid ? PP.Colors.onSolid : tone.dot).frame(width: 6, height: 6) }
            if let icon { PPIcon(icon, size: 13) }
            Text(text).lineLimit(1)
        }
        .font(.geist(small ? 11 : 12, .medium))
        .foregroundStyle(solid ? PP.Colors.onSolid : tone.fg)
        .padding(.horizontal, small ? 6 : 8)
        .frame(height: small ? 18 : 22)
        .background(Capsule().fill(solid ? tone.solid : tone.bg))
        .overlay(Capsule().strokeBorder(solid ? .clear : tone.border, lineWidth: 1))
        .accessibilityElement(children: .combine)
    }
}

/// A label chip (`.pp-tag`), 28pt high, optionally removable.
public struct PPTag: View {
    let text: String
    let icon: PPIconName?
    let selected: Bool
    let onRemove: (() -> Void)?

    public init(_ text: String, icon: PPIconName? = nil, selected: Bool = false, onRemove: (() -> Void)? = nil) {
        self.text = text
        self.icon = icon
        self.selected = selected
        self.onRemove = onRemove
    }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: PP.Radius.s, style: .continuous)
        HStack(spacing: 6) {
            if let icon { PPIcon(icon, size: 14).foregroundStyle(selected ? PP.Colors.textLink : PP.Colors.textFaint) }
            Text(text).lineLimit(1)
            if let onRemove {
                Button(action: onRemove) { PPIcon(.x, size: 13) }
                    .buttonStyle(.plain)
                    .foregroundStyle(PP.Colors.textFaint)
                    .frame(width: 22, height: 22)
                    .contentShape(Rectangle())
                    .accessibilityLabel(Text("Remove \(text)"))
            }
        }
        .font(.geist(13, .medium))
        .foregroundStyle(selected ? PP.Colors.textLink : PP.Colors.textBody)
        .padding(.leading, 10)
        .padding(.trailing, onRemove == nil ? 10 : 4)
        .frame(height: 28)
        .background(shape.fill(selected ? PP.Colors.accentSubtle : PP.Colors.surface))
        .overlay(shape.strokeBorder(selected ? PP.Colors.accentSubtleBorder : PP.Colors.borderStrong, lineWidth: 1))
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

// MARK: - Card and list

/// A surface card (`.pp-card`): radius 14, 1pt border, padding 20.
public struct PPCard<Content: View>: View {
    let padding: CGFloat
    let elevation: PPElevation
    let content: Content

    public init(padding: CGFloat = PP.Space.s5, elevation: PPElevation = .none, @ViewBuilder content: () -> Content) {
        self.padding = padding
        self.elevation = elevation
        self.content = content()
    }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: PP.Radius.l, style: .continuous)
        VStack(alignment: .leading, spacing: PP.Space.s3) { content }
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(shape.fill(PP.Colors.surface))
            .overlay(shape.strokeBorder(PP.Colors.border, lineWidth: 1))
            .ppShadow(elevation)
    }
}

/// A list row (`.pp-list-item`, mobile list cell): leading icon or avatar, title, subtitle,
/// trailing value and chevron. Min height 52.
public struct PPListRow<Leading: View, Trailing: View>: View {
    let title: String
    let subtitle: String?
    let value: String?
    let showsChevron: Bool
    let destructive: Bool
    let leading: Leading
    let trailing: Trailing

    public init(_ title: String, subtitle: String? = nil, value: String? = nil, showsChevron: Bool = false,
                destructive: Bool = false, @ViewBuilder leading: () -> Leading, @ViewBuilder trailing: () -> Trailing) {
        self.title = title
        self.subtitle = subtitle
        self.value = value
        self.showsChevron = showsChevron
        self.destructive = destructive
        self.leading = leading()
        self.trailing = trailing()
    }

    public var body: some View {
        HStack(spacing: PP.Space.s3) {
            leading
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.geist(15, .medium)).foregroundStyle(destructive ? PP.Colors.danger : PP.Colors.text).lineLimit(1)
                if let subtitle { Text(subtitle).font(.geist(13.5)).foregroundStyle(PP.Colors.textMuted).lineLimit(2) }
            }
            Spacer(minLength: 8)
            if let value { Text(value).font(.geist(14)).foregroundStyle(PP.Colors.textMuted).lineLimit(1) }
            trailing
            if showsChevron { PPIcon(.chevronRight, size: 16).foregroundStyle(PP.Colors.textFaint) }
        }
        .padding(.vertical, PP.Space.s3)
        .padding(.horizontal, PP.Space.s4)
        .frame(minHeight: 52)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

public extension PPListRow where Leading == EmptyView, Trailing == EmptyView {
    init(_ title: String, subtitle: String? = nil, value: String? = nil, showsChevron: Bool = false, destructive: Bool = false) {
        self.init(title, subtitle: subtitle, value: value, showsChevron: showsChevron, destructive: destructive, leading: { EmptyView() }, trailing: { EmptyView() })
    }
}

public extension PPListRow where Trailing == EmptyView {
    init(_ title: String, subtitle: String? = nil, value: String? = nil, showsChevron: Bool = false, destructive: Bool = false, @ViewBuilder leading: () -> Leading) {
        self.init(title, subtitle: subtitle, value: value, showsChevron: showsChevron, destructive: destructive, leading: leading, trailing: { EmptyView() })
    }
}

/// The square icon tile used at the start of a list row (`.pp-list-item__icon`).
public struct PPRowIcon: View {
    let icon: PPIconName
    let tone: PPStatusTone

    public init(_ icon: PPIconName, tone: PPStatusTone = .neutral) {
        self.icon = icon
        self.tone = tone
    }

    public var body: some View {
        PPIcon(icon, size: 18)
            .foregroundStyle(tone == .neutral ? PP.Colors.textMuted : tone.fg)
            .frame(width: 36, height: 36)
            .background(RoundedRectangle(cornerRadius: PP.Radius.m, style: .continuous).fill(tone.bg))
    }
}

/// An uppercase section label above a group (`.pp-overline` in lists and settings).
public struct PPSectionHeader: View {
    let title: String
    let actionTitle: String?
    let action: (() -> Void)?

    public init(_ title: String, actionTitle: String? = nil, action: (() -> Void)? = nil) {
        self.title = title
        self.actionTitle = actionTitle
        self.action = action
    }

    public var body: some View {
        HStack {
            Text(title).ppText(.overline).foregroundStyle(PP.Colors.textFaint)
                .accessibilityAddTraits(.isHeader)
            Spacer()
            if let actionTitle, let action {
                Button(actionTitle, action: action).buttonStyle(.plain)
                    .font(.geist(13, .semibold)).foregroundStyle(PP.Colors.textLink)
            }
        }
        .padding(.horizontal, PP.Space.s4)
        .padding(.top, PP.Space.s4)
        .padding(.bottom, 6)
    }
}

// MARK: - Empty state

/// `.pp-empty`: icon tile, title, text and up to two actions, centred.
public struct PPEmptyState: View {
    let icon: PPIconName
    let title: String
    let message: String
    let primary: (String, () -> Void)?
    let secondary: (String, () -> Void)?
    let compact: Bool

    public init(icon: PPIconName, title: String, message: String, primary: (String, () -> Void)? = nil,
                secondary: (String, () -> Void)? = nil, compact: Bool = false) {
        self.icon = icon
        self.title = title
        self.message = message
        self.primary = primary
        self.secondary = secondary
        self.compact = compact
    }

    public var body: some View {
        VStack(spacing: compact ? PP.Space.s2 : PP.Space.s3) {
            PPIcon(icon, size: 22)
                .foregroundStyle(PP.Colors.textMuted)
                .frame(width: 48, height: 48)
                .background(RoundedRectangle(cornerRadius: PP.Radius.l).fill(PP.Colors.surfaceSunken))
                .overlay(RoundedRectangle(cornerRadius: PP.Radius.l).strokeBorder(PP.Colors.border))
            Text(title).ppText(.h3).foregroundStyle(PP.Colors.text).multilineTextAlignment(.center)
                .accessibilityAddTraits(.isHeader)
            Text(message).ppText(.body).foregroundStyle(PP.Colors.textMuted).multilineTextAlignment(.center)
                .frame(maxWidth: 420)
            if primary != nil || secondary != nil {
                HStack(spacing: PP.Space.s2) {
                    if let secondary { PPButton(secondary.0, variant: .outline, action: secondary.1) }
                    if let primary { PPButton(primary.0, variant: .primary, action: primary.1) }
                }
                .padding(.top, PP.Space.s2)
            }
        }
        .padding(.vertical, compact ? PP.Space.s8 : PP.Space.s12)
        .padding(.horizontal, compact ? PP.Space.s4 : PP.Space.s6)
        .frame(maxWidth: .infinity)
    }
}

// MARK: - Progress, meter, skeleton

/// `.pp-progress`: a 6pt track. `value` nil shows an indeterminate sweep.
public struct PPProgress: View {
    let value: Double?
    let tone: PPStatusTone
    let label: String
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase = false

    public init(value: Double?, tone: PPStatusTone = .accent, label: String = "Progress") {
        self.value = value
        self.tone = tone
        self.label = label
    }

    public var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(PP.Colors.surfaceSunken)
                Capsule().strokeBorder(PP.Colors.borderSubtle, lineWidth: 1)
                if let value {
                    Capsule().fill(tone == .accent ? PP.Colors.accent : tone.solid)
                        .frame(width: geo.size.width * min(max(value, 0), 1))
                        .animation(.easeOut(duration: PP.Duration.slow), value: value)
                } else {
                    Capsule().fill(PP.Colors.accent)
                        .frame(width: geo.size.width * 0.35)
                        .offset(x: phase ? geo.size.width * 0.65 : 0)
                        .onAppear {
                            guard !reduceMotion else { return }
                            withAnimation(.easeInOut(duration: 1.1).repeatForever(autoreverses: true)) { phase = true }
                        }
                }
            }
        }
        .frame(height: 6)
        .clipShape(Capsule())
        .accessibilityElement()
        .accessibilityLabel(Text(label))
        .accessibilityValue(value.map { Text("\(Int(($0 * 100).rounded())) percent") } ?? Text("In progress"))
    }
}

/// `.pp-meter`: a labelled usage bar (Nest hours, tokens, storage).
public struct PPMeter: View {
    let label: String
    let valueText: String
    let fraction: Double

    public init(_ label: String, value valueText: String, fraction: Double) {
        self.label = label
        self.valueText = valueText
        self.fraction = fraction
    }

    var tone: PPStatusTone { fraction >= 1 ? .danger : (fraction >= 0.8 ? .warning : .accent) }

    public var body: some View {
        VStack(spacing: 6) {
            HStack {
                Text(label).foregroundStyle(PP.Colors.textMuted)
                Spacer(minLength: 8)
                Text(valueText).fontWeight(.medium).monospacedDigit().foregroundStyle(PP.Colors.text)
            }
            .font(.geist(12))
            PPProgress(value: fraction, tone: tone, label: label)
        }
        .accessibilityElement(children: .combine)
    }
}

public enum PPSkeletonShape: Sendable {
    case text(width: CGFloat? = nil)
    case title(width: CGFloat? = nil)
    case avatar(size: CGFloat = 40)
    case rect(height: CGFloat = 120)
    case button(width: CGFloat = 96)
    case pill(width: CGFloat = 64)
}

/// `.pp-skeleton`: a shimmering placeholder. Hidden from VoiceOver; mark the container busy.
public struct PPSkeleton: View {
    let shape: PPSkeletonShape
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var shimmer = false

    public init(_ shape: PPSkeletonShape = .text()) { self.shape = shape }

    public var body: some View {
        let (w, h, r): (CGFloat?, CGFloat, CGFloat) = {
            switch shape {
            case .text(let w): return (w, 12, 6)
            case .title(let w): return (w, 20, 6)
            case .avatar(let s): return (s, s, s / 2)
            case .rect(let h): return (nil, h, PP.Radius.m)
            case .button(let w): return (w, PP.Control.m, PP.Radius.m)
            case .pill(let w): return (w, 22, 11)
            }
        }()
        RoundedRectangle(cornerRadius: r, style: .continuous)
            .fill(LinearGradient(colors: [PP.Colors.skeletonBase, PP.Colors.skeletonHighlight, PP.Colors.skeletonBase],
                                 startPoint: shimmer ? .leading : UnitPoint(x: -1, y: 0.5),
                                 endPoint: shimmer ? UnitPoint(x: 2, y: 0.5) : .trailing))
            .frame(width: w, height: h)
            .frame(maxWidth: w == nil ? .infinity : nil, alignment: .leading)
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(.easeInOut(duration: 1.4).repeatForever(autoreverses: false)) { shimmer = true }
            }
            .accessibilityHidden(true)
    }
}

#Preview("Display") {
    ScrollView {
        VStack(alignment: .leading, spacing: 20) {
            HStack(spacing: 10) {
                PPAvatar(peer: "/ada", name: "Ada Yılmaz", size: 44, presence: .online)
                PPAvatar(peer: "/wodo/hr", size: 44)
                PPAvatar(peer: "/emre", name: "Emre Dağ", size: 36, presence: .away)
                PPAvatar(peer: "/marina", name: "Marina Petrova", size: 28)
                PPAvatar(initials: "", tone: .green, size: 36, icon: .bot)
                PPAvatar(initials: "ÖA", tone: .gray, size: 36, presence: .offline)
            }
            HStack(spacing: 6) {
                PPBadge("Agent", tone: .success, icon: .bot)
                PPBadge("Live", tone: .danger, dot: true)
                PPBadge("Held", tone: .warning)
                PPBadge("Beta", tone: .violet)
                PPBadge("3 new", tone: .accent, solid: true, small: true)
            }
            HStack(spacing: 6) {
                PPTag("Legal", icon: .briefcase)
                PPTag("Turkish", selected: true)
                PPTag("/wodo/hr", onRemove: {})
            }
            PPCard(elevation: .one) {
                Text("Monthly usage").ppText(.h4).foregroundStyle(PP.Colors.text)
                PPMeter("Running hours", value: "6.2 of 10 h", fraction: 0.62)
                PPMeter("Output tokens", value: "88K of 100K", fraction: 0.88)
            }
            VStack(spacing: 0) {
                PPSectionHeader("Account")
                PPListRow("Mailboxes", subtitle: "/bekir, /bekir/agent1", showsChevron: true) { PPRowIcon(.inbox, tone: .accent) }
                PPListRow("Credits", value: "$42.50", showsChevron: true) { PPRowIcon(.coins, tone: .success) }
                PPListRow("Sign out", destructive: true) { PPRowIcon(.logOut, tone: .danger) }
            }
            .background(RoundedRectangle(cornerRadius: PP.Radius.l).fill(PP.Colors.surface))
            PPEmptyState(icon: .inbox, title: "No messages yet", message: "Share your address, /ada, and new conversations show up here.",
                         primary: ("Copy address", {}), secondary: ("Find people", {}))
            PPProgress(value: 0.4, label: "Uploading contract.pdf")
            HStack(spacing: 12) {
                PPSkeleton(.avatar())
                VStack(alignment: .leading, spacing: 8) { PPSkeleton(.title(width: 140)); PPSkeleton(.text()) }
            }
        }
        .padding(20)
    }
    .background(PP.Colors.bg)
    .pigeonpostTheme()
}
