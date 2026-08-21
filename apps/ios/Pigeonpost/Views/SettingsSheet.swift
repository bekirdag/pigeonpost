//  Settings: the archive, who this mailbox admits, and the way out.
//
//  Admission and autonomy are the mailbox holder's decisions, so they are editable here — but the
//  vocabulary is the server's. A verb the postbox refuses to auto-accept for anybody is shown as
//  refused rather than offered and then rejected on save.

import SwiftUI

struct SettingsSheet: View {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(Session.self) private var session
    @Environment(\.dismiss) private var dismiss

    @State private var editing: Contact?
    @State private var addingSender = false

    var body: some View {
        NavigationStack {
            List {
                BuyHandleSection()

                Section {
                    Button {
                        inbox.viewingArchive = true
                        dismiss()
                    } label: {
                        HStack {
                            Label("Archived conversations", systemImage: "archivebox")
                            Spacer()
                            Text("\(inbox.archivedCount)")
                                .foregroundStyle(Theme.muted)
                        }
                    }
                } footer: {
                    Text("Archiving hides a conversation from your inbox. Nothing is deleted, the other side is never told, and new mail from them still arrives and still counts as unread.")
                }

                Section {
                    ForEach(inbox.contacts, id: \.peer) { contact in
                        Button { editing = contact } label: { ContactRow(contact: contact) }
                            .buttonStyle(.plain)
                    }
                    Button("Add a sender") { addingSender = true }
                        .font(.system(size: 15, weight: .medium))
                } header: {
                    Text("Trusted senders")
                } footer: {
                    Text("Who this mailbox admits, and how far it trusts them. /namespace/* covers a whole fleet. Autonomy *auto* plus a verb lets that sender's request be acted on without asking you first.")
                }

                Section("Account") {
                    LabeledContent("Signed in as", value: session.username ?? "—")
                    LabeledContent("Mailbox", value: account.me?.key ?? "—")
                    LabeledContent("Postbox", value: Config.postbox.host ?? "—")
                    Button("Sign out", role: .destructive) {
                        dismiss()
                        inbox.reset()
                        account.signOut()
                    }
                }
                .font(.system(size: 14))
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .sheet(item: $editing) { contact in
                ContactSheet(existing: contact)
            }
            .sheet(isPresented: $addingSender) {
                ContactSheet(existing: nil)
            }
        }
    }
}

private struct ContactRow: View {
    let contact: Contact

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 8) {
                Text(contact.alias?.isEmpty == false ? contact.alias! : PeerFace.displayName(contact.peer))
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Theme.ink)
                if contact.admission == "block" { PillView(text: "blocked", kind: .blocked) }
                if contact.autonomy == "auto" { PillView(text: "auto", kind: .auto) }
            }
            Text(contact.peer)
                .font(.system(size: 12.5, design: .monospaced))
                .foregroundStyle(Theme.muted)
        }
        .padding(.vertical, 2)
    }
}

struct ContactSheet: View {
    let existing: Contact?

    @Environment(Inbox.self) private var inbox
    @Environment(\.dismiss) private var dismiss

    @State private var peer = ""
    @State private var alias = ""
    @State private var admission = "allow"
    @State private var autonomy = "review"
    @State private var verbs: Set<String> = []
    @State private var error: String?
    @State private var working = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Address") {
                    TextField("/bekir/* or /bekir/agent1", text: $peer)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.system(size: 15, design: .monospaced))
                        .disabled(existing != nil)
                    TextField("Name for them", text: $alias)
                }

                Section {
                    Picker("Admission", selection: $admission) {
                        Text("Allow — their mail is admitted").tag("allow")
                        Text("Block — refuse their mail").tag("block")
                    }
                    Picker("Autonomy", selection: $autonomy) {
                        Text("Review — always ask me first").tag("review")
                        Text("Auto — may act on the verbs below").tag("auto")
                    }
                }
                .pickerStyle(.inline)

                Section {
                    ForEach(inbox.vocabulary?.grantable ?? [], id: \.self) { verb in
                        Toggle(verb, isOn: Binding(
                            get: { verbs.contains(verb) },
                            set: { on in if on { verbs.insert(verb) } else { verbs.remove(verb) } }
                        ))
                        .font(.system(size: 14, design: .monospaced))
                        .disabled(autonomy != "auto")
                    }
                } header: {
                    Text("Requests they may have acted on")
                } footer: {
                    if let never = inbox.vocabulary?.neverAuto, !never.isEmpty {
                        Text("Never automatic, whoever asks: \(never.joined(separator: ", ")).")
                    }
                }

                if let error {
                    Text(error)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.Pill.blockedText)
                }

                if existing != nil {
                    Section {
                        Button("Remove this sender", role: .destructive) { remove() }
                    }
                }
            }
            .navigationTitle(existing == nil ? "Add a sender" : "Trusted sender")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Save") { save() }.disabled(working || peer.trimmed.isEmpty)
                }
            }
            .onAppear {
                guard let existing else { return }
                peer = existing.peer
                alias = existing.alias ?? ""
                admission = existing.admission
                autonomy = existing.autonomy
                verbs = Set(existing.allowedVerbs ?? [])
            }
        }
    }

    private func save() {
        working = true
        error = nil
        Task {
            do {
                try await inbox.saveContact(
                    peer: peer.trimmed,
                    alias: alias.trimmed.isEmpty ? nil : alias.trimmed,
                    admission: admission,
                    // Verbs only mean anything alongside auto; sending them with review would store
                    // a grant that reads as active and is not.
                    autonomy: autonomy,
                    allowedVerbs: autonomy == "auto" ? Array(verbs).sorted() : []
                )
                dismiss()
            } catch let failure as APIError {
                error = failure.errorDescription
            } catch {
                self.error = "Could not save."
            }
            working = false
        }
    }

    private func remove() {
        working = true
        Task {
            do {
                try await inbox.removeContact(peer: peer)
                dismiss()
            } catch {
                self.error = "Could not remove."
            }
            working = false
        }
    }
}

extension Contact: Identifiable {
    public var id: String { peer }
}
