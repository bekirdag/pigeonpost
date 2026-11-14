//  Commerce: PPPricingCard, PPCreditBalance (wallet card), PPCreditChip, PPListingCard,
//  PPNestCard with PPNestStatus. Money is USD credits, shown with two decimals.

import SwiftUI

/// Formats a USD amount the way the kit shows credits: "$42.50".
public enum PPMoney {
    public static func usd(_ amount: Decimal, locale: Locale = Locale(identifier: "en_US")) -> String {
        amount.formatted(.currency(code: "USD").locale(locale).precision(.fractionLength(2)))
    }
}

// MARK: - Pricing card

public struct PPPlanFeature: Identifiable, Hashable, Sendable {
    public let id = UUID()
    public let text: String
    public let included: Bool
    public init(_ text: String, included: Bool = true) {
        self.text = text
        self.included = included
    }
}

/// A plan in the pricing grid (`.pp-price-card`): name, description, 40pt price, CTA and features.
/// `featured` adds the accent ring, tinted top and badge. `isCurrent` disables the CTA.
public struct PPPricingCard: View {
    let name: String
    let icon: PPIconName?
    let description: String
    let price: String
    let unit: String
    let note: String?
    let badge: String?
    let featured: Bool
    let isCurrent: Bool
    let ctaTitle: String
    let features: [PPPlanFeature]
    let onSelect: () -> Void

    public init(name: String, icon: PPIconName? = nil, description: String, price: String, unit: String = "/ month",
                note: String? = nil, badge: String? = nil, featured: Bool = false, isCurrent: Bool = false,
                ctaTitle: String, features: [PPPlanFeature], onSelect: @escaping () -> Void) {
        self.name = name
        self.icon = icon
        self.description = description
        self.price = price
        self.unit = unit
        self.note = note
        self.badge = badge
        self.featured = featured
        self.isCurrent = isCurrent
        self.ctaTitle = ctaTitle
        self.features = features
        self.onSelect = onSelect
    }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: PP.Radius.xl, style: .continuous)
        VStack(alignment: .leading, spacing: PP.Space.s4) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    if let icon { PPIcon(icon, size: 20).foregroundStyle(PP.Colors.accent) }
                    Text(name).font(.geist(16, .semibold)).foregroundStyle(PP.Colors.text)
                }
                .accessibilityAddTraits(.isHeader)
                Text(description).ppText(.bodyS).foregroundStyle(PP.Colors.textMuted)
            }
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(price).font(.geist(40, .semibold)).tracking(-0.4).monospacedDigit().foregroundStyle(PP.Colors.text)
                    Text(unit).font(.geist(13.5)).foregroundStyle(PP.Colors.textMuted)
                }
                Text(note ?? " ").font(.geist(12)).foregroundStyle(PP.Colors.textFaint)
            }
            .accessibilityElement(children: .combine)
            PPButton(isCurrent ? "Current plan" : ctaTitle, variant: featured ? .primary : .outline, size: .large, fullWidth: true, action: onSelect)
                .disabled(isCurrent)
            VStack(alignment: .leading, spacing: 10) {
                ForEach(features) { f in
                    HStack(alignment: .top, spacing: 10) {
                        PPIcon(f.included ? .check : .minus, size: 18, weight: f.included ? 2.25 : 1.75)
                            .foregroundStyle(f.included ? PP.Colors.success : PP.Colors.textFaint)
                        Text(f.text).ppText(.bodyS).foregroundStyle(f.included ? PP.Colors.textBody : PP.Colors.textFaint)
                    }
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(Text(f.included ? f.text : "Not included: \(f.text)"))
                }
            }
            .padding(.top, PP.Space.s4)
            .overlay(alignment: .top) { Rectangle().fill(PP.Colors.borderSubtle).frame(height: 1) }
        }
        .padding(PP.Space.s6)
        .background {
            if featured {
                shape.fill(LinearGradient(stops: [.init(color: PP.Colors.accentSubtle, location: 0), .init(color: PP.Colors.surface, location: 0.3)],
                                          startPoint: .top, endPoint: .bottom))
            } else {
                shape.fill(PP.Colors.surface)
            }
        }
        .overlay(shape.strokeBorder(featured ? PP.Colors.accent : PP.Colors.border, lineWidth: featured ? 2 : 1))
        .ppShadow(featured ? .three : .one)
        .overlay(alignment: .topLeading) {
            if let badge {
                Text(badge)
                    .font(.geist(11.5, .semibold)).tracking(0.2)
                    .foregroundStyle(PP.Colors.onAccent)
                    .padding(.horizontal, 10)
                    .frame(height: 22)
                    .background(Capsule().fill(PP.Colors.accent))
                    .offset(x: PP.Space.s6, y: -11)
            }
        }
    }
}

