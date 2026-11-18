//  The desktop surface ladder: the kit's surface roles, re-stepped for a window of three columns in
//  dark mode.
//
//  Hand-written, unlike `Generated/PPTokens.swift`. The kit's dark surfaces (`bg` #0B0E12,
//  `bgSubtle` #0F1217, `surfaceSunken` #10141A, `surface` #141820) sit within 1.1:1 of one another.
//  On a phone only one of them is on screen at a time and that is fine; a desktop window puts the
//  sidebar, the subjects column, the conversation and its cards side by side, and at those steps
//  they read as one black sheet (owner, 2026-11-18: "too dark … the user can't distinguish the
//  backgrounds"). So the desktop gets its own steps — window, column, sidebar, raised — about 1.1–1.3:1
//  apart, with a visible 1pt rule between them.
//
//  Light mode is untouched: every `light:` value below is the kit token the role used before.
//  Text stays the kit's except `textFaint`, lifted just enough to keep 4.5:1 (WCAG AA) on the
//  lighter selected row and raised cards it now sits on.
//
//  Only the Mac uses these (see `Theme` in the app). An iPad's desktop layout keeps the kit's
//  phone-dark surfaces, as the rest of iPadOS does.

import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

extension PP {
    public enum Desktop {
        // ---- surfaces, back to front ---------------------------------------------------------

        /// The window, and the conversation's ground — the deepest step. Kit: `bg` / `bgSubtle`.
        public static let canvas = desktopColor(light: 0xFAFBFC, dark: 0x0F1115)
        /// Behind everything a sheet or a settings page puts on the canvas. Kit: `bg`.
        public static let window = desktopColor(light: 0xF4F6F8, dark: 0x0F1115)
        /// The subjects column. Kit: `surfaceSunken`.
        public static let column = desktopColor(light: 0xF4F6F8, dark: 0x171A21)
        /// The window's toolbar: in the dark, the column's step, so the bar is told apart from the
        /// conversation beneath it. Kit: `bgSubtle` (the conversation's own ground, in the light).
        public static let toolbar = desktopColor(light: 0xFAFBFC, dark: 0x171A21)
        /// The conversations sidebar. Kit: `bgSubtle`.
        public static let sidebar = desktopColor(light: 0xFAFBFC, dark: 0x1E222B)
        /// Cards, a received message, the composer, a sheet. Kit: `surface`.
        public static let raised = desktopColor(light: 0xFFFFFF, dark: 0x262B36)
        /// A field to type in — recessed below whatever column it sits in. Kit: `surfaceSunken`.
        public static let sunken = desktopColor(light: 0xF4F6F8, dark: 0x13161C)
        /// The row that is chosen. Kit: `surfaceSelected`.
        public static let selected = desktopColor(light: 0xE4EBF7, dark: 0x24365A)
        /// A row under the pointer, an icon's tile. Kit: `surfaceHover`.
        public static let hover = desktopColor(light: 0xEEF1F5, dark: 0x2A2F3B)
        /// The chosen segment of a segmented strip. The app's own `chip`.
        public static let chip = desktopColor(light: 0xDFE3E9, dark: 0x2A2F3B)

        // ---- lines ----------------------------------------------------------------------------

        /// The hairline between columns and under a header. Kit: `borderSubtle`.
        public static let rule = desktopColor(light: 0xECEEF2, dark: 0x2E3440)
        /// The border of a card. Kit: `border`.
        public static let border = desktopColor(light: 0xE1E5EA, dark: 0x363D4B)
        /// The border of a field or an outline button. Kit: `borderStrong`.
        public static let borderStrong = desktopColor(light: 0xC9CFD8, dark: 0x4A5262)

        // ---- text -----------------------------------------------------------------------------

        /// Times and captions. Kit: `textFaint`, lifted in the dark to 4.7:1 on `selected` and
        /// 5.6:1 on `raised`.
        public static let textFaint = desktopColor(light: 0x636C78, dark: 0x9AA3B0)
    }
}

/// One role, two values, chosen at draw time — the same shape as the generated tokens.
private func desktopColor(light: UInt32, dark: UInt32) -> Color {
    func rgb(_ hex: UInt32) -> (CGFloat, CGFloat, CGFloat) {
        (CGFloat((hex >> 16) & 0xFF) / 255, CGFloat((hex >> 8) & 0xFF) / 255, CGFloat(hex & 0xFF) / 255)
    }
    #if canImport(UIKit)
    return Color(UIColor { traits in
        let (r, g, b) = rgb(traits.userInterfaceStyle == .dark ? dark : light)
        return UIColor(red: r, green: g, blue: b, alpha: 1)
    })
    #elseif canImport(AppKit)
    return Color(NSColor(name: nil) { appearance in
        let (r, g, b) = rgb(appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light)
        return NSColor(srgbRed: r, green: g, blue: b, alpha: 1)
    })
    #else
    return Color(.sRGB, red: Double((light >> 16) & 0xFF) / 255, green: Double((light >> 8) & 0xFF) / 255,
                 blue: Double(light & 0xFF) / 255, opacity: 1)
    #endif
}
