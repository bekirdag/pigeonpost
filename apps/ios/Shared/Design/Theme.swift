//  The palette and the type — the Pigeonpost design kit's, not the app's own.
//
//  Every colour here is a role from the kit's tokens (`PP.Colors`, generated from the design kit's
//  `css/tokens.css` into `Packages/PigeonpostUI`). The names below are the app's older vocabulary,
//  kept so the screens did not have to be rewritten to adopt the kit: `ink` is the kit's `text`,
//  `ground` its `surface`, `Mine` its `bubbleMine…`, and so on. Where the kit has no role of the
//  same meaning (the search hit, the code block, the toast) the value is built from the kit's brand
//  scales rather than invented.
//
//  Every token is adaptive. A phone turns the surfaces it owns — the list, the sheets, the
//  navigation bar — dark on its own, and a palette of fixed values then paints near-black text onto
//  them, which is what shipped in 1.0 (1). So each colour is asked which appearance it is being drawn
//  into, at draw time — the kit's tokens are built that way too.

import SwiftUI
#if canImport(PigeonpostUI)
import PigeonpostUI
#endif
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

enum Theme {
    private typealias K = PP.Colors

    // ---- accent ---------------------------------------------------------------------------------

    /// The accent: the send button, the tint on every control, the unread count. Signal Blue.
    static let accent = K.accent
    /// What is written on an accent fill.
    static let onAccent = K.onAccent
    /// An accent used as text — a link, a tappable word. Signal Blue deepened (light) or lifted
    /// (dark) until it passes 4.5:1 on the ground it sits on.
    static let link = K.textLink
    /// Pigeon Navy, for the brand mark only.
    static let brand = K.brand
    /// The older name for the accent, kept because "navy" is what every control's tint was called.
    static let navy = accent
    /// A dot that says "unread", beside a subject.
    static let blue = accent
    /// The one search result you are standing on. Violet rather than the app's own blue, so a hit
    /// is never mistaken for a selection or for a message of your own — both of which are blue.
    static let found = adaptive(light: 0x5735B0, dark: 0x9A85F2)
    static let green = K.success
    static let amber = K.warning
    static let danger = K.danger
    /// A solid red fill with white on it: the LIVE badge, a destructive button.
    static let dangerSolid = K.dangerSolid

    // ---- text -----------------------------------------------------------------------------------

    /// A name, a heading, anything that must be read first.
    static let ink = K.text
    /// What was said, in a message.
    static let body = K.textBody
    /// A preview, a caption, an inactive control.
    static let secondary = K.textMuted
    /// When it was said.
    static let muted = K.textFaint
    /// The quiet name over a column, the address under a title.
    static let quiet = K.textBody

    // ---- surfaces, back to front ----------------------------------------------------------------

    /// The window behind everything, and the sign-in screen.
    static let app = K.bg
    /// The conversations column.
    static let sidebar = K.bgSubtle
    /// The subjects column.
    static let column = K.surfaceSunken
    /// The recessed ground a conversation sits on.
    static let wash = K.bgSubtle
    /// The raised surfaces: a received message, a dialog, a sheet.
    static let ground = K.surface
    /// A field to type in, and the switcher's card.
    static let field = K.surfaceSunken
    /// The composer's card.
    static let composer = K.surface
    /// A row the pointer is resting on or that is chosen.
    static let selected = K.surfaceSelected
    /// The chosen segment of a segmented strip, and a disabled button's fill.
    static let chip = adaptive(light: 0xDFE3E9, dark: 0x1F252E)
    /// A small square tile behind an icon.
    static let tile = K.surfaceHover

    /// The hairline between columns and under a header.
    static let rule = K.borderSubtle
    /// The border of a card.
    static let stroke = K.border
    /// The border of a field or a button that is only an outline.
    static let strokeStrong = K.borderStrong

    /// Your own message: the kit's `bubbleMine` — Signal Blue at card strength, not a solid fill.
    enum Mine {
        static let fill = K.bubbleMine
        static let stroke = K.bubbleMineBorder
        static let rule = K.bubbleMineBorder
        static let text = K.bubbleMineText
        static let secondary = adaptive(light: 0x2C3E66, dark: 0xC8D9FA)
        static let header = adaptive(light: 0x3E5486, dark: 0xB3D0FF)
        static let meta = adaptive(light: 0x4F6391, dark: 0x9DB9EA)
        static let panel = adaptive(light: 0xD8E7FF, dark: 0x0E3570)
        static let panelText = adaptive(light: 0x0D418F, dark: 0xD8E7FF)
    }

