import SwiftUI

struct OrganizationRoomsScreen: View {
    let account: Account
    let visit: (Mailbox) -> Void
    let opened: (String) -> Void
    @Environment(GroupStore.self) private var groups
    @Environment(CallHost.self) private var calls
    @Environment(\.dismiss) private var dismiss
    @State private var store: OrganizationRoomStore
    @State private var search = ""
    @State private var revision = 0
    @State private var path: [Route] = []
    @State private var pendingSwitch: MailboxChoice?
    @State private var preparing = false
    @State private var preparationProblem: String?
    @State private var visible = true

    private enum Route: Hashable { case room(String), create(String) }
    private struct Load: Equatable { let scope: OrganizationRoomScope?; let search: String; let revision: Int }
    private struct MailboxChoice { let scope: OrganizationRoomScope; let mailbox: Mailbox }

    init(account: Account, visit: @escaping (Mailbox) -> Void, opened: @escaping (String) -> Void) {
        self.account = account; self.visit = visit; self.opened = opened
        #if DEBUG
        _store = State(initialValue: OrganizationRoomFixtures.store(account: account))
        #else
        _store = State(initialValue: OrganizationRoomStore(session: account.session,
            mailbox: { account.me?.address }, enabled: { account.features.orgs && account.features.groups }))
        #endif
    }

