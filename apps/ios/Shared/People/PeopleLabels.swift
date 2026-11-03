import SwiftUI

/// Shared labels observe profile/local changes without changing the message/permission model.
struct ContactTitle: View {
    let peer: String
    let fallback: String
    var legacyName: String? = nil
    @Environment(PeopleStore.self) private var people: PeopleStore?
    var body: some View {
        let name = people?.displayName(peer, legacyName: legacyName) ?? fallback
        if name.hasPrefix("~") { Text(name).foregroundStyle(.secondary) }
        else { Text(name.hasPrefix("/") ? PeerFace.displayName(name) : name) }
    }
}

extension Conversation {
    var peopleLegacyName: String? { mine ? name : contact?.alias }
}
