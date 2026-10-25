//  Settings → Privacy & filtering and Settings → Blocked (SDS §13.4, §15.3).
//
//  The rows, their order, their words and the flag each depends on are the web inbox's
//  (`POLICY_ROWS`), so both clients describe the same policy the same way. A row whose feature is off
//  is not shown at all.

import SwiftUI

struct PolicyRow: Identifiable {
    let field: String
    let flag: String
    let title: String
    let note: String?
    let options: [(value: String, label: String)]
    var id: String { field }

    static let all: [PolicyRow] = [
        PolicyRow(field: "unknown_calls", flag: Features.callAdmission, title: "Calls from people you don’t know",
                  note: "Silence keeps the call out of your way and lists it under Filtered calls.",
                  options: [("ring", "Ring"), ("silence", "Silence"), ("block", "Block")]),
        PolicyRow(field: "unnamed_messages", flag: Features.messageFiltering, title: "Messages from addresses without a name",
                  note: "Addresses like /k/… that have no handle.",
                  options: [("show", "Show"), ("filter", "Filter"), ("block", "Block")]),
        PolicyRow(field: "unknown_messages", flag: Features.messageFiltering, title: "Messages from people you don’t know",
                  note: "People you know are your contacts and anyone you have written to first.",
                  options: [("show", "Show"), ("filter", "Filter"), ("block", "Block")]),
        PolicyRow(field: "unnamed_calls", flag: Features.callAdmission, title: "Calls from addresses without a name",
                  note: nil, options: [("ring", "Ring"), ("silence", "Silence"), ("block", "Block")]),
        PolicyRow(field: "low_reputation", flag: Features.messageFiltering, title: "Low-reputation senders",
                  note: "Senders others have reported as spam.",
                  options: [("filter", "Filter"), ("allow", "Allow")]),
        PolicyRow(field: "call_history_days", flag: Features.callHistory, title: "Keep call history",
                  note: "Off stops keeping new calls. Entries older than this are deleted.",
                  options: [("0", "Off"), ("30", "30 days"), ("180", "180 days"), ("365", "1 year")]),
        PolicyRow(field: "read_receipts", flag: Features.receipts, title: "Read receipts",
                  note: "Share when you have seen a message. Turning this off also hides other people's seen state from you.",
                  options: [("false", "Off"), ("true", "On")]),
    ]
}

struct PrivacyPage: View {
    @Environment(Account.self) private var account
    @State private var policy: InboxFilters?
    @State private var error: String?
    @State private var saved: String?
    @State private var generation = UUID()
    @State private var saving = false