    var body: some View {
        NavigationStack(path: $path) {
            Form {
                if store.context == nil {
                    ContentUnavailableView("Organization rooms are unavailable", systemImage: "building.2")
                } else {
                    directory
                    if let error = store.error {
                        Section { Text(error).foregroundStyle(Theme.Pill.blockedText).accessibilityIdentifier("org-rooms-error") }
                    }
                    if store.loading { Section { ProgressView("Loading rooms…") } }
                }
            }
            .font(.geist(14)).tint(Theme.navy).sheetSurface()
            .accessibilityIdentifier("org-rooms-directory")
            .navigationTitle("Organization rooms").inlineTitle()
            .searchable(text: $search, prompt: "Find a room")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() }.disabled(store.busy || preparing) }
                ToolbarItem(placement: .confirmationAction) {
                    if store.canCreate, let organization = store.organization {
                        NavigationLink(value: Route.create(organization.id)) { Label("New room", systemImage: "plus") }
                            .disabled(!groups.enabled || preparing)
                            .accessibilityIdentifier("org-room-new")
                    }
                }
            }
            .navigationDestination(for: Route.self) { destination in
                if let scope = store.context {
                    switch destination {
                    case .room(let id):
                        OrganizationRoomPreview(store: store, roomId: id, scope: scope, preparing: preparing,
                            preparationProblem: preparationProblem, open: prepare)
                            .id(scope)
                    case .create(let organizationId):
                        OrganizationRoomCreation(store: store, scope: scope, organizationId: organizationId, preparing: preparing,
                            preparationProblem: preparationProblem, open: prepare)
                            .id(scope)
                    }
                } else {
                    ContentUnavailableView("Refresh your organizations", systemImage: "building.2")
                }
            }
        }
        #if os(macOS)
        .frame(width: 580, height: 690)
        #endif
        .interactiveDismissDisabled(store.busy || preparing)
        .task(id: Load(scope: store.context, search: search, revision: revision)) {
            if !search.isEmpty {
                do { try await Task.sleep(for: .milliseconds(300)) } catch { return }
            }
            await store.loadOrganizations(search: search)
        }
        .onChange(of: store.context) { _, _ in
            store.synchronize(); path = []; pendingSwitch = nil; preparationProblem = nil
        }
        .onChange(of: store.selectedOrganizationId) { _, _ in path = []; preparationProblem = nil }
        .onDisappear { visible = false; store.closeReview() }
        .onAppear { visible = true }
        .confirmationDialog("Use this organization inbox?", isPresented: Binding(get: { pendingSwitch != nil }, set: { if !$0 { pendingSwitch = nil } }), titleVisibility: .visible) {
            if let choice = pendingSwitch {
                Button("Use inbox") {
                    guard choice.scope == store.context, calls.mode == .hidden, !calls.preparingScheduled,
                          account.mailboxes.contains(where: { $0.address == choice.mailbox.address }),
                          store.organizations.contains(where: { $0.myMailbox?.address == choice.mailbox.address }) else { pendingSwitch = nil; return }
                    pendingSwitch = nil; visit(choice.mailbox)
                }
                Button("Cancel", role: .cancel) { pendingSwitch = nil }
            }
        } message: {
            if let choice = pendingSwitch { Text("Rooms will open as \(choice.mailbox.key).") }
        }
    }

    @ViewBuilder private var directory: some View {
        if let organization = store.organization {
            Section {
                if store.matchingOrganizations.count > 1 {
                    Picker("Organization", selection: Binding(get: { store.selectedOrganizationId ?? "" }, set: {
                        if store.selectOrganization($0) { revision += 1 }
                    })) {
                        ForEach(store.matchingOrganizations) { Text($0.name).tag($0.id) }
                    }
                    .accessibilityIdentifier("org-rooms-organization")
                } else { Label(organization.name, systemImage: "building.2").font(.geist(17, .semibold)) }
                if let mailbox = account.me { Text("Using \(mailbox.key)").foregroundStyle(Theme.secondary) }
            }
            if organization.readable {
                Section {
                    ForEach(store.rooms) { room in
                        NavigationLink(value: Route.room(room.id)) {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(room.name).foregroundStyle(Theme.ink)
                                Text(roomStatus(room)).font(.geist(12.5)).foregroundStyle(Theme.secondary)
                            }
                        }
                        .accessibilityIdentifier("org-room:" + room.id)
                    }
                    if store.rooms.isEmpty, !store.loading {
                        Text(search.isEmpty ? "No rooms are listed here yet." : "No rooms match your search.").foregroundStyle(Theme.secondary)
                    }
                } header: { Text("Rooms") } footer: {
                    Text("Messages are end-to-end encrypted. Your organization controls who can join each room.")
                }
            } else {
                Section { Text("Rooms are unavailable while this organization is suspended.") }
            }
        } else if !store.organizations.isEmpty {
            Section {
                Text("Choose your organization inbox to see its rooms.").foregroundStyle(Theme.secondary)
                ForEach(store.organizations) { organization in
                    if let address = organization.myMailbox?.address,
                       let mailbox = account.mailboxes.first(where: { $0.address == address }), let scope = store.context {
                        Button {
                            pendingSwitch = .init(scope: scope, mailbox: mailbox)
                        } label: {
                            VStack(alignment: .leading, spacing: 4) {
                                Label(organization.name, systemImage: "building.2")
                                Text(mailbox.key).font(.geist(12.5)).foregroundStyle(Theme.secondary)
                            }
                        }
                        .disabled(calls.mode != .hidden || calls.preparingScheduled || store.busy)
                        .accessibilityIdentifier("org-room-switch:" + mailbox.address)
                    } else {
                        Text(organization.name + " — organization inbox not available").foregroundStyle(Theme.secondary)
                    }
                }
                if calls.mode != .hidden || calls.preparingScheduled { Text("Finish your call before switching inboxes.") }
            } header: { Text("Organization inboxes") }
        } else if !store.loading {
            Section { ContentUnavailableView("No organizations found", systemImage: "building.2", description: Text("Your organization inboxes will appear here after you join.")) }
        }
        Section {
            Button("Refresh organizations") { revision += 1 }.disabled(store.busy || preparing)
                .accessibilityIdentifier("org-rooms-refresh")
        }
    }

    private func roomStatus(_ room: OrganizationRoom) -> String {
        let status: String
        switch room.myState {
        case "active": status = "Joined"
        case "keying": status = "Waiting for encryption"
        case "invited": status = "Invitation waiting"
        default: status = room.joinPolicy == "public" ? "Organization members can join" : "Invitation required"
        }
        return "\(room.memberCount) members · \(status)"
    }

    private func prepare(_ opening: OrganizationRoomOpening) async -> Bool {
        guard !preparing, visible, store.allows(opening) else { return false }
        guard groups.enabled else { preparationProblem = "The encrypted conversation is not ready. Reopen your inbox and try again."; return false }
        preparing = true; preparationProblem = nil
        defer { preparing = false }
        #if DEBUG
        if Fixtures.enabled { OrganizationRoomFixtures.stage(opening, in: groups) }
        #endif
        for _ in 0..<3 {
            guard visible, !Task.isCancelled, store.allows(opening) else { return false }
            await groups.refresh()
            guard visible, !Task.isCancelled, store.allows(opening) else { return false }
            if let group = groups.group(opening.groupId), group.orgId == opening.organizationId,
               group.deletedAt == nil, group.my?.state == "active" || group.my?.state == "keying" {
                opened(opening.groupId); dismiss(); return true
            }
            do { try await Task.sleep(for: .milliseconds(150)) } catch { return false }
        }
        preparationProblem = "The room is confirmed. Its encrypted conversation is still loading; try opening it again."
        return false
    }
}