// MARK: - Credits

public enum PPCreditLevel: Sendable { case normal, low, empty }

/// The wallet card (`.pp-wallet`): label, 40pt balance, held amount and actions.
public struct PPCreditBalance: View {
    let balance: Decimal
    let held: Decimal?
    let level: PPCreditLevel
    let onTopUp: () -> Void
    let onHistory: (() -> Void)?

    public init(balance: Decimal, held: Decimal? = nil, level: PPCreditLevel? = nil,
                onTopUp: @escaping () -> Void, onHistory: (() -> Void)? = nil) {
        self.balance = balance
        self.held = held
        self.level = level ?? (balance <= 0 ? .empty : (balance < 5 ? .low : .normal))
        self.onTopUp = onTopUp
        self.onHistory = onHistory
    }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: PP.Radius.xl, style: .continuous)
        VStack(alignment: .leading, spacing: PP.Space.s4) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    PPIcon(.coins, size: 18).foregroundStyle(PP.Colors.success)
                    Text("Credit balance").font(.geist(13.5, .medium)).foregroundStyle(PP.Colors.textMuted)
                }
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(PPMoney.usd(balance)).font(.geist(40, .semibold)).tracking(-0.4).monospacedDigit()
                        .foregroundStyle(level == .normal ? PP.Colors.text : (level == .low ? PP.Colors.warning : PP.Colors.danger))
                    Text("USD").font(.geist(15, .medium)).foregroundStyle(PP.Colors.textMuted)
                }
                if let held, held > 0 {
                    Text("\(PPMoney.usd(held)) held for running work").font(.geist(13.5)).monospacedDigit().foregroundStyle(PP.Colors.textMuted)
                }
            }
            .accessibilityElement(children: .combine)
            if level != .normal {
                PPBanner(level == .low ? .warning : .danger,
                         message: level == .low ? "Credits are running low. Agents stop replying at $0.00." : "No credits left. Paid agents are paused until you top up.")
            }
            HStack(spacing: PP.Space.s2) {
                PPButton("Top up", icon: .plus, variant: .primary, action: onTopUp)
                if let onHistory { PPButton("History", icon: .history, variant: .outline, action: onHistory) }
            }
        }
        .padding(PP.Space.s6)
        .frame(maxWidth: 560, alignment: .leading)
        .background(shape.fill(PP.Colors.surface))
        .overlay(shape.strokeBorder(level == .low ? PP.Colors.warningBorder : (level == .empty ? PP.Colors.dangerBorder : PP.Colors.border), lineWidth: 1))
        .ppShadow(.one)
    }
}

/// The header wallet pill (`.pp-credit-chip`), 34pt high, with an optional "+" top-up.
public struct PPCreditChip: View {
    let balance: Decimal
    let level: PPCreditLevel
    let onTap: () -> Void
    let showsAdd: Bool

    public init(balance: Decimal, showsAdd: Bool = true, onTap: @escaping () -> Void) {
        self.balance = balance
        self.level = balance <= 0 ? .empty : (balance < 5 ? .low : .normal)
        self.showsAdd = showsAdd
        self.onTap = onTap
    }

