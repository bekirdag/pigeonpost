//  The palette and the type.
//
//  Dark is the conversation design's own values, exactly: a near-black ground in three steps
//  (sidebar, subjects, conversation), raised cards a step lighter with a hairline border, a soft
//  blue accent, and amber and green used only for what the server decided. Light is the same design
//  language re-derived for paper — the greys keep their order and their spacing, the tinted cards
//  keep their hue, and the accent deepens until it reads on white.
//
//  Every token is adaptive. A phone turns the surfaces it owns — the list, the sheets, the
//  navigation bar — dark on its own, and a palette of fixed values then paints near-black text onto
//  them, which is what shipped in 1.0 (1). So each colour is asked which appearance it is being drawn
//  into, at draw time.

import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

enum Theme {
    // ---- accent ---------------------------------------------------------------------------------

    /// The accent: links, the send button, the tint on every control, the unread count.
    static let accent = adaptive(light: 0x2F6BE0, dark: 0x8AB4FF)
    /// What is written on an accent fill.
    static let onAccent = adaptive(light: 0xFFFFFF, dark: 0x0D0F12)
    /// The older name for the accent, kept because "navy" is what every control's tint was called.
    static let navy = accent
    /// A dot that says "unread", beside a subject.
    static let blue = accent
    /// The one search result you are standing on. Purple rather than the app's own blue, so a hit
    /// is never mistaken for a selection or for a message of your own — both of which are blue.
    static let found = adaptive(light: 0x7C3AED, dark: 0xA78BFA)
    static let green = adaptive(light: 0x1E8A50, dark: 0x7DD3A0)
    static let amber = adaptive(light: 0xA35F00, dark: 0xF0B860)

    // ---- text -----------------------------------------------------------------------------------

    /// A name, a heading, anything that must be read first.
    static let ink = adaptive(light: 0x14181F, dark: 0xE8EAED)
    /// What was said, in a message.
    static let body = adaptive(light: 0x2E353F, dark: 0xD7DCE3)
    /// A preview, a caption, an inactive control.
    static let secondary = adaptive(light: 0x5B6470, dark: 0x9AA3AE)
    /// When it was said.
    static let muted = adaptive(light: 0x737C88, dark: 0x7D8590)
    /// The quiet name over a column, the address under a title.
    static let quiet = adaptive(light: 0x3E4651, dark: 0xC3C9D1)

    // ---- surfaces, back to front ----------------------------------------------------------------

    /// The window behind everything, and the sign-in screen.
    static let app = adaptive(light: 0xF3F4F6, dark: 0x0D0F12)
    /// The conversations column.
    static let sidebar = adaptive(light: 0xF7F8FA, dark: 0x111419)
    /// The subjects column.
    static let column = adaptive(light: 0xF4F5F8, dark: 0x13161B)
    /// The recessed ground a conversation sits on.
    static let wash = adaptive(light: 0xFAFBFC, dark: 0x0F1216)
    /// The raised surfaces: a received message, a dialog, a sheet.
    static let ground = adaptive(light: 0xFFFFFF, dark: 0x171B21)
    /// A field to type in, and the switcher's card.
    static let field = adaptive(light: 0xECEEF2, dark: 0x171B21)
    /// The composer's card.
    static let composer = adaptive(light: 0xFFFFFF, dark: 0x151920)
    /// A row the pointer is resting on or that is chosen.
    static let selected = adaptive(light: 0xE6EAF0, dark: 0x1C2129)
    /// The chosen segment of a segmented strip, and a disabled button's fill.
    static let chip = adaptive(light: 0xDFE3EA, dark: 0x252B34)
    /// A small square tile behind an icon.
    static let tile = adaptive(light: 0xE9ECF1, dark: 0x22272F)

    /// The hairline between columns and under a header.
    static let rule = adaptive(light: 0xE3E6EB, dark: 0x1F242B)
    /// The border of a card.
    static let stroke = adaptive(light: 0xDFE3E8, dark: 0x262B33)
    /// The border of a field or a button that is only an outline.
    static let strokeStrong = adaptive(light: 0xD3D8DF, dark: 0x2A3039)

