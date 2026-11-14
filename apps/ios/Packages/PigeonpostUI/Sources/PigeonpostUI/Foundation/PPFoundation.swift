//  Pigeonpost UI foundation: fonts, type roles, theme, shadows, focus ring and the colours the
//  generated tokens do not carry (focus ring, scrim, skeleton). Everything else reads `PP.*` from
//  Generated/PPTokens.swift, which tools/build_tokens.py writes from css/tokens.css.

import SwiftUI
import CoreText
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

// MARK: - Colour helpers

extension Color {
    /// A colour from a 0xRRGGBB literal.
    init(ppHex hex: UInt32, opacity: Double = 1) {
        self.init(.sRGB,
                  red: Double((hex >> 16) & 0xFF) / 255,
                  green: Double((hex >> 8) & 0xFF) / 255,
                  blue: Double(hex & 0xFF) / 255,
                  opacity: opacity)
    }

    /// A colour that follows the system (or `.pigeonpostTheme(_:)`) appearance.
    static func ppDynamic(light: Color, dark: Color) -> Color {
        #if canImport(UIKit)
        return Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(dark) : UIColor(light) })
        #else
        return Color(NSColor(name: nil) { $0.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? NSColor(dark) : NSColor(light) })
        #endif
    }

    static func ppDynamic(light: UInt32, dark: UInt32) -> Color {
        ppDynamic(light: Color(ppHex: light), dark: Color(ppHex: dark))
    }
}

public extension PP.Colors {
    /// `--pp-focus-ring`: the 3pt halo around a focused control.
    static let focusRing = Color.ppDynamic(light: PP.Effects.focusRingLight, dark: PP.Effects.focusRingDark)
    /// `--pp-scrim`: behind sheets and dialogs.
    static let scrim = Color.ppDynamic(light: PP.Effects.scrimLight, dark: PP.Effects.scrimDark)
    /// The two stops of `--pp-skeleton`.
    static let skeletonBase = Color.ppDynamic(light: 0xF0F2F5, dark: 0x171C24)
    static let skeletonHighlight = Color.ppDynamic(light: 0xE6E9EE, dark: 0x1F2530)
}

// MARK: - Fonts

/// Registers the bundled Geist and Geist Mono faces (SIL OFL 1.1). Call once at launch, or let
/// `.pigeonpostTheme()` do it. Safe to call more than once.
public enum PPFonts {
    public static let sansFaces = ["Geist-Regular", "Geist-Medium", "Geist-SemiBold", "Geist-Bold", "Geist-Italic"]
    public static let monoFaces = ["GeistMono-Regular", "GeistMono-Medium", "GeistMono-SemiBold"]

    private static let lock = NSLock()
    nonisolated(unsafe) private static var registered = false

    @discardableResult
    public static func register() -> Bool {
        lock.lock(); defer { lock.unlock() }
        if registered { return true }
        let urls = Bundle.module.urls(forResourcesWithExtension: "ttf", subdirectory: "Fonts") ?? []
        for url in urls {
            // Already-registered faces (the host app may bundle Geist too) report an error; ignore it.
            CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        }
        registered = isAvailable
        return registered
    }

    /// Whether Geist resolves by PostScript name (bundled here or by the host app).
    public static var isAvailable: Bool {
        #if canImport(UIKit)
        return UIFont(name: "Geist-Regular", size: 12) != nil && UIFont(name: "GeistMono-Regular", size: 12) != nil
        #else
        return NSFont(name: "Geist-Regular", size: 12) != nil && NSFont(name: "GeistMono-Regular", size: 12) != nil
        #endif
    }

    static func sansName(_ weight: SwiftUI.Font.Weight) -> String {
        switch weight {
        case .bold, .heavy, .black: return "Geist-Bold"
        case .semibold: return "Geist-SemiBold"
        case .medium: return "Geist-Medium"
        default: return "Geist-Regular"
        }
    }

    static func monoName(_ weight: SwiftUI.Font.Weight) -> String {
        switch weight {
        case .bold, .heavy, .black, .semibold: return "GeistMono-SemiBold"
        case .medium: return "GeistMono-Medium"
        default: return "GeistMono-Regular"
        }
    }
}

public extension SwiftUI.Font {
    /// Geist at a size and weight, scaled with Dynamic Type relative to `relativeTo`.
    static func geist(_ size: CGFloat, _ weight: SwiftUI.Font.Weight = .regular, relativeTo style: SwiftUI.Font.TextStyle = .body) -> SwiftUI.Font {
        PPFonts.register()
        return .custom(PPFonts.sansName(weight), size: size, relativeTo: style)
    }

    /// Geist Mono, for handles, addresses, codes and amounts in tables.
    static func geistMono(_ size: CGFloat, _ weight: SwiftUI.Font.Weight = .regular, relativeTo style: SwiftUI.Font.TextStyle = .body) -> SwiftUI.Font {
        PPFonts.register()
        return .custom(PPFonts.monoName(weight), size: size, relativeTo: style)
    }
}

/// The kit's type roles (`.pp-h1` … `.pp-overline`), with the token sizes and line heights.
public enum PPTextRole: String, CaseIterable, Sendable {
    case displayXl, displayL, displayM, h1, h2, h3, h4, bodyL, body, bodyS, caption, overline

    public var size: CGFloat {
        switch self {
        case .displayXl: return 56
        case .displayL: return 44
        case .displayM: return 36
        case .h1: return 28
        case .h2: return 22
        case .h3: return 18
        case .h4: return 16
        case .bodyL: return 17
        case .body: return 15
        case .bodyS: return 13.5
        case .caption: return 12
        case .overline: return 11.5
        }
    }

