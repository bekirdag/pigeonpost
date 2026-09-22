//  Start a conversation with an address, and say something in the same breath.
//
//  The shared typing guard catches incomplete addresses; the postbox resolves the destination.

import SwiftUI

struct MacNewConversationSheet: View {
    let opened: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @Environment(Inbox.self) private var inbox
    @State private var peer = "/"
    @State private var draft = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("New conversation")
                .font(.system(size: 15, weight: .semibold))
            TextField("/bekir/agent1 or /k/…", text: Binding(get: { peer }, set: { peer = PeerFace.conversationAddressInput($0) }))
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 13, design: .monospaced))
                .frame(width: 340)
                .onSubmit(go)
            // Addressing somebody and writing to them are one intention. Without this field the
            // sheet could only open an empty conversation, so the shortest way to send a first
            // message was to type the address, dismiss, find the conversation and start again.
            // Styled as the thread composer is, because it is the same act.
            TextField("Write a message", text: $draft, axis: .vertical)
                .textFieldStyle(.plain)
                .font(.system(size: 13))
                .lineLimit(3...8)
                .frame(width: 340, alignment: .leading)
                .padding(.horizontal, 10)
                .padding(.vertical, 7)
                .background(Theme.ground, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.rule, lineWidth: 1))
            Text("An address is a handle or a /k/ key address. An empty message just opens the conversation.")
                .font(.system(size: 11.5))
                .foregroundStyle(Theme.muted)
                .frame(width: 340, alignment: .leading)
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                // The button says which of the two things will happen, so nothing is sent by
                // surprise and nothing written is silently dropped.
                Button(writing ? "Send" : "Open") { go() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!PeerFace.validConversationAddress(peer))
            }
        }
        .padding(20)
    }

    private var writing: Bool { !draft.trimmed.isEmpty }

    private func go() {
        let trimmed = peer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard PeerFace.validConversationAddress(trimmed) else { return }
        let text = draft.trimmed
        // Open first. The send puts an optimistic row in this conversation immediately, and that
        // row should appear in a conversation already on screen rather than behind the sheet.
        opened(trimmed)
        if !text.isEmpty {
            Task { await inbox.send(text, to: trimmed, threadId: nil) }
        }
        dismiss()
    }
}