private struct OrganizationRoomPreview: View {
    let store: OrganizationRoomStore
    let roomId: String
    let scope: OrganizationRoomScope
    let preparing: Bool
    let preparationProblem: String?
    let open: (OrganizationRoomOpening) async -> Bool
    @State private var opening: OrganizationRoomOpening?

    var body: some View {
        Form {
            if scope == store.context {
                if let preview = store.preview {
                    Section {
                        Label(preview.name, systemImage: "person.3").font(.geist(18, .semibold))
                        if let description = preview.description, !description.isEmpty { Text(description) }
                        Text("\(preview.memberCount ?? 0) members").foregroundStyle(Theme.secondary)
                        if let organization = store.organization { Text(organization.name).foregroundStyle(Theme.secondary) }
                    }
                    Section {
                        Text("Messages are end-to-end encrypted. You will see messages from when you join, not before.")
                        if preview.myState == "keying" { Text("A member's device will add you to the encryption.") }
                        if store.reviewAction == .unavailable, !store.mustReviewAgain {
                            if store.organization?.writable != true {
                                Text("This organization is read-only. You can open rooms you already belong to.")
                            } else {
                                Text(preview.myState == "banned" ? "You cannot join this room." : "An invitation is required to join this room.")
                            }
                        }
                    }
                    if store.reviewAction != .unavailable || opening != nil {
                        Section {
                            Button(actionLabel) {
                                let proof = store.reviewProof
                                Task {
                                    let result: OrganizationRoomOpening?
                                    if let opening { result = opening } else { result = await store.openReviewedRoom(proof: proof) }
                                    if let result, scope == store.context { opening = result; _ = await open(result) }
                                }
                            }
                            .disabled(store.busy || preparing)
                            .accessibilityIdentifier("org-room-join-open")
                        }
                    }
                }
                if store.reviewing || store.busy || preparing { ProgressView(preparing ? "Opening encrypted conversation…" : "Checking room…") }
                if let error = preparationProblem ?? store.error { Text(error).foregroundStyle(Theme.Pill.blockedText).accessibilityIdentifier("org-room-review-error") }
                Button("Check room again") { opening = nil; Task { await store.inspect(roomId) } }
                    .disabled(store.busy || preparing).accessibilityIdentifier("org-room-review-refresh")
            } else { Text("Your inbox changed. Return to the room directory.") }
        }
        .font(.geist(14)).tint(Theme.navy).sheetSurface()
        .accessibilityIdentifier("org-room-preview-form")
        .navigationTitle("Room").inlineTitle()
        .navigationBarBackButtonHidden(store.busy || preparing)
        .task(id: roomId) { await store.inspect(roomId) }
        .onDisappear { store.closeReview() }
    }

    private var actionLabel: String {
        if opening != nil || store.reviewAction == .open { return "Open room" }
        return store.reviewAction == .acceptInvitation ? "Accept invitation" : "Join room"
    }
}

