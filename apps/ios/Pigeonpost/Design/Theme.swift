//  The palette, from site/style.css by way of site-inbox/app.css — so this reads as the same
//  product as the web app and the site. White ground, navy accent, the same rule and wash greys.
//
//  Light only for now, deliberately: the web app declares `color-scheme: light` and matching it is
//  the point. A phone is not a browser and will want a dark palette; that is a phase of its own,
//  and doing it by halves now would leave half the surfaces white in the dark.

import SwiftUI

enum Theme {
    static let navy = Color(hex: 0x16326B)
    static let blue = Color(hex: 0x2563EB)
    static let green = Color(hex: 0x22C55E)
    static let amber = Color(hex: 0xB45309)
    static let ink = Color(hex: 0x14181F)
    static let body = Color(hex: 0x444C58)
    static let muted = Color(hex: 0x6B7480)
    static let rule = Color(hex: 0xE6E9EE)
    static let wash = Color(hex: 0xF7F9FB)
    static let ground = Color.white

    /// The flat layer laid over the chat doodle, and the one number worth touching. The artwork is
    /// near-black strokes on transparency and has no opacity of its own, so this alpha is its
    /// strength: 0.94 hid it entirely, 0.80 reads as a faint pattern you notice only if you look for
    /// it. Slightly blue-grey rather than the flat page colour, so the strokes read as ink on paper
    /// rather than dirt on white.
    static let doodleVeil = Color(hex: 0xF0F4F9).opacity(0.80)

    /// The six avatar tones, in the order the web app's `data-tone` uses them.
    static let tones: [Color] = [
        Color(hex: 0x16326B),
        Color(hex: 0x2563EB),
        Color(hex: 0x0F766E),
        Color(hex: 0x7C3AED),
        Color(hex: 0xB45309),
        Color(hex: 0xBE123C),
    ]

    enum Pill {
        static let heldText = Color(hex: 0xB45309)
        static let heldFill = Color(hex: 0xFFF8EE)
        static let heldStroke = Color(hex: 0xFCD9A4)
        static let autoText = Color(hex: 0x15803D)
        static let autoFill = Color(hex: 0xF0FDF4)
        static let autoStroke = Color(hex: 0xBBF7D0)
        static let blockedText = Color(hex: 0x9F1239)
        static let blockedFill = Color(hex: 0xFFF1F2)
        static let blockedStroke = Color(hex: 0xFECDD3)
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
