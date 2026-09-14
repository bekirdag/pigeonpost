//  Start a conversation with an address.
//
//  The shared typing guard catches incomplete addresses; the postbox resolves the destination.

import SwiftUI

struct MacNewConversationSheet: View {
    let opened: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @Environment(Inbox.self) private var inbox
    @State private var peer = "/"

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("New conversation")
                .font(.system(size: 15, weight: .semibold))
            TextField("/bekir/agent1 or /k/…", text: Binding(get: { peer }, set: { peer = PeerFace.conversationAddressInput($0) }))
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 13, design: .monospaced))
                .frame(width: 340)
                .onSubmit(open)
            Text("An address is a handle or a /k/ key address. Nothing is sent until you write one.")
                .font(.system(size: 11.5))
                .foregroundStyle(Theme.muted)
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Open") { open() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!PeerFace.validConversationAddress(peer))
            }
        }
        .padding(20)
    }

    private func open() {
        let trimmed = peer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard PeerFace.validConversationAddress(trimmed) else { return }
        opened(trimmed)
        dismiss()
    }
}