    var body: some View {
        List {
            ForEach(PolicyRow.all.filter { account.features.has($0.flag) }) { row in
                Section {
                    Picker(row.title, selection: binding(row)) {
                        ForEach(row.options, id: \.value) { option in
                            Text(option.label).tag(option.value)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .disabled(policy == nil || saving)
                    .accessibilityIdentifier("policy-" + row.field)
                } header: {
                    Text(row.title)
                } footer: {
                    if let note = row.note { Text(note) }
                }
            }
            if let error {
                Section {
                    Text(error)
                        .font(.geist(13))
                        .foregroundStyle(Theme.Pill.blockedText)
                }
            }
        }
        .sheetSurface()
        .task(id: account.me?.address) { await load() }
        .toast($saved)
    }

    private func binding(_ row: PolicyRow) -> Binding<String> {
        Binding(
            get: { policy?[row.field] ?? "" },
            set: { value in save(row.field, value) }
        )
    }

    private func load() async {
        let request = UUID()
        generation = request
        policy = nil
        saving = false
        guard let address = account.me?.address else { return }
        error = nil
        #if DEBUG
        if Fixtures.enabled {
            policy = policy ?? InboxFilters(unknownCalls: "ring", unnamedCalls: "silence", unnamedMessages: "show",
                                            unknownMessages: "show", lowReputation: "filter", callHistoryDays: 180)
            return
        }
        #endif
        do {
            let answer = try await account.client.policy(identity: address)
            guard !Task.isCancelled, request == generation, address == account.me?.address else { return }
            policy = answer
        } catch {
            guard !Task.isCancelled, request == generation, address == account.me?.address else { return }
            self.error = "Could not load these settings. Close and try again."
        }
    }

    /// Optimistic, then the server's answer; the old value comes back if the save fails.
    private func save(_ field: String, _ value: String) {
        guard !saving, var next = policy, let address = account.me?.address,
              let wire = InboxFilters.wireValue(field: field, value: value) else { return }
        let previous = next[field]
        next.set(field, value)
        policy = next
        error = nil
        guard !Fixtures.enabled else { saved = "Saved."; return }
        let request = generation
        saving = true
        Task {
            defer { if request == generation { saving = false } }
            do {
                let answer = try await account.client.setPolicy(identity: address, field: field, value: wire)
                guard request == generation, address == account.me?.address else { return }
                policy = answer
                saved = "Saved."
            } catch {
                guard request == generation, address == account.me?.address else { return }
                if var back = policy, let previous { back.set(field, previous); policy = back }
                self.error = "Could not save that setting. Please try again."
            }
        }
    }
}

struct BlockedPage: View {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @State private var rows: [Contact]?
    @State private var error: String?
    @State private var adding = ""
    @State private var working = false
    @State private var said: String?

    var body: some View {
        List {
            Section {
                if let rows {
                    if rows.isEmpty {
                        Text("Nobody is blocked.")
                            .foregroundStyle(Theme.muted)
                    }
                    ForEach(rows, id: \.peer) { contact in
                        HStack(spacing: 10) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(contact.peer)
                                    .font(.geistMono(13.5, .semibold))
                                    .foregroundStyle(Theme.ink)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                Text(contact.isWildcard ? "Everyone at \(String(contact.peer.dropLast(2)))"
                                     : (contact.alias?.isEmpty == false ? contact.alias! : "Blocked"))
                                    .font(.geist(12.5))
                                    .foregroundStyle(Theme.muted)
                            }
                            Spacer(minLength: 8)
                            Button("Unblock") { unblock(contact.peer) }
                                .buttonStyle(.borderless)
                                .disabled(working)
                                .accessibilityLabel("Unblock \(contact.peer)")
                                .accessibilityIdentifier("unblock:" + contact.peer)
                        }
                    }
                } else {
                    ProgressView()
                }
            } footer: {
                Text("Blocked senders can’t message or call this mailbox, and aren’t told.")
            }
            Section("Block an address") {
                HStack {
                    TextField("/k/…, /spamco/agent or /spamco/*", text: $adding)
                        .font(.geistMono(14))
                        .noAutocapitalize()
                        .autocorrectionDisabled()
                        .onSubmit(add)
                        .accessibilityIdentifier("blockAddressField")
                    Button("Block", action: add)
                        .disabled(working || adding.trimmingCharacters(in: .whitespaces).isEmpty)
                        .accessibilityIdentifier("blockAddressButton")
                }
            }
            if let error {
                Section {
                    Text(error)
                        .font(.geist(13))
                        .foregroundStyle(Theme.Pill.blockedText)
                }
            }
        }
        .sheetSurface()
        .task(id: account.me?.address) { await load() }
        .toast($said)
    }

    private func load() async {
        guard let address = account.me?.address else { return }
        error = nil
        guard !Fixtures.enabled else {
            rows = inbox.contacts.filter { $0.admission == "block" }
            return
        }
        do {
            rows = try await account.client.blockedContacts(identity: address)
        } catch {
            rows = inbox.contacts.filter { $0.admission == "block" }
            self.error = "Could not refresh the list. Showing what was last loaded."
        }
    }

    private func unblock(_ peer: String) {
        working = true
        Task {
            if await Blocking.unblock(peer, inbox: inbox) {
                rows?.removeAll { $0.peer == peer }
                said = "Unblocked \(peer)."
                error = nil
            } else {
                error = "Could not unblock \(peer). Please try again."
            }
            working = false
        }
    }

    private func add() {
        var peer = adding.trimmingCharacters(in: .whitespacesAndNewlines)
        if !peer.isEmpty, !peer.hasPrefix("/") { peer = "/" + peer }
        guard Blocking.validAddress(peer) else {
            error = "Enter an address such as /k/…, /spamco/agent or /spamco/*."
            return
        }
        guard peer != account.me?.address, peer != account.me?.handle else {
            error = "You cannot block this mailbox itself."
            return
        }
        working = true
        error = nil
        Task {
            if let failure = await Blocking.block(peer, inbox: inbox) {
                error = failure
            } else {
                adding = ""
                rows = (rows ?? []).filter { $0.peer != peer }
                    + [Contact(peer: peer, alias: nil, admission: "block", autonomy: "review", allowedVerbs: [])]
                said = "Blocked. They won’t be able to message or call you."
            }
            working = false
        }
    }
}
