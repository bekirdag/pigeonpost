//  How an address is shown: its name, its initials, and the colour it keeps.
//
//  The tone hash is the web app's, character for character, so one peer has the same face in the
//  browser and on the phone. That is worth more than any improvement to the hash would be.

import SwiftUI

enum PeerFace {
    /// The whole handle, with a trailing `/main` dropped.
    ///
    /// Showing the last segment alone is what made a conversation with `/ozgur/main` read as
    /// "main" — and every other namespace's default inbox reads as "main" too, so the one name on
    /// screen was the one word that identified nobody. The rest of a handle is not decoration
    /// either: `/wodo/home` and `/bekir/home` are different mailboxes, and a row saying only "home"
    /// does not say whose. So the namespace always survives; only the `main` that a bare
    /// `/<namespace>` already resolves to is worth dropping.
    static func displayName(_ peer: String?) -> String {
        guard let peer, !peer.isEmpty else { return "unknown" }
        if peer.hasPrefix("/k/") { return String(peer.prefix(12)) + "…" }
        let parts = peer.split(separator: "/").filter { !$0.isEmpty }
        guard !parts.isEmpty else { return peer }
        if parts.count > 1, parts.last == "main" {
            return "/" + parts.dropLast().joined(separator: "/")
        }
        return "/" + parts.joined(separator: "/")
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

    /// Letters come from the last segment of the name, not from the whole of it. The badge is two
    /// characters wide, and now that the name carries its namespace, taking them off the front
    /// would give every mailbox in one fleet the same face.
    static func initials(_ peer: String?) -> String {
        let name = displayName(peer)
        let last = name.split(separator: "/").last.map(String.init) ?? name
        let letters = last.filter { $0.isLetter || $0.isNumber }
        return letters.isEmpty ? "··" : String(letters.prefix(2)).uppercased()
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
}

struct Avatar: View {
    let peer: String?
    var size: CGFloat = 38

    var body: some View {
        Circle()
            .fill(PeerFace.tone(peer))
            .frame(width: size, height: size)
            .overlay(
                Text(PeerFace.initials(peer))
                    .font(.system(size: size * 0.355, weight: .semibold))
                    .foregroundStyle(.white)
            )
            .accessibilityHidden(true)
    }
}
