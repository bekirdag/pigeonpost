//  Making a group, and joining one from a link.

import SwiftUI

struct NewGroupSheet: View {
    /// Told the new group's id, to open it.
    let created: (String) -> Void

    @Environment(GroupStore.self) private var groups
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(\.dismiss) private var dismiss

    @State private var who = ""
    @State private var people: [String] = []
    @State private var name = ""
    @State private var about = ""
    @State private var joinPolicy = "private"
    @State private var postPolicy = "everyone"
    @State private var timer: Int?
    @State private var error: String?
    @State private var working = false

    /// Contacts by name, offered as people to add. Wildcard rules are not people.
    private var suggestions: [String] {
        let typed = who.lowercased()
        return inbox.contacts.map(\.peer)
            .filter { !$0.contains("*") && !people.contains($0) && (typed.isEmpty || $0.lowercased().contains(typed)) }
            .prefix(6).map { $0 }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if !people.isEmpty {
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack(spacing: 6) {
                                ForEach(people, id: \.self) { person in
                                    Button { people.removeAll { $0 == person } } label: {
                                        HStack(spacing: 4) {
                                            Text(PeerFace.displayName(person)).font(.geistMono(12.5))
                                            Image(systemName: "xmark").font(.geist(9, .bold))
                                        }
                                        .padding(.horizontal, 8).padding(.vertical, 5)
                                        .background(Theme.chip, in: Capsule())
                                    }
                                    .buttonStyle(.plain)
                                    .accessibilityLabel("Remove \(person)")
                                }
                            }
                        }
                    }
                    HStack {
                        TextField("/alp/main or /k/…", text: $who)
                            .noAutocapitalize()
                            .autocorrectionDisabled()
                            .font(.geistMono(14))
                            .onSubmit(addPerson)
                            .accessibilityIdentifier("newGroupWho")
                        Button("Add", action: addPerson).disabled(who.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                    ForEach(suggestions, id: \.self) { peer in
                        Button { people.append(peer); who = "" } label: {
                            Label(PeerFace.displayName(peer), systemImage: "person.badge.plus").font(.geist(13.5))
                        }
                    }
                } header: { Text("Who") }

                Section {
                    TextField("Garden club", text: $name).accessibilityIdentifier("newGroupName")
                    TextField("Description (optional)", text: $about, axis: .vertical).lineLimit(1...4)
                } header: { Text("Name") }

                Section {
                    Picker("Who can join", selection: $joinPolicy) {
                        Text("Private — invite only").tag("private")
                        Text("Public — anyone with the link").tag("public")
                    }
                    Picker("Who can post", selection: $postPolicy) {
                        Text("Everyone").tag("everyone")
                        Text("Only admins").tag("admins")
                    }
                    if account.features.disappearing {
                        TimerPicker(seconds: timer) { timer = $0 }
                    }
                } footer: {
                    Text("Group messages are end-to-end encrypted. People you add see messages from when they join, not before."
                         + (timer != nil ? " " + GroupTimers.limits : ""))
                }

                if let error {
                    Text(error).font(.geist(13)).foregroundStyle(Theme.Pill.blockedText)
                }
            }
            .sheetSurface()
            .navigationTitle("New group")
            .inlineTitle()
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button(working ? "Creating…" : "Create") { create() }
                        .disabled(working || name.trimmingCharacters(in: .whitespaces).isEmpty)
                        .accessibilityIdentifier("createGroup")
                }
            }
        }
    }

    private func addPerson() {
        let raw = who.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: ","))
        guard !raw.isEmpty else { return }
        let address = PeerFace.conversationAddressInput(raw)
        guard PeerFace.validConversationAddress(address) else {
            error = "That does not look like a Pigeonpost address."
            return
        }
        if !people.contains(address), people.count < 200 { people.append(address) }
        who = ""
        error = nil
    }

    private func create() {
        addPerson()
        let title = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { error = "Give the group a name."; return }
        working = true
        error = nil
        Task {
            switch await groups.create(name: String(title.prefix(80)), description: String(about.prefix(500)), invite: people,
                                       joinPolicy: joinPolicy, postPolicy: postPolicy, timer: timer) {
            case let .success(id):
                dismiss()
                created(id)
            case let .failure(failure):
                error = GroupText.explain(failure)
            }
            working = false
        }
    }
}

/// A join link (`https://inbox.pigeonpost.dev/g/<id>#<token>`): a preview first, then Join.
struct JoinGroupSheet: View {
    let link: GroupStore.JoinLink
    let joined: (String) -> Void

