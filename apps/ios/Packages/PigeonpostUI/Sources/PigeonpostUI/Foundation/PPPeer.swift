//  How an address is shown: its name, its initials and the tone it keeps. The rules and the tone
//  hash are the shipped apps' (web names.js, iOS PeerFace), so one peer has the same face in the
//  browser, on the phone and in anything built with this kit.

import SwiftUI

/// Avatar and logo tones (`.pp-initials[data-tone]`).
public enum PPTone: String, CaseIterable, Sendable {
    case blue, green, navy, violet, amber, teal, red, gray

    /// The disc colour.
    public var fill: Color {
        switch self {
        case .blue: return PP.Colors.accentSubtle
        case .green: return PP.Colors.successFill
        case .navy: return PP.Colors.brand
        case .violet: return .ppDynamic(light: 0xEBE7FC, dark: 0x2F2E4A)
        case .amber: return PP.Colors.warningFill
        case .teal: return .ppDynamic(light: 0xD9F4F0, dark: 0x1D3B3E)
        case .red: return PP.Colors.dangerFill
        case .gray: return PP.Colors.surfaceSunken
        }
    }

    /// The initials / icon colour on that disc.
    public var ink: Color {
        switch self {
        case .blue: return PP.Colors.textLink
        case .green: return PP.Colors.success
        case .navy: return PP.Colors.surface
        case .violet: return .ppDynamic(light: 0x422C84, dark: 0x866EC5)
        case .amber: return PP.Colors.warning
        case .teal: return .ppDynamic(light: 0x0E5049, dark: 0x468880)
        case .red: return PP.Colors.danger
        case .gray: return PP.Colors.textMuted
        }
    }

    /// Solid colour for listing logos (`.pp-listing-logo--*`), white ink.
    public var solid: Color {
        switch self {
        case .blue: return PP.Palette.blue600
        case .green: return PP.Palette.green700
        case .navy: return PP.Palette.navy700
        case .violet: return PP.Palette.violet600
        case .amber: return PP.Palette.amber700
        case .teal: return PP.Palette.teal600
        case .red: return PP.Palette.red700
        case .gray: return PP.Palette.gray600
        }
    }

    /// The tone a peer keeps: tone index 1…6 maps to navy, blue, teal, violet, amber, red, the
    /// order the web app's `data-tone` numbers use.
    public static func forPeer(_ peer: String?) -> PPTone {
        [.navy, .blue, .teal, .violet, .amber, .red][PPPeer.toneIndex(peer) - 1]
    }
}

/// Name, initials and tone for a Pigeonpost address (`/bekir`, `/wodo/hr`, `/k/9mjp…`).
public enum PPPeer {
    /// Short segments that read as acronyms: "/wodo/hr" is "Wodo HR", not "Wodo Hr".
    public static let acronyms: Set<String> = ["hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev"]

    /// A handle as a name: "/bekir/main" and "/bekir" are "Bekir", "/wodo/hr" is "Wodo HR",
    /// "/acme/john-doe" is "Acme John Doe". Key addresses are shortened; GitHub logins and
    /// address-style handles keep their display; anything else is returned unchanged.
    public static func displayName(_ peer: String?) -> String {
        guard let peer, !peer.isEmpty else { return "unknown" }
        if peer.hasPrefix("/k/") { return String(peer.prefix(12)) + "…" }
        var parts = peer.split(separator: "/").filter { !$0.isEmpty }.map(String.init)
        guard peer.hasPrefix("/"), !parts.isEmpty else { return peer }
        if parts.count > 1, parts.last == "main" { parts.removeLast() }
        if peer.contains("@") || parts.first == "github" { return "/" + parts.joined(separator: "/") }
        let name = parts.map { part in
            part.split(whereSeparator: { $0 == "-" || $0 == "_" }).map { word -> String in
                let lower = word.lowercased()
                if acronyms.contains(lower) { return lower.uppercased() }
                return lower.prefix(1).uppercased() + lower.dropFirst()
            }.joined(separator: " ")
        }.filter { !$0.isEmpty }.joined(separator: " ")
        return name.isEmpty ? peer : name
    }

    /// Two letters from the shown name: the first and last words, or the first two letters of a
    /// single word. An alias (`name`) wins over the handle.
    public static func initials(_ peer: String?, name: String? = nil) -> String {
        let shown = (name?.isEmpty == false ? name! : displayName(peer)).trimmingCharacters(in: .whitespaces)
        let base = shown.contains("/") ? (shown.split(separator: "/").last.map(String.init) ?? shown) : shown
        let words = base.split(whereSeparator: { $0.isWhitespace || $0 == "-" || $0 == "_" })
            .map { $0.filter { $0.isLetter || $0.isNumber } }
            .filter { !$0.isEmpty }
        guard let first = words.first else { return "··" }
        let letters = words.count > 1 ? String(first.prefix(1)) + String(words.last!.prefix(1)) : String(first.prefix(2))
        return letters.uppercased()
    }

    /// Tone 1…6: `h * 31 + charCode` over UTF-16 units, wrapping at 32 bits like JavaScript's `>>> 0`.
    public static func toneIndex(_ peer: String?) -> Int {
        guard let peer else { return 1 }
        var hash: UInt32 = 0
        for unit in peer.utf16 {
            hash = hash &* 31 &+ UInt32(unit)
        }
        return Int(hash % 6) + 1
    }
}
