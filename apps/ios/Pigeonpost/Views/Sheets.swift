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

/// Who this sender is, and what this mailbox has decided about them. Read-only on purpose: granting
/// is a deliberate act by the mailbox holder, and Settings is where it is made.
struct PeerInfoSheet: View {
    let conversation: Conversation
    let onOpenMailbox: (Mailbox) -> Void

    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(\.dismiss) private var dismiss
    @State private var confirmingBlock = false
    @State private var editingContact = false

    private var lastIncoming: ThreadMessage? {
        conversation.messages.last { $0.kind == .incoming }
    }

    private var archived: Bool { inbox.archived.contains(conversation.peer) }

    /// Being "known" is having a row of your own. Turning it off forgets this sender by name; it
    /// cannot reach into a namespace rule, which is what the footnote says when one covers them.
    private var knownBinding: Binding<Bool> {
        Binding(
            get: { inbox.exactContact(conversation.peer) != nil },
            set: { on in
                Task {
                    if on { await inbox.markKnown(peer: conversation.peer) }
                    else { await inbox.forget(peer: conversation.peer) }
                }
            }
        )
    }

    private var fullBinding: Binding<Bool> {
        Binding(
            get: { inbox.hasFullPermissions(conversation.peer) },
            set: { on in
                Task { await inbox.grantFullPermissions(peer: conversation.peer, granted: on) }
            }
        )
    }

    private var trustFootnote: String {
        if conversation.isBlocked {
            return "Blocked. Their mail is refused, so trust does not apply until you unblock them."
        }
        if let wildcard = inbox.wildcardContact(conversation.peer) {
            return "Covered by \(wildcard.peer). Marking this sender known gives them a row of their own, which outranks that rule."
        }
        if inbox.hasFullPermissions(conversation.peer) {
            return "Their requests are acted on without asking you. The postbox still refuses what it never auto-accepts for anyone, and the receiving machine's own permission tier and limits still apply on top."
        }
        return "A known sender is admitted by name. Full permissions lets their requests be acted on without waiting for you; everything else is held."
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    row("Address", conversation.identity?.address ?? lastIncoming?.address ?? conversation.peer, mono: true)
                    if conversation.peer.hasPrefix("/"), !conversation.peer.hasPrefix("/k/") {
                        row("Handle", conversation.peer, mono: true)
                    }
                    if conversation.mine { row("Mailbox", "yours — on this account") }
                }
                // The decisions, first — this panel is opened to change them, not to read an
                // address.
                //
                // Shown for your own mailboxes too. They were hidden there on the grounds that a
                // mailbox on your own account is not a sender to be decided about, which is exactly
                // backwards: your fleet is who writes to you, the postbox already grants them verbs
                // through the namespace rule, and the agent you most need to set terms for is the
                // one that can push and deploy. Hiding the controls did not make the decision
                // simpler, it made it invisible.
                Group {
                    Section {
                        Toggle("Known sender", isOn: knownBinding)
                        Toggle("Full permissions", isOn: fullBinding)
                            .disabled(!inbox.isKnown(conversation.peer) || conversation.isBlocked)
                        Button("Choose which requests run…") { editingContact = true }
                            .disabled(inbox.exactContact(conversation.peer) == nil)
                    } header: {
                        Text("Trust")
                    } footer: {
                        Text(trustFootnote)
                    }

                    Section {
                        Button(archived ? "Move back to the inbox" : "Archive this conversation") {
                            let wanted = !archived
                            Task { await inbox.setArchived(conversation.peer, archived: wanted) }
                            dismiss()
                        }
                        if conversation.isBlocked {
                            Button("Unblock this sender") {
                                Task { await inbox.markKnown(peer: conversation.peer) }
                            }
                        } else {
                            Button("Block this sender", role: .destructive) { confirmingBlock = true }
                        }
                    } footer: {
                        Text(conversation.mine
                             ? "Archiving hides a conversation; nothing is deleted and their mail still arrives. Blocking one of your own agents refuses its mail from here on — the way to stop one talking to you is usually to stop running it."
                             : "Archiving hides a conversation; nothing is deleted and their mail still arrives. Blocking refuses it from here on. Neither is announced to them.")
                    }
                }

                if conversation.mine, let identity = conversation.identity {
                    Section {
                        Button("Open this mailbox") { onOpenMailbox(identity) }
                            .font(.system(size: 15, weight: .semibold))
                        Text(note)
                            .font(.system(size: 12.5))
                            .foregroundStyle(Theme.muted)
                    }
                }


                // What cannot be changed from here. Admission and autonomy were listed here once
                // and are not any more: they are the toggles above, and printing a value twice
                // invites the two copies to disagree.
                Section("What the postbox knows") {
                    row("Contact", contactState)
                    row("Granted verbs", grantedVerbs)
                    if let standing = lastIncoming?.standing {
                        row("Standing", standing + (lastIncoming?.tier.map { " · \($0)" } ?? ""))
                    }
                }
            }
            .navigationTitle(conversation.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .sheet(isPresented: $editingContact) {
                if let contact = inbox.exactContact(conversation.peer) {
                    ContactSheet(existing: contact)
                }
            }
            .confirmationDialog(
                "Block \(conversation.name)?",
                isPresented: $confirmingBlock,
                titleVisibility: .visible
            ) {
                Button("Block", role: .destructive) {
                    Task {
                        await inbox.block(peer: conversation.peer)
                        dismiss()
                    }
                }
                Button("Cancel", role: .cancel) {}
            }
        }
        .presentationDetents([.large, .medium])
    }

    private func row(_ term: String, _ value: String, mono: Bool = false) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(term)
                .font(.system(size: 13))
                .foregroundStyle(Theme.muted)
            Spacer(minLength: 12)
            Text(value)
                .font(.system(size: 13, design: mono ? .monospaced : .default))
                .foregroundStyle(Theme.body)
                .multilineTextAlignment(.trailing)
                .textSelection(.enabled)
        }
    }

    private var contactState: String {
        guard let contact = conversation.contact else { return "not a contact" }
        return contact.peer == conversation.peer ? "yes" : "via \(contact.peer)"
    }

    private var grantedVerbs: String {
        let verbs = conversation.contact?.allowedVerbs ?? []
        return verbs.isEmpty ? "none" : verbs.joined(separator: ", ")
    }

    private var note: String {
        if conversation.mine {
            let from = account.me?.handle ?? account.me?.address ?? "this mailbox"
            return "You are writing to this agent from \(from). Opening the mailbox instead shows the mail it has received."
        }
        return conversation.contact?.autonomy == "auto"
            ? "Requests naming a granted verb are acted on without you. Everything else is held."
            : "Nothing from this sender is acted on automatically."
    }
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

extension String {
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}