private struct OrganizationRoomCreation: View {
    let store: OrganizationRoomStore
    let scope: OrganizationRoomScope
    let organizationId: String
    let preparing: Bool
    let preparationProblem: String?
    let open: (OrganizationRoomOpening) async -> Bool
    @State private var name = ""
    @State private var about = ""
    @State private var joinPolicy = OrganizationRoomJoinPolicy.members
    @State private var postPolicy = OrganizationRoomPostPolicy.everyone
    @State private var listed = true
    @State private var opening: OrganizationRoomOpening?
    @State private var discard = false

    var body: some View {
        Form {
            if scope == store.context, organizationId == store.organization?.id {
                if let organization = store.organization {
                    Section { Label(organization.name, systemImage: "building.2") }
                }
                Section {
                    TextField("Room name", text: $name).accessibilityIdentifier("org-room-name")
                    TextField("Description (optional)", text: $about, axis: .vertical).lineLimit(1...4)
                        .accessibilityIdentifier("org-room-description")
                } header: { Text("Room details") }
                    .disabled(store.busy || store.pendingCreation != nil || opening != nil || preparing)
                Section {
                    Picker("Who can join", selection: $joinPolicy) {
                        Text("Organization members").tag(OrganizationRoomJoinPolicy.members)
                        Text("Invited members").tag(OrganizationRoomJoinPolicy.invited)
                    }
                    Picker("Who can post", selection: $postPolicy) {
                        Text("Everyone").tag(OrganizationRoomPostPolicy.everyone)
                        Text("Only admins").tag(OrganizationRoomPostPolicy.admins)
                    }
                    Toggle("List in organization rooms", isOn: $listed)
                } footer: { Text("Messages are end-to-end encrypted. New members cannot read earlier messages.") }
                    .disabled(store.busy || store.pendingCreation != nil || opening != nil || preparing)
                if let opening {
                    Section {
                        Text("Room created").accessibilityIdentifier("org-room-created")
                        Button("Open room") { Task { _ = await open(opening) } }.disabled(preparing)
                            .accessibilityIdentifier("org-room-created-open")
                    }
                } else {
                    Section {
                        if let error = preparationProblem ?? store.error {
                            Text(error).foregroundStyle(Theme.Pill.blockedText)
                                .accessibilityIdentifier("org-room-create-error")
                        }
                        Button(store.pendingCreation == nil ? "Create room" : "Retry creation") {
                            Task {
                                if let result = await store.createRoom(in: scope, organizationId: organizationId, name: name, description: about, joinPolicy: joinPolicy, postPolicy: postPolicy, listed: listed), scope == store.context {
                                    opening = result; _ = await open(result)
                                }
                            }
                        }
                        .disabled(!store.canCreate || preparing || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || (store.retryAt.map { $0 > Date() } ?? false))
                        .accessibilityIdentifier("org-room-create")
                        if store.pendingCreation != nil {
                            Button("Start another room") { discard = true }.disabled(store.busy)
                                .accessibilityIdentifier("org-room-discard-attempt")
                        }
                    }
                }
                if store.busy || preparing { ProgressView(preparing ? "Opening encrypted conversation…" : "Creating room…") }
                if opening != nil, let error = preparationProblem ?? store.error {
                    Text(error).foregroundStyle(Theme.Pill.blockedText).accessibilityIdentifier("org-room-create-error")
                }
            } else { Text("Your inbox changed. Return to the room directory.") }
        }
        .font(.geist(14)).tint(Theme.navy).sheetSurface()
        .accessibilityIdentifier("org-room-create-form")
        .navigationTitle("New room").inlineTitle()
        .navigationBarBackButtonHidden(store.busy || preparing)
        .onAppear {
            if let pending = store.pendingCreation {
                name = pending.name; about = pending.description; joinPolicy = pending.joinPolicy; postPolicy = pending.postPolicy; listed = pending.listed
            }
        }
        .confirmationDialog("Start another room?", isPresented: $discard, titleVisibility: .visible) {
            Button("Start another room", role: .destructive) {
                guard scope == store.context, organizationId == store.organization?.id else { return }
                store.discardCreation(in: scope, organizationId: organizationId); name = ""; about = ""
            }
            Button("Cancel", role: .cancel) {}
        } message: { Text("The previous request may already have created a room. Check your rooms before starting another.") }
    }
}
