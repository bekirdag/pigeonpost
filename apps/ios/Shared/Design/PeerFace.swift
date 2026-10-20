//  How an address is shown: its name, its initials, and the colour it keeps.
//
//  The tone hash is the web app's, character for character, so one peer has the same face in the
//  browser and on the phone. That is worth more than any improvement to the hash would be.

import SwiftUI

enum PeerFace {
    /// Short segments that read as acronyms: "/wodo/hr" is "Wodo HR", not "Wodo Hr". The same list
    /// as `ACRONYMS` in site-inbox/names.js and the Android, Windows and Linux copies.
    static let acronyms: Set<String> = ["hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev"]

    /// A handle as a person's name: "/bekir/main" and "/bekir" are "Bekir", "/wodo/hr" is
    /// "Wodo HR", "/acme/john-doe" is "Acme John Doe". Every segment survives — "/wodo/home" and
    /// "/bekir/home" are different mailboxes — and only the `main` that a bare `/<namespace>`
    /// already resolves to is dropped. Key addresses, address-style handles and GitHub logins keep
    /// their old display; anything that is not a handle is returned as it is. A contact alias wins
    /// over this, and callers check for one first.
    static func displayName(_ peer: String?) -> String {
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

    static func conversationAddressInput(_ input: String) -> String {
        let value = input.trimmingCharacters(in: .whitespacesAndNewlines)
        return value.hasPrefix("/") ? value : "/" + value
    }

    static func validConversationAddress(_ value: String) -> Bool {
        guard (2...512).contains(value.utf8.count), value.hasPrefix("/"),
              value.utf8.allSatisfy({ $0 < 128 }) else { return false }
        // A typing guard, including handles containing @. Complete grammar/routing stays on the server.
        return value.dropFirst().split(separator: "/", omittingEmptySubsequences: false).allSatisfy { part in
            !part.isEmpty && part != "." && part != ".." && part != "*"
                && (!part.contains("*") || part.contains("@"))
                && part.allSatisfy { $0.isLetter || $0.isNumber || "!$&'*+-=^_`{|}~.@".contains($0) }
        }
    }

    /// Two letters for a face, taken from the name it shows: "Bekir" is "BE", "Wodo HR" is "WH" (the
    /// first and last words). Pass `name` when the person has an alias; otherwise the handle's
    /// derived name is used. A name that is still a path (a key or an address) is lettered from its
    /// last segment.
    static func initials(_ peer: String?, name: String? = nil) -> String {
        let shown = (name?.isEmpty == false ? name! : displayName(peer)).trimmingCharacters(in: .whitespaces)
        let base = shown.contains("/") ? (shown.split(separator: "/").last.map(String.init) ?? shown) : shown
        let words = base.split(whereSeparator: { $0.isWhitespace || $0 == "-" || $0 == "_" })
            .map { $0.filter { $0.isLetter || $0.isNumber } }
            .filter { !$0.isEmpty }
        guard let first = words.first else { return "··" }
        let letters = words.count > 1 ? String(first.prefix(1)) + String(words.last!.prefix(1)) : String(first.prefix(2))
        return letters.uppercased()
    }

    /// Which of the six tones a peer keeps, 1...6 — the numbering the web app's `data-tone` uses,
    /// so the two can be compared directly. The hash is `h * 31 + charCode` over UTF-16 units,
    /// wrapping at 32 bits exactly as JavaScript's `>>> 0` does.
    static func toneIndex(_ peer: String?) -> Int {
        guard let peer else { return 1 }
        var hash: UInt32 = 0
        for unit in peer.utf16 {
            hash = hash &* 31 &+ UInt32(unit)
        }
        return Int(hash % 6) + 1
    }

    /// Stable per-peer colour, so a thread keeps its face between sessions and between clients.
    static func tone(_ peer: String?) -> Color {
        Theme.tones[toneIndex(peer) - 1]
    }

    /// The initials' colour on that tone: the same hue, lighter in the dark and deeper in the light.
    static func toneInk(_ peer: String?) -> Color {
        Theme.toneInk[toneIndex(peer) - 1]
    }
}

/// Where `Avatar` finds a profile picture: the app's `AvatarStore`, put in the environment at the
/// root. Absent — a test, a preview, a postbox without pictures — the face is the initials, which is
/// also what shows underneath while a picture loads and whenever there is none.
struct AvatarLookup {
    let image: @MainActor (String) -> Image?
}

private struct AvatarLookupKey: EnvironmentKey {
    static let defaultValue: AvatarLookup? = nil
}

extension EnvironmentValues {
    var avatarLookup: AvatarLookup? {
        get { self[AvatarLookupKey.self] }
        set { self[AvatarLookupKey.self] = newValue }
    }
}

struct Avatar: View {
    let peer: String?
    var size: CGFloat = 38
    /// The name shown beside the face, when it is an alias rather than the handle's own name.
    var name: String? = nil
    @Environment(\.avatarLookup) private var lookup

    var body: some View {
        Circle()
            .fill(PeerFace.tone(peer))
            .frame(width: size, height: size)
            .overlay(
                Text(PeerFace.initials(peer, name: name))
                    .font(.geist(size * 0.32, .bold))
                    .kerning(size * 0.32 * 0.02)
                    .foregroundStyle(PeerFace.toneInk(peer))
            )
            .overlay {
                if let peer, let picture = lookup?.image(peer) {
                    picture
                        .resizable()
                        .scaledToFill()
                        .frame(width: size, height: size)
                        .clipShape(Circle())
                }
            }
            .accessibilityHidden(true)
    }
}
