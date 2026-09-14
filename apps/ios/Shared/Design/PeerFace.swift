//  How an address is shown: its name, its initials, and the colour it keeps.
//
//  The tone hash is the web app's, character for character, so one peer has the same face in the
//  browser and on the phone. That is worth more than any improvement to the hash would be.

import SwiftUI

enum PeerFace {
    /// Default inboxes keep their namespace so /bekir/main and /alp/main remain distinguishable.
    static func displayName(_ peer: String?) -> String {
        guard let peer, !peer.isEmpty else { return "unknown" }
        if peer.hasPrefix("/k/") { return String(peer.prefix(12)) + "…" }
        let parts = peer.split(separator: "/").filter { !$0.isEmpty }
        if peer.hasPrefix("/"), parts.count > 1, parts.last == "main" {
            return "/" + parts.dropLast().joined(separator: "/")
        }
        return parts.count > 1 ? String(parts[parts.count - 1]) : peer
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

    static func initials(_ peer: String?) -> String {
        let name = displayName(peer).filter { $0.isLetter || $0.isNumber }
        return name.isEmpty ? "··" : String(name.prefix(2)).uppercased()
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
