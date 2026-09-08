//  Start a conversation with an address.
//
//  The grammar check is the shared one, so a name this refuses is a name the postbox would refuse
//  too — showing a green tick and then an error after the send is the failure worth avoiding.

import SwiftUI

struct MacNewConversationSheet: View {
    let opened: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @Environment(Inbox.self) private var inbox
    @State private var peer = ""
    @State private var message = ""
    @State private var working = false
    @State private var error: String?
    @FocusState private var focused: Field?

    private enum Field { case peer, message }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("New conversation")
                .font(.system(size: 15, weight: .semibold))
            TextField("/bekir/agent1 or /k/…", text: $peer)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 13, design: .monospaced))
                .frame(width: 340)
                .focused($focused, equals: .peer)
                .onSubmit { focused = .message }
            TextField("First message", text: $message, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 13))
                .lineLimit(3...8)
                .frame(width: 340)
                .focused($focused, equals: .message)
            Text("The first message creates the conversation and appears in the inbox after it is sent.")
                .font(.system(size: 11.5))
                .foregroundStyle(Theme.muted)
            if let error {
                Text(error)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Theme.Pill.blockedText)
                    .frame(width: 340, alignment: .leading)
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(working)
                Button(working ? "Sending…" : "Send") { open() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!sendable || working)
            }
        }
        .padding(20)
        .onAppear { focused = .peer }
    }

    private var sendable: Bool {
        !peer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func open() {
        let trimmed = peer.trimmingCharacters(in: .whitespacesAndNewlines)
        let body = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !body.isEmpty, !working else { return }
        working = true
        error = nil
        Task {
            defer { working = false }
            do {
                let started = try await inbox.startConversation(to: trimmed, body: body)
                opened(started)
                dismiss()
            } catch let apiError as APIError {
                error = apiError.errorDescription ?? "Could not start that conversation."
            } catch {
                self.error = "Could not start that conversation."
            }
        }
    }
}
