//  Who a sender is, and how far this mailbox trusts them.
//
//  Shared rather than iOS-only: the Mac asks exactly the same questions of a sender and had no way
//  to answer them. The toolbar placements are `.cancellationAction` and `.confirmationAction`,
//  which mean the right thing on both platforms, and the two modifiers that exist only on a phone
//  go through the shims in `Shared/Design/Platform.swift`.

import SwiftUI

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
            .inlineTitle()
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
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