    @Environment(GroupStore.self) private var groups
    @Environment(Account.self) private var account
    @Environment(\.dismiss) private var dismiss
    @State private var preview: GroupPreview?
    @State private var problem: String?
    @State private var working = false

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 12) {
                if let preview {
                    HStack(spacing: 12) {
                        GroupFace(id: preview.id, name: preview.name, size: 46)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(preview.name).font(.geist(18, .semibold)).foregroundStyle(Theme.ink)
                            Text("\(preview.memberCount ?? 1) members · joining as \(account.me.map { PeerFace.displayName($0.key) } ?? "")")
                                .font(.geist(12.5)).foregroundStyle(Theme.secondary)
                        }
                    }
                    if let text = preview.description, !text.isEmpty {
                        Text(text).font(.geist(14)).foregroundStyle(Theme.body)
                    }
                    Text("Group messages are end-to-end encrypted. You will see messages from when you join, not before.")
                        .font(.geist(12.5)).foregroundStyle(Theme.muted)
                    if preview.myState == "active" {
                        Text("You are already a member.").font(.geist(14))
                    } else if preview.canJoin != true {
                        Text(preview.needs == "invitation"
                             ? "You have an invitation to this group: accept it from Invitations in your conversation list."
                             : "This group is invite only.")
                            .font(.geist(13.5)).foregroundStyle(Theme.secondary)
                    }
                } else if let problem {
                    Text(problem).font(.geist(14)).foregroundStyle(Theme.body)
                } else {
                    ProgressView()
                }
                Spacer()
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.app)
            .navigationTitle("Join group")
            .inlineTitle()
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Not now") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    if preview?.myState == "active" {
                        Button("Open") { dismiss(); joined(link.groupId) }
                    } else if preview?.canJoin == true {
                        Button(working ? "Joining…" : "Join") { join() }
                            .disabled(working)
                            .accessibilityIdentifier("joinGroup")
                    }
                }
            }
        }
        .task {
            #if DEBUG
            if Fixtures.enabled {
                preview = GroupPreview(id: link.groupId, name: "Neighbourhood swap", description: "Things to give away",
                                       memberCount: 9, joinPolicy: "public", visibility: "unlisted", myState: nil, canJoin: true, needs: nil)
                return
            }
            #endif
            do {
                preview = try await groups.resolve(link)
            } catch let error as APIError where ["not_found", "invalid_link"].contains(error.code ?? "") {
                problem = "This invite link is not valid any more."
            } catch {
                problem = GroupText.explain(error)
            }
        }
    }

    private func join() {
        working = true
        Task {
            do {
                try await groups.join(link)
                dismiss()
                joined(link.groupId)
            } catch {
                problem = GroupText.explain(error)
                preview = nil
            }
            working = false
        }
    }
}

/// Settings for a direct conversation's disappearing timer (SDS §17): either side may change it,
/// and both see who did.
struct ThreadTimerSection: View {
    let threadId: String
    let seconds: Int?
    let changed: () async -> Void

    @Environment(GroupStore.self) private var groups

    var body: some View {
        Section {
            TimerPicker(seconds: seconds) { value in
                Task {
                    if await groups.setThreadTimer(thread: threadId, seconds: value) { await changed() }
                }
            }
        } header: {
            Text("Disappearing messages")
        } footer: {
            Text("Applies to both of you; they see who changed it. The clock starts when a message is sent. " + GroupTimers.limits)
        }
    }
}

/// Pasting a join link, for when the link arrived somewhere that does not open the app by itself.
struct JoinLinkSheet: View {
    let found: (GroupStore.JoinLink) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var problem: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("https://inbox.pigeonpost.dev/g/…", text: $text, axis: .vertical)
                        .noAutocapitalize()
                        .autocorrectionDisabled()
                        .font(.geistMono(13))
                        .accessibilityIdentifier("joinLinkField")
                } footer: {
                    Text(problem ?? "Paste the invite link someone shared with you.")
                }
            }
            .sheetSurface()
            .navigationTitle("Join with a link")
            .inlineTitle()
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Continue") {
                        guard let url = URL(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
                              let link = GroupStore.JoinLink(url: url) else {
                            problem = "That is not a Pigeonpost group link."
                            return
                        }
                        dismiss()
                        found(link)
                    }
                    .disabled(text.isEmpty)
                }
            }
        }
    }
}

/// The timer control for a direct conversation, where its thread is known and timers are on.
struct DirectTimerSection: View {
    let peer: String
    @Environment(Inbox.self) private var inbox
    @Environment(Account.self) private var account

    var body: some View {
        if account.features.disappearing, let thread = inbox.defaultThread(with: peer) {
            ThreadTimerSection(threadId: thread.threadId, seconds: thread.ephemeralSecs) {
                await inbox.reloadThreads()
            }
        }
    }
}

/// "⏱ 1 day" in a direct conversation's bar while its timer is on.
struct DirectTimerBadge: View {
    let peer: String
    @Environment(Inbox.self) private var inbox
    @Environment(Account.self) private var account

    var body: some View {
        if account.features.disappearing, let secs = inbox.defaultThread(with: peer)?.ephemeralSecs {
            Label(GroupTimers.label(secs), systemImage: "timer")
                .labelStyle(.titleAndIcon)
                .font(.geist(12))
                .foregroundStyle(Theme.secondary)
                .accessibilityLabel("Messages disappear after \(GroupTimers.label(secs))")
                .accessibilityIdentifier("directTimer")
        }
    }
}