    /// Someone else's message: the kit's `bubbleTheirs`.
    enum Theirs {
        static let fill = K.bubbleTheirs
        static let stroke = K.bubbleTheirsBorder
        static let text = K.bubbleTheirsText
        /// The green rail down the side of an agent's message.
        static let agentRail = K.bubbleAgentRail
    }

    /// Delivery ticks: grey until read, green once seen, the kit's `tick*` roles.
    enum Tick {
        static let sent = K.tickSent
        static let seen = K.tickSeen
        static let working = K.tickWorking
    }

    /// Inline and fenced code.
    enum Code {
        static let fill = adaptive(light: 0xF0F2F5, dark: 0x1F252E)
        static let stroke = K.border
        static let text = K.textBody
    }

    /// The toast carries white text in both appearances, so its fill cannot be `ink` — in the dark
    /// that token is nearly white, and white on white is a message nobody reads.
    static let toastFill = adaptive(light: 0x1A1F27, dark: 0x343C47)

    /// The layer over the chat doodle, and the one number worth touching. The artwork has no opacity
    /// of its own, so this alpha is its strength: in the dark it is all but the conversation ground
    /// itself, so the pattern is a texture you notice only if you look for it.
    static let doodleVeil = K.bgSubtle.opacity(doodleVeilAlpha)

    private static let doodleVeilAlpha = 0.9

    /// The six avatar tones, in the order the web app's `data-tone` uses them: navy, blue, teal,
    /// violet, amber, red — the kit's `PPTone.forPeer` order, with the kit's disc and ink colours,
    /// so a peer keeps one colour everywhere, which is the whole point of the tone hash.
    static let tones: [Color] = [
        K.brand,
        K.accentSubtle,
        adaptive(light: 0xD9F4F0, dark: 0x1D3B3E),
        adaptive(light: 0xEBE7FC, dark: 0x2F2E4A),
        K.warningFill,
        K.dangerFill,
    ]
    static let toneInk: [Color] = [
        K.surface,
        K.textLink,
        adaptive(light: 0x0E5049, dark: 0x6FD6C4),
        adaptive(light: 0x422C84, dark: 0xB9A2FF),
        K.warning,
        K.danger,
    ]

    /// The server's decisions, shown as chips, and the card a held request sits in — the kit's
    /// warning, success and danger roles.
    enum Pill {
        static let heldText = K.warning
        static let heldFill = K.warningFill
        static let heldStroke = K.warningBorder
        static let autoText = K.success
        static let autoFill = K.successFill
        static let autoStroke = K.successBorder
        static let blockedText = K.danger
        static let blockedFill = K.dangerFill
        static let blockedStroke = K.dangerBorder
    }

    /// A request that is waiting on you: amber, at card strength.
    enum Held {
        static let fill = K.warningFill
        static let stroke = K.warningBorder
        static let title = adaptive(light: 0x6E4106, dark: 0xF6D9A6)
        static let text = K.warning
        static let panel = adaptive(light: 0xFAE8C6, dark: 0x352812)
        static let panelText = adaptive(light: 0x6E4106, dark: 0xF0D6A4)
    }

    /// The kit's radii (`--pp-radius-*`). A message is `bubble` with a `bubbleTail` corner.
    enum Radius {
        static let xs = PP.Radius.xs
        static let s = PP.Radius.s
        static let m = PP.Radius.m
        static let l = PP.Radius.l
        static let xl = PP.Radius.xl
        static let xxl = PP.Radius.x2xl
        static let bubble = PP.Radius.bubble
        static let bubbleTail = PP.Radius.bubbletail
    }

    /// The kit's spacing scale (`--pp-space-*`).
    enum Space {
        static let s1 = PP.Space.s1
        static let s2 = PP.Space.s2
        static let s3 = PP.Space.s3
        static let s4 = PP.Space.s4
        static let s5 = PP.Space.s5
        static let s6 = PP.Space.s6
        static let s8 = PP.Space.s8
    }