    public var body: some View {
        let (bg, bd, ink): (Color, Color, Color) = {
            switch level {
            case .normal: return (PP.Colors.surface, PP.Colors.borderStrong, PP.Colors.text)
            case .low: return (PP.Colors.warningFill, PP.Colors.warningBorder, PP.Colors.warning)
            case .empty: return (PP.Colors.dangerFill, PP.Colors.dangerBorder, PP.Colors.danger)
            }
        }()
        Button(action: onTap) {
            HStack(spacing: 8) {
                PPIcon(.coins, size: 16).foregroundStyle(level == .normal ? PP.Colors.success : ink)
                Text(PPMoney.usd(balance)).font(.geist(13.5, .semibold)).monospacedDigit()
                if showsAdd {
                    PPIcon(.plus, size: 14, weight: 2.5)
                        .foregroundStyle(PP.Colors.onAccent)
                        .frame(width: 24, height: 24)
                        .background(Circle().fill(PP.Colors.accent))
                }
            }
            .foregroundStyle(ink)
            .padding(.leading, 12)
            .padding(.trailing, showsAdd ? 6 : 12)
            .frame(height: 34)
            .background(Capsule().fill(bg))
            .overlay(Capsule().strokeBorder(bd, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .frame(minHeight: 44)
        .accessibilityLabel(Text("Credits, \(PPMoney.usd(balance))"))
        .accessibilityHint(Text(showsAdd ? "Top up" : "Open wallet"))
    }
}

// MARK: - Marketplace listing

/// An agent or service in the marketplace (`.pp-listing-card`).
public struct PPListingCard: View {
    let title: String
    let publisher: String
    let verified: Bool
    let logoText: String?
    let logoIcon: PPIconName?
    let logoTone: PPTone
    let category: String
    let categoryIcon: PPIconName?
    let description: String
    let price: String
    let priceUnit: String
    let host: String
    let comingSoon: Bool

    public init(title: String, publisher: String, verified: Bool = false, logoText: String? = nil, logoIcon: PPIconName? = nil,
                logoTone: PPTone = .navy, category: String, categoryIcon: PPIconName? = nil, description: String,
                price: String, priceUnit: String, host: String, comingSoon: Bool = false) {
        self.title = title
        self.publisher = publisher
        self.verified = verified
        self.logoText = logoText
        self.logoIcon = logoIcon
        self.logoTone = logoTone
        self.category = category
        self.categoryIcon = categoryIcon
        self.description = description
        self.price = price
        self.priceUnit = priceUnit
        self.host = host
        self.comingSoon = comingSoon
    }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: PP.Radius.xl, style: .continuous)
        VStack(alignment: .leading, spacing: PP.Space.s3) {
            HStack(spacing: PP.Space.s3) {
                ZStack {
                    RoundedRectangle(cornerRadius: PP.Radius.l, style: .continuous).fill(logoTone.solid)
                    if let logoIcon { PPIcon(logoIcon, size: 24) } else { Text(logoText ?? "").font(.geist(17, .bold)).tracking(-0.2) }
                }
                .foregroundStyle(.white)
                .frame(width: 48, height: 48)
                .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).font(.geist(16, .semibold)).foregroundStyle(PP.Colors.text).lineLimit(1)
                    HStack(spacing: 6) {
                        Text(publisher).font(.geistMono(12))
                        if verified {
                            HStack(spacing: 3) { PPIcon(.badgeCheck, size: 15); Text("Verified") }
                                .font(.geist(12, .medium)).foregroundStyle(PP.Colors.textLink)
                        }
                    }
                    .foregroundStyle(PP.Colors.textMuted)
                }
                Spacer(minLength: comingSoon ? 48 : 0)
            }
            HStack(spacing: 5) {
                if let categoryIcon { PPIcon(categoryIcon, size: 13) }
                Text(category)
            }
            .font(.geist(12, .medium))
            .foregroundStyle(PP.Colors.textMuted)
            .padding(.horizontal, 9)
            .frame(height: 24)
            .background(Capsule().fill(PP.Colors.surfaceSunken))
            .overlay(Capsule().strokeBorder(PP.Colors.borderSubtle))
            Text(description).ppText(.bodyS).foregroundStyle(PP.Colors.textBody).lineLimit(3)
            HStack(alignment: .bottom) {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(price).font(.geist(15, .semibold)).monospacedDigit()
                        .foregroundStyle(comingSoon ? PP.Colors.textFaint : PP.Colors.text)
                    Text(priceUnit).font(.geist(12)).foregroundStyle(PP.Colors.textMuted)
                }
                Spacer(minLength: 8)
                HStack(spacing: 4) { PPIcon(.server, size: 13); Text(host) }
                    .font(.geist(12)).foregroundStyle(PP.Colors.textFaint)
            }
            .padding(.top, PP.Space.s3)
            .overlay(alignment: .top) { Rectangle().fill(PP.Colors.borderSubtle).frame(height: 1) }
        }
        .padding(PP.Space.s5)
        .background(shape.fill(PP.Colors.surface))
        .overlay(shape.strokeBorder(PP.Colors.border, lineWidth: 1))
        .overlay(alignment: .topTrailing) {
            if comingSoon {
                Text("Coming soon")
                    .font(.geist(11, .semibold))
                    .foregroundStyle(PP.Colors.textLink)
                    .padding(.horizontal, 8)
                    .frame(height: 22)
                    .background(Capsule().fill(PP.Colors.accentSubtle))
                    .padding(12)
            }
        }
        .clipShape(shape)
        .ppShadow(.one)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
    }
}

// MARK: - Nest

