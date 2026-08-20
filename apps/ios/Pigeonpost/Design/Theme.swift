//  The palette, from site/style.css by way of site-inbox/app.css — so this reads as the same
//  product as the web app and the site. White ground, navy accent, the same rule and wash greys.
//
//  Every token is adaptive. The web app declares `color-scheme: light` and can stop there; a phone
//  cannot. iOS turns the surfaces it owns — the list, the sheets, the navigation bar — dark on its
//  own, and a palette of fixed light values then paints near-black text onto them. That is not a
//  theme that "does not support dark mode"; it is an unreadable app, which is what shipped in
//  1.0 (1).
//
//  The dark values are the light ones re-derived rather than inverted: the greys keep their order
//  and their spacing, and the navy lifts to a blue that survives on a dark ground instead of
//  disappearing into it.

import SwiftUI
#if canImport(UIKit)
import UIKit
#endif

enum Theme {
    /// Brand and accent. Navy is the app's own colour, the outgoing bubble, the selected row and the
    /// tint on every control; at #16326B it is nearly black against a dark ground, so it lifts.
    static let navy = adaptive(light: 0x16326B, dark: 0x4A73D6)
    static let blue = adaptive(light: 0x2563EB, dark: 0x6C9BFF)
    static let green = adaptive(light: 0x22C55E, dark: 0x34D06A)
    static let amber = adaptive(light: 0xB45309, dark: 0xE9A23B)

    /// Text, in the order the web app uses it: ink for a name, body for what was said, muted for
    /// when it was said.
    static let ink = adaptive(light: 0x14181F, dark: 0xF4F6F9)
    static let body = adaptive(light: 0x444C58, dark: 0xC6CCD6)
    static let muted = adaptive(light: 0x6B7480, dark: 0x949BA6)

    static let rule = adaptive(light: 0xE6E9EE, dark: 0x2C313A)
    /// The recessed ground a conversation sits on.
    static let wash = adaptive(light: 0xF7F9FB, dark: 0x0B0D11)
    /// The raised surfaces: a bubble, the composer, the subject chips, the navigation bar.
    static let ground = adaptive(light: 0xFFFFFF, dark: 0x16191F)

    /// The toast carries white text in both appearances, so its fill cannot be `ink` — in the dark
    /// that token is nearly white, and white on white is a message nobody reads.
    static let toastFill = adaptive(light: 0x14181F, dark: 0x2E333B)

    /// The layer over the chat doodle, and the one number worth touching. The artwork has no opacity
    /// of its own, so this alpha is its strength: 0.94 hid it entirely, 0.80 reads as a faint
    /// pattern you notice only if you look for it. Slightly off the flat page colour either way, so
    /// the strokes read as ink on paper rather than dirt on white.
    static let doodleVeil = adaptive(light: 0xF0F4F9, dark: 0x0B0D11)
        .opacity(doodleVeilAlpha)

    /// Stronger in the dark, where the inverted strokes are brighter against their ground than the
    /// original strokes ever are against paper.
    private static let doodleVeilAlpha = 0.82

    /// The six avatar tones, in the order the web app's `data-tone` uses them. Saturated enough to
    /// carry white initials on either ground, so they do not change between appearances — a peer
    /// keeps one face everywhere, which is the whole point of the tone hash.
    static let tones: [Color] = [
        Color(hex: 0x16326B),
        Color(hex: 0x2563EB),
        Color(hex: 0x0F766E),
        Color(hex: 0x7C3AED),
        Color(hex: 0xB45309),
        Color(hex: 0xBE123C),
    ]

    /// The server's decisions, shown as chips. Light fills in the light appearance, dark fills in
    /// the dark one — a pale chip on a dark ground glares.
    enum Pill {
        static let heldText = adaptive(light: 0xB45309, dark: 0xE9A23B)
        static let heldFill = adaptive(light: 0xFFF8EE, dark: 0x2A2318)
        static let heldStroke = adaptive(light: 0xFCD9A4, dark: 0x4A3A1E)
        static let autoText = adaptive(light: 0x15803D, dark: 0x4ADE80)
        static let autoFill = adaptive(light: 0xF0FDF4, dark: 0x16281C)
        static let autoStroke = adaptive(light: 0xBBF7D0, dark: 0x26492F)
        static let blockedText = adaptive(light: 0x9F1239, dark: 0xFDA4AF)
        static let blockedFill = adaptive(light: 0xFFF1F2, dark: 0x2A171B)
        static let blockedStroke = adaptive(light: 0xFECDD3, dark: 0x4C2028)
    }

    /// One token, two values, resolved by the trait collection at draw time rather than read once at
    /// launch — so the app follows a change of appearance without being restarted.
    ///
    /// The `#else` is for the thread-model test, which compiles this file for the mac it runs on and
    /// never draws anything. It is not a macOS port.
    private static func adaptive(light: UInt32, dark: UInt32) -> Color {
        #if canImport(UIKit)
        return Color(uiColor: UIColor { traits in
            UIColor(rgb: traits.userInterfaceStyle == .dark ? dark : light)
        })
        #else
        return Color(hex: light)
        #endif
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
#endif