    /// One token, two values, resolved at draw time rather than read once at launch — so the app
    /// follows a change of appearance without being restarted.
    ///
    /// Both platforms have the same idea under different names: a colour that is asked which
    /// appearance it is being drawn into. Neither branch decides anything the other does not; the
    /// values above are the single source, and this only says who is asking.
    private static func adaptive(light: UInt32, dark: UInt32) -> Color {
        #if canImport(UIKit)
        return Color(uiColor: UIColor { traits in
            UIColor(rgb: traits.userInterfaceStyle == .dark ? dark : light)
        })
        #elseif canImport(AppKit)
        return Color(
            nsColor: NSColor(name: nil) { appearance in
                // `bestMatch` rather than reading the name: an appearance can be a vibrant or
                // high-contrast variant, and those still answer this question correctly.
                let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
                return NSColor(rgb: isDark ? dark : light)
            }
        )
        #else
        return Color(hex: light)
        #endif
    }
}

// ---- type ---------------------------------------------------------------------------------------

/// Geist for words and Geist Mono for addresses and code, bundled with the app (SIL OFL 1.1, see
/// `Shared/Fonts/Geist-OFL.txt`) and registered through `UIAppFonts` / `ATSApplicationFontsPath`.
///
/// If a face is missing — a test binary built without the bundle, say — the system font of the same
/// size and weight stands in, so nothing ever renders in a fallback nobody chose.
enum Typeface {
    static func sansName(_ weight: Font.Weight) -> String {
        switch weight {
        case .bold, .heavy, .black: return "Geist-Bold"
        case .semibold: return "Geist-SemiBold"
        case .medium: return "Geist-Medium"
        default: return "Geist-Regular"
        }
    }

    static func monoName(_ weight: Font.Weight) -> String {
        switch weight {
        case .bold, .heavy, .black, .semibold: return "GeistMono-SemiBold"
        case .medium: return "GeistMono-Medium"
        default: return "GeistMono-Regular"
        }
    }

    /// Whether the bundled faces registered. Asked once.
    static let available: Bool = {
        #if canImport(UIKit)
        return UIFont(name: "Geist-Regular", size: 12) != nil && UIFont(name: "GeistMono-Regular", size: 12) != nil
        #elseif canImport(AppKit)
        return NSFont(name: "Geist-Regular", size: 12) != nil && NSFont(name: "GeistMono-Regular", size: 12) != nil
        #else
        return false
        #endif
    }()

    #if canImport(UIKit)
    typealias PlatformFont = UIFont
    #elseif canImport(AppKit)
    typealias PlatformFont = NSFont
    #endif

    #if canImport(UIKit) || canImport(AppKit)
    /// The same faces for the text views that render message bodies.
    static func platform(_ size: CGFloat, _ weight: Font.Weight = .regular, mono: Bool = false,
                         italic: Bool = false) -> PlatformFont {
        let name = italic && !mono && weight == .regular ? "Geist-Italic" : (mono ? monoName(weight) : sansName(weight))
        if available, let font = PlatformFont(name: name, size: size) { return font }
        let systemWeight: PlatformFont.Weight
        switch weight {
        case .bold, .heavy, .black: systemWeight = .bold
        case .semibold: systemWeight = .semibold
        case .medium: systemWeight = .medium
        default: systemWeight = .regular
        }
        return mono ? .monospacedSystemFont(ofSize: size, weight: systemWeight)
                    : .systemFont(ofSize: size, weight: systemWeight)
    }
    #endif
}

extension Font {
    /// Geist at a fixed size. Fixed, as the system sizes it replaced were.
    static func geist(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        guard Typeface.available else { return .system(size: size, weight: weight) }
        return .custom(Typeface.sansName(weight), fixedSize: size)
    }

    /// Geist Mono, for addresses, handles and code.
    static func geistMono(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        guard Typeface.available else { return .system(size: size, weight: weight, design: .monospaced) }
        return .custom(Typeface.monoName(weight), fixedSize: size)
    }
}

extension Color {
    init(hex: UInt32) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            opacity: 1
        )
    }
}

#if canImport(UIKit)
extension UIColor {
    convenience init(rgb: UInt32) {
        self.init(
            red: CGFloat((rgb >> 16) & 0xFF) / 255,
            green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255,
            alpha: 1
        )
    }
}
#elseif canImport(AppKit)
extension NSColor {
    convenience init(rgb: UInt32) {
        self.init(
            srgbRed: CGFloat((rgb >> 16) & 0xFF) / 255,
            green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255,
            alpha: 1
        )
    }
}
#endif