public enum PPNestStatus: String, CaseIterable, Sendable {
    case running, sleeping, setup, paused, error

    public var title: String {
        switch self {
        case .running: return "Running"
        case .sleeping: return "Sleeping"
        case .setup: return "Setting up"
        case .paused: return "Paused"
        case .error: return "Stopped"
        }
    }

    var colors: (Color, Color, Color, Color) { // dot, ink, fill, border
        switch self {
        case .running: return (PP.Colors.successSolid, PP.Colors.success, PP.Colors.successFill, PP.Colors.successBorder)
        case .sleeping: return (PP.Colors.textFaint, PP.Colors.textMuted, PP.Colors.surfaceSunken, PP.Colors.borderSubtle)
        case .setup: return (PP.Colors.infoSolid, PP.Colors.info, PP.Colors.infoFill, PP.Colors.infoBorder)
        case .paused: return (PP.Colors.warningSolid, PP.Colors.warning, PP.Colors.warningFill, PP.Colors.warningBorder)
        case .error: return (PP.Colors.dangerSolid, PP.Colors.danger, PP.Colors.dangerFill, PP.Colors.dangerBorder)
        }
    }
}

/// `.pp-nest-status`: a dot and a word in a 24pt pill.
public struct PPNestStatusPill: View {
    let status: PPNestStatus
    let detail: String?

    public init(_ status: PPNestStatus, detail: String? = nil) {
        self.status = status
        self.detail = detail
    }

    public var body: some View {
        let (dot, ink, fill, border) = status.colors
        HStack(spacing: 6) {
            Circle().fill(dot).frame(width: 8, height: 8)
                .background(Circle().fill(dot.opacity(status == .running ? 0.25 : 0)).frame(width: 14, height: 14))
            Text(detail.map { "\(status.title) · \($0)" } ?? status.title).lineLimit(1)
        }
        .font(.geist(12, .medium))
        .foregroundStyle(ink)
        .padding(.leading, 8)
        .padding(.trailing, 9)
        .frame(height: 24)
        .background(Capsule().fill(fill))
        .overlay(Capsule().strokeBorder(border))
        .accessibilityElement(children: .combine)
    }
}

public struct PPNestUsage: Identifiable, Sendable {
    public let id = UUID()
    public let label: String
    public let value: String
    public let fraction: Double
    public init(_ label: String, value: String, fraction: Double) {
        self.label = label
        self.value = value
        self.fraction = fraction
    }
}

/// A Nest (a hosted agent with its own workspace): avatar, name, handle, status, package line,
/// usage meters, note and actions.
public struct PPNestCard: View {
    let name: String
    let handle: String
    let status: PPNestStatus
    let statusDetail: String?
    let packageName: String
    let packageDetail: String
    let usage: [PPNestUsage]
    let note: String?
    let onOpen: () -> Void
    let onSettings: (() -> Void)?

    public init(name: String, handle: String, status: PPNestStatus, statusDetail: String? = nil, packageName: String,
                packageDetail: String, usage: [PPNestUsage] = [], note: String? = nil,
                onOpen: @escaping () -> Void, onSettings: (() -> Void)? = nil) {
        self.name = name
        self.handle = handle
        self.status = status
        self.statusDetail = statusDetail
        self.packageName = packageName
        self.packageDetail = packageDetail
        self.usage = usage
        self.note = note
        self.onOpen = onOpen
        self.onSettings = onSettings
    }

