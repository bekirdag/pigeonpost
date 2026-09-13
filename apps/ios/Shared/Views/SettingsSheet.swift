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

    /// One sheet at a time, for the same reason `ConversationsView` has one: stacked
    /// `.sheet` modifiers on a single view are not reliably all honoured, and the one that loses
    /// simply never appears.
    @State private var sheet: Sheet?
    /// Owned here so it outlives every re-render of the rows below it. See `BuyHandleSection`.
    @State private var handle: HandleStore?

    private enum Sheet: Identifiable {
        case addSender
        case edit(Contact)
        case scan

        var id: String {
            switch self {
            case .addSender: return "add"
            case let .edit(contact): return "edit:" + contact.peer
            case .scan: return "scan"
            }
        }
    }

    private enum Page: String, Hashable {
        case account = "Account"
        case handles = "Handles"
        case purchases = "Get a handle"
        case inbox = "Inbox and storage"
        case contacts = "Contacts and permissions"
        case help = "Help and about"
    }
    @State private var confirmSignOut = false

    var body: some View {
        NavigationStack {
            List {
                Section {
                    destination(.account, icon: "person.crop.circle", detail: session.username ?? "Your profile and devices")
                }
                Section {
                    destination(.handles, icon: "at", detail: handle == nil ? "Loading your handles…" : "Your names and subscriptions")
                        .disabled(handle == nil)
                    destination(.inbox, icon: "tray", detail: "Storage and archived conversations")
                    destination(.contacts, icon: "person.2", detail: "Senders you know and trust")
                }
                Section {
                    destination(.help, icon: "questionmark.circle", detail: "Support, privacy and app information")
                }
            }
            .navigationTitle("Settings")
            .inlineTitle()
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .navigationDestination(for: Page.self) { page in
                List { pageContent(page) }
                    .navigationTitle(page.rawValue)
                    .inlineTitle()
                    .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            }
        }
        // Keep one store alive while moving between pages, including an unfinished purchase.
        .task {
            #if DEBUG
            if Fixtures.enabled {
                if handle == nil { handle = HandleFixtures.make(Fixtures.handleState ?? "unavailable", account: account) }
                await handle?.refresh()
                return
            }
            #endif
            if handle == nil { handle = account.handles }
            await handle?.refresh()
        }
        .sheet(item: $sheet) { which in
            switch which {
            case .addSender: ContactSheet(existing: nil)
            case let .edit(contact): ContactSheet(existing: contact)
            case .scan:
                #if os(iOS)
                ScanView()
                #else
                EmptyView()
                #endif
            }
        }
        .confirmationDialog("Sign out of Pigeonpost?", isPresented: $confirmSignOut, titleVisibility: .visible) {
            Button("Sign out", role: .destructive) {
                dismiss()
                inbox.reset()
                Task { await account.signOut() }
            }
        } message: { Text("You can sign in again to return to your inboxes and handles.") }
    }

    private func destination(_ page: Page, icon: String, detail: String) -> some View {
        NavigationLink(value: page) {
            HStack(spacing: 14) {
                Image(systemName: icon)
                    .font(.system(size: 22)).foregroundStyle(.tint)
                    .frame(width: 30).accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(page.rawValue).font(.body.weight(.medium))
                    Text(detail).font(.subheadline).foregroundStyle(.secondary)
                }
                .padding(.vertical, 6)
            }
        }
        .accessibilityIdentifier("settings-" + String(describing: page))
    }

    @ViewBuilder private func pageContent(_ page: Page) -> some View {
        switch page {
        case .account:
            Section("Your account") {
                LabeledContent("Signed in as", value: session.username ?? "—")
                LabeledContent("Current inbox", value: account.me?.key ?? "—")
                    .textSelection(.enabled)
            }
            #if os(iOS)
            Section {
                Button { sheet = .scan } label: { Label("Scan a sign-in code", systemImage: "qrcode.viewfinder") }
            } footer: { Text("Scan a Pigeonpost code to sign in on another device.") }
            #endif
            Section {
                Button("Sign out", role: .destructive) { confirmSignOut = true }
                Link("Delete account", destination: URL(string: "https://pigeonpost.dev/account#delete-account")!)
                    .foregroundStyle(.red).accessibilityIdentifier("deleteAccount")
            }
        case .handles:
            Section {
                destination(.purchases, icon: "plus.circle", detail: "Register a name or restore purchases")
            }
            if let handle { AccountHandlesSection(store: handle, closeSettings: { dismiss() }) }
        case .purchases:
            if let handle { BuyHandleSection(store: handle, closeSettings: { dismiss() }).id("handle-section") }
        case .inbox:
            if let quota = inbox.quota { MailboxUsageSection(quota: quota) }
            else { Section("Storage") { Text("Storage information is unavailable.").foregroundStyle(.secondary) } }
            Section {
                Button { inbox.viewingArchive = true; dismiss() } label: {
                    HStack {
                        Label("Archived conversations", systemImage: "archivebox")
                        Spacer()
                        Text("\(inbox.archivedCount)").foregroundStyle(.secondary)
                    }
                }
            } footer: { Text("Archived conversations stay saved. New messages still arrive.") }
        case .contacts:
            Section {
                ForEach(inbox.contacts, id: \.peer) { contact in
                    Button { sheet = .edit(contact) } label: { ContactRow(contact: contact) }.buttonStyle(.plain)
                }
                Button { sheet = .addSender } label: { Label("Add a sender", systemImage: "person.badge.plus") }
            } footer: {
                Text("Choose who can send you messages and which requests need your approval. Adding a sender does not grant automatic permissions.")
            }
        case .help:
            Section {
                Link("Contact support", destination: URL(string: "https://pigeonpost.dev/app-support.html")!)
                Link("Privacy policy", destination: URL(string: "https://pigeonpost.dev/app-privacy.html")!)
                Link("Terms of use", destination: URL(string: "https://pigeonpost.dev/app-terms.html")!)
            }
            Section("About Pigeonpost") {
                LabeledContent("Version", value: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "1.0")
                LabeledContent("Postbox", value: Config.postbox.host ?? "—")
                Text("Wodo Teknoloji A.Ş.").foregroundStyle(.secondary)
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
                        .noAutocapitalize()
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
            .inlineTitle()
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
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

/// How full this mailbox is.
///
/// Shown always rather than only when it matters, because the quota refuses the *sender*: when a
/// mailbox fills, the bounce goes to whoever wrote and the holder sees nothing at all. Somebody who
/// has watched the bar creep up is not surprised by it.
private struct MailboxUsageSection: View {
    let quota: Quota

    var body: some View {
        Section {
            VStack(alignment: .leading, spacing: 7) {
                HStack {
                    Text("\(quota.used) of \(quota.limit)")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(Theme.ink)
                    Spacer()
                    if quota.isFull {
                        PillView(text: "full", kind: .blocked)
                    }
                }
                ProgressView(value: quota.fraction)
                    .tint(quota.isFull ? Theme.Pill.blockedText : (quota.shouldWarn ? .orange : Theme.navy))
            }
            .padding(.vertical, 2)
        } header: {
            Text("Mailbox")
        } footer: {
            if quota.isFull {
                Text(quota.canBuyMoreRoom
                     ? "This mailbox is full, so new mail is being refused and senders are told. Delete a few messages to make room — hold one and choose Delete — or buy a handle above for more space."
                     : "This mailbox is full, so new mail is being refused and senders are told. Delete a few messages to make room: hold one and choose Delete.")
            } else if quota.shouldWarn {
                Text(quota.canBuyMoreRoom
                     ? "Nearly full. When it fills, new mail is refused and senders are told. Delete messages to make room, or buy a handle above for more space."
                     : "Nearly full. When it fills, new mail is refused and senders are told. Hold a message and choose Delete to make room.")
            } else {
                Text("Nothing here is deleted on a schedule — mail stays until you delete it or the mailbox fills. Hold a message to delete it.")
            }
        }
    }
}
