//  The dialogs: which mailbox to act as, who a sender is, and the two ways to start something new.

import SwiftUI

/// Reading *as* an agent, which is a different act from writing *to* one — and the two are one tap
/// apart, so the app is explicit about which one you are doing.
struct IdentityPickerSheet: View {
    @Environment(Account.self) private var account
    @Environment(\.dismiss) private var dismiss
    let onPick: (Mailbox) -> Void

    var body: some View {
        NavigationStack {
            List {
                if let me = account.me {
                    Section("Acting as") { MailboxRow(mailbox: me) }
                }
                if !account.ownAgents.isEmpty {
                    Section("Your other mailboxes") {
                        ForEach(account.ownAgents) { mailbox in
                            Button {
                                onPick(mailbox)
                                dismiss()
                            } label: {
                                MailboxRow(mailbox: mailbox)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
                Section {
                    Text("Opening a mailbox shows the mail it has received. To write *to* one of your agents, pick it from the conversation list instead.")
                        .font(.system(size: 12.5))
                        .foregroundStyle(Theme.muted)
                }
            }
            .navigationTitle("Mailboxes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } }
            }
        }
        .presentationDetents([.medium, .large])
    }
}

struct MailboxRow: View {
    let mailbox: Mailbox

    var body: some View {
        HStack(spacing: 11) {
            Avatar(peer: mailbox.key)
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Theme.ink)
                Text(sub)
                    .font(.system(size: 12.5))
                    .foregroundStyle(Theme.muted)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .padding(.vertical, 3)
    }

    private var name: String {
        if let handle = mailbox.handle { return PeerFace.displayName(handle) }
        return mailbox.label ?? PeerFace.displayName(mailbox.address)
    }

    /// An unnamed mailbox is worth flagging rather than padded over: handle-based trust will never
    /// match one, so a fleet that expects to be trusted by name needs to know.
    private var sub: String { mailbox.handle ?? "\(mailbox.address) · no handle" }
}

/// Writing to somebody who has never written in. A handle, a whole namespace, or a key address —
/// the server resolves which mailbox that is, and the conversation is filed under what it says.
struct NewConversationSheet: View {
    @Environment(Inbox.self) private var inbox
    @Environment(\.dismiss) private var dismiss
    let onStarted: (String) -> Void

    @State private var peer = ""
    @State private var body_ = ""
    @State private var error: String?
    @State private var sending = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("/bekir/agent1 or /k/…", text: $peer)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.system(size: 15, design: .monospaced))
                } header: {
                    Text("Their address")
                } footer: {
                    Text("A handle like /bekir/agent1, a whole namespace like /bekir, or a key address.")
                }

                Section("First message") {
                    TextField("Write a message", text: $body_, axis: .vertical)
                        .lineLimit(3...8)
                }

                if let error {
                    Text(error)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.Pill.blockedText)
                }
            }
            .navigationTitle("New conversation")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Send") { send() }
                        .disabled(sending || peer.trimmed.isEmpty || body_.trimmed.isEmpty)
                }
            }
        }
    }

    private func send() {
        sending = true
        error = nil
        Task {
            do {
                let started = try await inbox.startConversation(to: peer.trimmed, body: body_.trimmed)
                onStarted(started)
                dismiss()
            } catch let failure as APIError {
                error = failure.sendFailureMessage
            } catch {
                self.error = "Could not reach the postbox."
            }
            sending = false
        }
    }
}

/// A thread keeps one subject apart from the rest, so an old request does not colour a new one.
/// Both sides see the same name.
struct NewThreadSheet: View {
    let peer: String
    let onOpened: (String) -> Void

    @Environment(Inbox.self) private var inbox
    @Environment(\.dismiss) private var dismiss

    @State private var title = ""
    @State private var error: String?
    @State private var working = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("the deploy, the failing tests, next week's release", text: $title)
                } header: {
                    Text("What is it about?")
                } footer: {
                    Text("A thread keeps one subject apart from the rest. Both sides see the same name.")
                }
                if let error {
                    Text(error)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.Pill.blockedText)
                }
            }
            .navigationTitle("New thread")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Create") { create() }
                        .disabled(working || title.trimmed.isEmpty)
                }
            }
        }
        .presentationDetents([.medium])
    }

    private func create() {
        working = true
        error = nil
        Task {
            do {
                let id = try await inbox.openThread(with: peer, title: title.trimmed)
                onOpened(id)
                dismiss()
            } catch let failure as APIError {
                error = failure.errorDescription
            } catch {
                self.error = "Could not open the thread."
            }
            working = false
        }
    }
}

