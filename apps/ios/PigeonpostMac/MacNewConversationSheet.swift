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
    @State private var error: String?
    @State private var sending = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("New conversation")
                .font(.geist(17, .semibold))
                .foregroundStyle(Theme.ink)
            TextField("/bekir/agent1 or /k/…", text: Binding(get: { peer }, set: { peer = PeerFace.conversationAddressInput($0) }))
                .textFieldStyle(.plain)
                .font(.geistMono(13.5))
                .padding(.horizontal, 12)
                .frame(width: 340, height: 40)
                .background(Theme.wash, in: RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.strokeStrong, lineWidth: 1))
                .onSubmit(go)
            // Addressing somebody and writing to them are one intention. Without this field the
            // sheet could only open an empty conversation, so the shortest way to send a first
            // message was to type the address, dismiss, find the conversation and start again.
            // Styled as the thread composer is, because it is the same act.
            TextField("Write a message", text: $draft, axis: .vertical)
                .textFieldStyle(.plain)
                .font(.geist(13))
                .lineLimit(3...8)
                .frame(width: 316, alignment: .leading)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(Theme.wash, in: RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.strokeStrong, lineWidth: 1))
            Text("An address is a handle or a /k/ key address. An empty message just opens the conversation.")
                .font(.geist(11.5))
                .foregroundStyle(Theme.muted)
                .frame(width: 340, alignment: .leading)
            if let error {
                Text(error)
                    .font(.geist(12.5))
                    .foregroundStyle(Theme.Pill.blockedText)
                    .frame(width: 340, alignment: .leading)
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                // The button says which of the two things will happen, so nothing is sent by
                // surprise and nothing written is silently dropped.
                Button(sending ? "Sending…" : writing ? "Send" : "Open") { go() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(sending || !PeerFace.validConversationAddress(peer))
            }
        }
        .padding(24)
        .background(Theme.ground)
    }

    private var writing: Bool { !draft.trimmed.isEmpty }

    private func go() {
        let trimmed = peer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard PeerFace.validConversationAddress(trimmed), !sending else { return }
        let text = draft.trimmed
        guard !text.isEmpty else {
            opened(trimmed)
            dismiss()
            return
        }
        // The conversation opens from `inbox.started` in the window reading the mailbox that sent
        // it, once the postbox has answered — including an answer that never came, which opens it
        // with the message marked not sent and its words back in the composer. A refusal stays
        // here, with the words, rather than leaving an empty conversation for a wrong address.
        sending = true
        error = nil
        Task {
            switch await inbox.startConversation(to: trimmed, body: text) {
            case .opened: dismiss()
            case .failed(let message): error = message ?? "Could not send."
            }
            sending = false
        }
    }
}