    public var body: some View {
        let shape = RoundedRectangle(cornerRadius: PP.Radius.xl, style: .continuous)
        VStack(alignment: .leading, spacing: PP.Space.s4) {
            HStack(spacing: PP.Space.s3) {
                avatar
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(.geist(15, .semibold)).foregroundStyle(PP.Colors.text).lineLimit(1)
                    Text(handle).font(.geistMono(12.5)).foregroundStyle(PP.Colors.textMuted).lineLimit(1)
                }
                Spacer(minLength: 8)
                PPNestStatusPill(status, detail: statusDetail)
            }
            .accessibilityElement(children: .combine)
            HStack {
                Text(packageName).fontWeight(.semibold).foregroundStyle(PP.Colors.text)
                Spacer(minLength: 8)
                Text(packageDetail).foregroundStyle(PP.Colors.textMuted)
            }
            .font(.geist(13.5))
            .padding(.vertical, 10)
            .padding(.horizontal, 12)
            .background(RoundedRectangle(cornerRadius: PP.Radius.m).fill(PP.Colors.surfaceSunken))
            .accessibilityElement(children: .combine)
            if !usage.isEmpty {
                VStack(spacing: PP.Space.s3) {
                    ForEach(usage) { PPMeter($0.label, value: $0.value, fraction: $0.fraction) }
                }
            }
            if let note { Text(note).ppText(.bodyS).foregroundStyle(PP.Colors.textMuted) }
            HStack(spacing: PP.Space.s2) {
                PPButton("Open in inbox", icon: .inbox, variant: .primary, action: onOpen)
                if let onSettings { PPIconButton(.settings, label: "Nest settings", action: onSettings) }
            }
        }
        .padding(PP.Space.s5)
        .background(shape.fill(PP.Colors.surface))
        .overlay(shape.strokeBorder(PP.Colors.border, lineWidth: 1))
        .ppShadow(.one)
    }

    @ViewBuilder private var avatar: some View {
        switch status {
        case .setup:
            PPIcon(.egg, size: 22)
                .foregroundStyle(PP.Colors.textMuted)
                .frame(width: 44, height: 44)
                .background(Circle().fill(PP.Colors.surfaceSunken))
                .overlay(Circle().strokeBorder(PP.Colors.borderStrong, style: StrokeStyle(lineWidth: 1, dash: [3, 3])))
        case .sleeping:
            PPIcon(.moon, size: 22).foregroundStyle(.white).frame(width: 44, height: 44)
                .background(Circle().fill(LinearGradient(colors: [PP.Palette.gray400, PP.Palette.gray600], startPoint: .topLeading, endPoint: .bottomTrailing)))
        default:
            PPIcon(.bird, size: 22).foregroundStyle(.white).frame(width: 44, height: 44)
                .background(Circle().fill(LinearGradient(colors: [PP.Palette.blue500, PP.Palette.navy700], startPoint: .topLeading, endPoint: .bottomTrailing)))
        }
    }
}

#Preview("Commerce") {
    ScrollView {
        VStack(spacing: 24) {
            HStack { Spacer(); PPCreditChip(balance: 42.5) {}; PPCreditChip(balance: 2.4) {}; PPCreditChip(balance: 0, showsAdd: false) {} }
            PPCreditBalance(balance: 42.5, held: 3.2, onTopUp: {}, onHistory: {})
            PPCreditBalance(balance: 2.4, onTopUp: {})
            PPPricingCard(name: "Pro", icon: .sparkles, description: "For people who work with agents every day.", price: "$12", unit: "/ month",
                          note: "Billed yearly, $144", badge: "Most popular", featured: true, ctaTitle: "Choose Pro",
                          features: [.init("5 mailboxes, including /you/agent1"), .init("Meeting summaries"), .init("Calls with up to 30 people"), .init("Custom domain", included: false)]) {}
            PPPricingCard(name: "Free", description: "One address and the basics.", price: "$0", isCurrent: true, ctaTitle: "Choose Free",
                          features: [.init("1 mailbox"), .init("1:1 calls"), .init("Meeting summaries", included: false)]) {}
            PPListingCard(title: "Contract Review", publisher: "/lexa", verified: true, logoText: "Lx", logoTone: .violet, category: "Legal", categoryIcon: .briefcase,
                          description: "Send a contract as an attachment; get back the risky clauses, what they mean and a suggested change, in English or Turkish.",
                          price: "$25.00", priceUnit: "per run", host: "Hosted by the publisher")
            PPListingCard(title: "Translate to Turkish", publisher: "/dil", logoIcon: .languages, logoTone: .teal, category: "Language",
                          description: "Documents and messages translated by a reviewed agent.", price: "$0.02", priceUnit: "per 100 words", host: "Runs in a Nest", comingSoon: true)
            PPNestCard(name: "Ada's assistant", handle: "/ada/nest", status: .running, packageName: "Pigeonpost AI", packageDetail: "$10 / month · renews 2 Dec",
                       usage: [.init("Running hours", value: "6.2 of 10 h", fraction: 0.62), .init("Input tokens", value: "410K of 1M", fraction: 0.41),
                               .init("Output tokens", value: "88K of 100K", fraction: 0.88)],
                       onOpen: {}, onSettings: {})
            PPNestCard(name: "HR helper", handle: "/wodo/hr", status: .sleeping, packageName: "Pigeonpost AI", packageDetail: "$10 / month",
                       note: "Wakes when a message arrives. Sleeping does not use running hours.", onOpen: {})
        }
        .padding(20)
    }
    .background(PP.Colors.bg)
    .pigeonpostTheme()
}
