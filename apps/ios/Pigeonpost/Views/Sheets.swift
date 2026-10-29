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
                Section("Your mailboxes") {
                    ForEach(MailboxOrder.sorted(account.mailboxes, primaryNamespace: Config.primaryNamespace,
                                               username: account.session.username)) { mailbox in
                        HStack {
                            Button {
                                onPick(mailbox)
                                dismiss()
                            } label: {
                                MailboxRow(mailbox: mailbox)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.borderless)
                            .accessibilityIdentifier("mailbox:" + mailbox.key)
                            .accessibilityValue(mailbox.address == account.me?.address ? "Selected" : "")
                            if mailbox.address == account.me?.address {
                                Image(systemName: "checkmark").foregroundStyle(Theme.ink)
                                    .accessibilityHidden(true)
                            }
                            CopyAddressButton(address: mailbox.key)
                        }
                    }
                }
                Section {
                    Text("Opening a mailbox shows the mail it has received. To write *to* one of your agents, pick it from the conversation list instead.")
                        .font(.geist(12.5))
                        .foregroundStyle(Theme.muted)
                }
            }
            .sheetSurface()
            .navigationTitle("Mailboxes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } }
            }
        }
        .phoneDetents([.medium, .large])
    }
}

struct MailboxRow: View {
    let mailbox: Mailbox

    var body: some View {
        HStack(spacing: 11) {
            Avatar(peer: mailbox.key)
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                    .font(.geistMono(14.5, .semibold))
                    .foregroundStyle(Theme.ink)
                Text(sub)
                    .font(.geist(12.5))
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

    init(initialPeer: String? = nil, onStarted: @escaping (String) -> Void) {
        self.onStarted = onStarted
        _peer = State(initialValue: initialPeer ?? "/")
    }
    @State private var peer = "/"
    @State private var body_ = ""
    @State private var error: String?
    @State private var sending = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("/bekir/agent1 or /k/…", text: $peer)
                        .onChange(of: peer) { _, input in
                            // Normalize the committed field value so UIKit displays the correction
                            // too; sanitizing only a Binding setter can leave the raw paste visible.
                            let value = PeerFace.conversationAddressInput(input)
                            let normalized = "/" + value.drop(while: { $0 == "/" })
                            if peer != normalized { peer = normalized }
                        }
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.geistMono(15))
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
                        .font(.geist(13))
                        .foregroundStyle(Theme.Pill.blockedText)
                }
            }
            .sheetSurface()
            .navigationTitle("New conversation")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Send") { send() }
                        .disabled(sending || !PeerFace.validConversationAddress(peer) || body_.trimmed.isEmpty)
                }
            }
        }
    }

    private func send() {
        guard PeerFace.validConversationAddress(peer), !body_.trimmed.isEmpty, !sending else { return }
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