    /// Your own message: the accent's hue at card strength, rather than a solid fill.
    enum Mine {
        static let fill = adaptive(light: 0xEDF3FF, dark: 0x131C2E)
        static let stroke = adaptive(light: 0xC9D8F6, dark: 0x263556)
        static let rule = adaptive(light: 0xD9E4F8, dark: 0x1F2C47)
        static let text = adaptive(light: 0x14213D, dark: 0xE6ECF8)
        static let secondary = adaptive(light: 0x3A4A6B, dark: 0xC5D0E6)
        static let header = adaptive(light: 0x4A5B80, dark: 0xA9B6D3)
        static let meta = adaptive(light: 0x5B6B8E, dark: 0x8C9AB8)
        static let panel = adaptive(light: 0xDDE7FA, dark: 0x1B2740)
        static let panelText = adaptive(light: 0x2F4470, dark: 0xB9C8E8)
    }

    /// Inline and fenced code.
    enum Code {
        static let fill = adaptive(light: 0xF0F2F5, dark: 0x1E232B)
        static let stroke = adaptive(light: 0xDDE1E7, dark: 0x2A3039)
        static let text = adaptive(light: 0x2E353F, dark: 0xD7DCE3)
    }

    /// The toast carries white text in both appearances, so its fill cannot be `ink` — in the dark
    /// that token is nearly white, and white on white is a message nobody reads.
    static let toastFill = adaptive(light: 0x14181F, dark: 0x2A3039)

    /// The layer over the chat doodle, and the one number worth touching. The artwork has no opacity
    /// of its own, so this alpha is its strength: in the dark it is all but the conversation ground
    /// itself, so the pattern is a texture you notice only if you look for it.
    static let doodleVeil = adaptive(light: 0xFAFBFC, dark: 0x0F1216)
        .opacity(doodleVeilAlpha)

    private static let doodleVeilAlpha = 0.9

    /// The six avatar tones, in the order the web app's `data-tone` uses them: navy, blue, teal,
    /// purple, amber, rose. Each is a tinted disc with its initials in the same hue, lighter — the
    /// conversation design's faces — so a peer keeps one colour everywhere, which is the whole point
    /// of the tone hash.
    static let tones: [Color] = [
        adaptive(light: 0xE4E9F2, dark: 0x232A3A),
        adaptive(light: 0xE0EBFD, dark: 0x17263F),
        adaptive(light: 0xDAF2EC, dark: 0x12302D),
        adaptive(light: 0xECE6FD, dark: 0x2A2142),
        adaptive(light: 0xFBEDD3, dark: 0x33280F),
        adaptive(light: 0xFCE4EA, dark: 0x3A1A22),
    ]
    static let toneInk: [Color] = [
        adaptive(light: 0x3A4A6B, dark: 0xA9B8D6),
        adaptive(light: 0x2457B8, dark: 0x8DB4F5),
        adaptive(light: 0x12756A, dark: 0x6FD6C4),
        adaptive(light: 0x6340C0, dark: 0xB9A2FF),
        adaptive(light: 0x985A06, dark: 0xF0B860),
        adaptive(light: 0xAE2A4B, dark: 0xF29AAE),
    ]

    /// The server's decisions, shown as chips, and the card a held request sits in.
    enum Pill {
        static let heldText = adaptive(light: 0xA35F00, dark: 0xF0B860)
        static let heldFill = adaptive(light: 0xFCEFD3, dark: 0x2A2213)
        static let heldStroke = adaptive(light: 0xF4DDAE, dark: 0x45361A)
        static let autoText = adaptive(light: 0x1E7A47, dark: 0x7DD3A0)
        static let autoFill = adaptive(light: 0xE2F4E9, dark: 0x142A1F)
        static let autoStroke = adaptive(light: 0xC4E8D2, dark: 0x1F4030)
        static let blockedText = adaptive(light: 0xB0284B, dark: 0xF29AAE)
        static let blockedFill = adaptive(light: 0xFDE8EE, dark: 0x3A1A22)
        static let blockedStroke = adaptive(light: 0xF6C9D5, dark: 0x5A2633)
    }

    /// A request that is waiting on you: amber, at card strength.
    enum Held {
        static let fill = adaptive(light: 0xFFF8EA, dark: 0x1C1810)
        static let stroke = adaptive(light: 0xF1D9A6, dark: 0x45361A)
        static let title = adaptive(light: 0x7A4A06, dark: 0xF6D9A6)
        static let text = adaptive(light: 0x7D6334, dark: 0xC9B48F)
        static let panel = adaptive(light: 0xFCEFD3, dark: 0x2E2715)
        static let panelText = adaptive(light: 0x7A4A06, dark: 0xE9CF9C)
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