    public var lineHeight: CGFloat {
        switch self {
        case .displayXl: return PP.LineHeight.displayXl
        case .displayL: return PP.LineHeight.displayL
        case .displayM: return PP.LineHeight.displayM
        case .h1: return PP.LineHeight.h1
        case .h2: return PP.LineHeight.h2
        case .h3: return PP.LineHeight.h3
        case .h4: return PP.LineHeight.h4
        case .bodyL: return PP.LineHeight.bodyL
        case .body: return PP.LineHeight.body
        case .bodyS: return PP.LineHeight.bodyS
        case .caption: return PP.LineHeight.caption
        case .overline: return PP.LineHeight.overline
        }
    }

    public var weight: SwiftUI.Font.Weight {
        switch self {
        case .displayXl, .displayL, .displayM, .h1, .h2, .h3, .h4, .overline: return .semibold
        default: return .regular
        }
    }

    var textStyle: SwiftUI.Font.TextStyle {
        switch self {
        case .displayXl, .displayL, .displayM: return .largeTitle
        case .h1: return .title
        case .h2: return .title2
        case .h3: return .title3
        case .h4: return .headline
        case .bodyL, .body: return .body
        case .bodyS: return .subheadline
        case .caption: return .caption
        case .overline: return .caption2
        }
    }

    public var font: SwiftUI.Font { .geist(size, weight, relativeTo: textStyle) }
}

public extension View {
    /// Applies a type role: Geist at the role's size and weight, with its line height and, for
    /// `overline`, the uppercase tracking.
    func ppText(_ role: PPTextRole, weight: SwiftUI.Font.Weight? = nil) -> some View {
        self.font(.geist(role.size, weight ?? role.weight, relativeTo: role.textStyle))
            .lineSpacing(max(0, role.lineHeight - role.size * 1.2))
            .tracking(role == .overline ? 0.7 : (role.size >= 28 ? -0.2 : 0))
            .textCase(role == .overline ? .uppercase : nil)
    }
}

// MARK: - Shadows and focus

public enum PPElevation: Int, Sendable { case none = 0, one, two, three, four }

public extension View {
    /// `--pp-shadow-1…4`. Dark mode deepens the shadow, as the tokens do.
    func ppShadow(_ level: PPElevation) -> some View {
        modifier(PPShadowModifier(level: level))
    }

    /// The kit's focus treatment: a 3pt `focusRing` halo in `shape`.
    func ppFocusRing<S: InsettableShape>(_ visible: Bool, in shape: S) -> some View {
        overlay(shape.inset(by: -3).stroke(PP.Colors.focusRing, lineWidth: visible ? 3 : 0))
    }
}

struct PPShadowModifier: ViewModifier {
    let level: PPElevation
    @Environment(\.colorScheme) private var scheme

    func body(content: Content) -> some View {
        let dark = scheme == .dark
        switch level {
        case .none:
            content
        case .one:
            content.shadow(color: .black.opacity(dark ? 0.4 : 0.06), radius: 1, y: 1)
        case .two:
            content.shadow(color: .black.opacity(dark ? 0.45 : 0.08), radius: 6, y: 4)
                .shadow(color: .black.opacity(dark ? 0 : 0.05), radius: 1, y: 1)
        case .three:
            content.shadow(color: .black.opacity(dark ? 0.55 : 0.12), radius: 16, y: 12)
        case .four:
            content.shadow(color: .black.opacity(dark ? 0.65 : 0.20), radius: 30, y: 24)
        }
    }
}

// MARK: - Theme

/// Kit-wide settings read by the components. Set with `.pigeonpostTheme(_:)`.
public struct PPTheme: Equatable, Sendable {
    /// Forces light or dark; `nil` follows the system.
    public var colorScheme: ColorScheme?
    /// Larger controls for touch-first surfaces (44pt minimum everywhere).
    public var touch: Bool
    /// Honour Reduce Motion in addition to the system setting (for screenshots and tests).
    public var reduceMotion: Bool

    public init(colorScheme: ColorScheme? = nil, touch: Bool = true, reduceMotion: Bool = false) {
        self.colorScheme = colorScheme
        self.touch = touch
        self.reduceMotion = reduceMotion
    }

    public static let standard = PPTheme()
}

private struct PPThemeKey: EnvironmentKey {
    static let defaultValue = PPTheme.standard
}

public extension EnvironmentValues {
    var ppTheme: PPTheme {
        get { self[PPThemeKey.self] }
        set { self[PPThemeKey.self] = newValue }
    }
}

public extension View {
    /// Registers Geist, sets the accent tint, the background and (optionally) a fixed appearance.
    /// Put it once at the root of the app.
    func pigeonpostTheme(_ theme: PPTheme = .standard) -> some View {
        PPFonts.register()
        return self
            .environment(\.ppTheme, theme)
            .tint(PP.Colors.accent)
            .font(PPTextRole.body.font)
            .foregroundStyle(PP.Colors.textBody)
            .modifier(PPSchemeModifier(scheme: theme.colorScheme))
    }
}

struct PPSchemeModifier: ViewModifier {
    let scheme: ColorScheme?
    func body(content: Content) -> some View {
        if let scheme {
            content.environment(\.colorScheme, scheme).preferredColorScheme(scheme)
        } else {
            content
        }
    }
}

/// True when animations should be skipped (system Reduce Motion or `PPTheme.reduceMotion`).
struct PPMotion {
    static func reduced(_ env: Bool, _ theme: PPTheme) -> Bool { env || theme.reduceMotion }
}
