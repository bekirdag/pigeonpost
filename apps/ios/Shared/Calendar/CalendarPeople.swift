import SwiftUI

struct CalendarManagementTarget: Identifiable {
    enum Kind { case attendees, group, placement }
    let id = UUID()
    let kind: Kind
    let response: CalendarEventResponse
    let key: CalendarEventKey
}

/// SwiftUI can read a row binding while removing its controls. Resolve by stable identity so a
/// removed/reloaded row never indexes a shortened array or writes into the next participant.
struct CalendarInviteeFields: View {
    @Binding private var person: CalendarInviteeDraft
    let discardTitle: String
    let destinationID: String
    let discard: () -> Void

    init(_ snapshot: CalendarInviteeDraft, in drafts: Binding<[CalendarInviteeDraft]>,
         discardTitle: String = "Remove", destinationID: String = "calendar-new-attendee-address", discard: @escaping () -> Void) {
        self.discardTitle = discardTitle; self.destinationID = destinationID; self.discard = discard
        _person = Binding(get: { drafts.wrappedValue.first { $0.id == snapshot.id } ?? snapshot }, set: { changed in
            guard let index = drafts.wrappedValue.firstIndex(where: { $0.id == snapshot.id }) else { return }
            drafts.wrappedValue[index] = changed
        })
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Pigeonpost handle or contact address", text: $person.destination).autocorrectionDisabled()
                .accessibilityIdentifier(destinationID)
            TextField("Name (optional)", text: $person.name)
            Toggle("Optional attendee", isOn: $person.optional)
            if person.destination.trimmingCharacters(in: .whitespacesAndNewlines).hasPrefix("/") { Toggle("Co-host", isOn: $person.cohost) }
            Button(discardTitle, role: .destructive, action: discard).buttonStyle(.borderless)
        }
    }
}

/// The participant list and expected revision stay fixed until the person explicitly reloads.
struct CalendarAttendeesScreen: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    let key: CalendarEventKey
    @State private var snapshot: CalendarEventResponse
    @State private var scope = CalendarAnswerScope.all
    @State private var removing: Set<String> = []
    @State private var adding: [CalendarInviteeDraft] = []
    @State private var problem: String?
    @State private var confirmReload = false
    private let visit: UUID

    init(store: CalendarStore, target: CalendarManagementTarget) {
        self.store = store; key = target.key; visit = store.scopeId
        _snapshot = State(initialValue: target.response)
    }

    private var busy: Bool { store.busy("event:" + key.eventId) }
    private var recurring: Bool { snapshot.event.rrule?.isEmpty == false }
    var body: some View {
        NavigationStack {
            Form {
                if let error = problem ?? store.lastError { Text(error).foregroundStyle(Color.red).accessibilityIdentifier("calendar-people-error") }
                if store.revisionConflict {
                    Button("Reload current participants") { confirmReload = true }.accessibilityIdentifier("calendar-people-reload")
                }
                if recurring {
                    Section("Apply changes to") {
                        if key.occurrenceId != nil {
                            Picker("Scope", selection: $scope) {
                                Text("Whole series").tag(CalendarAnswerScope.all)
                                Text("Only this occurrence").tag(CalendarAnswerScope.this)
                            }.accessibilityIdentifier("calendar-people-scope")
                        } else { Text("Whole series") }
                        Text("The list below shows series participants and their series responses. It does not show individual occurrence changes.")
                            .font(.geist(12)).foregroundStyle(Theme.secondary)
                    }
                }
                Section(recurring ? "Series participants" : "Participants") {
                    ForEach((snapshot.event.attendees ?? []).filter { $0.invited != false }) { person in
                        HStack {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(person.name ?? person.handle ?? person.contact ?? "Guest")
                                Text(CalendarPresentation.rsvp(person.rsvp)).font(.geist(12)).foregroundStyle(Theme.secondary)
                            }
                            Spacer()
                            Button(removing.contains(person.id) ? "Keep" : "Remove") {
                                if removing.contains(person.id) { removing.remove(person.id) } else { removing.insert(person.id) }
                            }.buttonStyle(.bordered).tint(removing.contains(person.id) ? Theme.navy : Color.red)
                                .accessibilityIdentifier("calendar-remove-attendee:" + person.id)
                        }
                    }
                    Text("Participant changes apply to the scope you choose. Invitation permissions are checked again when joining a call.").font(.geist(12)).foregroundStyle(Theme.secondary)
                }
                Section("Invite people") {
                    ForEach(adding) { person in
                        CalendarInviteeFields(person, in: $adding, discardTitle: "Discard new participant") {
                            adding.removeAll { $0.id == person.id }
                        }
                    }
                    Button("Add a person") { adding.append(CalendarInviteeDraft()) }.disabled(adding.count >= 100)
                        .accessibilityIdentifier("calendar-add-person")
                    Text("Inviting someone does not add a contact or add them to a linked group.").font(.geist(12)).foregroundStyle(Theme.secondary)
                    if !store.emailEnabled {
                        Text("For contact guests, copy their personal invitation links from the event after saving.")
                            .font(.geist(12)).foregroundStyle(Theme.secondary)
                    }
                }
            }
            .accessibilityIdentifier("calendar-people-form")
            .disabled(busy).formStyle(.grouped).navigationTitle("Manage participants")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() }.disabled(busy) }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { Task { await save() } }
                        .disabled(busy || store.revisionConflict || snapshot.event.revision == nil || (adding.isEmpty && removing.isEmpty))
                        .accessibilityIdentifier("calendar-save-people")
                }
            }
        }
        .font(.geist(14)).tint(Theme.navy).interactiveDismissDisabled(busy)
        .onChange(of: store.scopeId) { _, _ in dismiss() }
        .confirmationDialog("Reload and discard your participant changes?", isPresented: $confirmReload, titleVisibility: .visible) {
            Button("Reload participants", role: .destructive) {
                Task {
                    guard store.scopeId == visit, let fresh = await store.loadEvent(key), store.scopeId == visit else { return }
                    snapshot = fresh; removing = []; adding = []; problem = nil; store.clearError()
                }
            }.accessibilityIdentifier("calendar-confirm-people-reload")
            Button("Keep changes", role: .cancel) {}
        }
    }

    private func save() async {
        guard store.scopeId == visit, !store.revisionConflict, let revision = snapshot.event.revision else { return }
        do {
            let invitees = try adding.compactMap { try $0.fields() }
            guard !invitees.isEmpty || !removing.isEmpty else { problem = "Enter a participant or choose someone to remove."; return }
            problem = nil
            if await store.changeAttendees(key: key, revision: revision, scope: scope, adding: invitees, removing: removing.sorted()), store.scopeId == visit {
                _ = await store.loadEvent(key)
                if store.scopeId == visit { dismiss() }
            }
        } catch { problem = error.localizedDescription }
    }
}

struct CalendarLinkedGroupScreen: View {
    @Environment(GroupStore.self) private var groups
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    let target: CalendarManagementTarget
    @State private var selected: String
    private let visit: UUID

    init(store: CalendarStore, target: CalendarManagementTarget) {
        self.store = store; self.target = target; visit = store.scopeId
        _selected = State(initialValue: target.response.event.linkedGroup ?? "")
    }
    private var available: [GroupSummary] { groups.groups.filter { $0.isActive && $0.deletedAt == nil } }
    var body: some View {
        NavigationStack {
            Form {
                if let error = store.lastError { Text(error).foregroundStyle(Color.red) }
                Picker("Group", selection: $selected) {
                    Text("No linked group").tag("")
                    ForEach(available) { Text($0.name).tag($0.id) }
                    if !selected.isEmpty && !available.contains(where: { $0.id == selected }) { Text("Previously linked group unavailable").tag(selected) }
                }.accessibilityIdentifier("calendar-linked-group-picker")
                Text("Choose a group you have joined. Linking does not invite event participants into the group or group members into the event.")
                    .font(.geist(12)).foregroundStyle(Theme.secondary)
                Text("Unlinking leaves the group and its messages in place.").font(.geist(12)).foregroundStyle(Theme.secondary)
            }
            .formStyle(.grouped).navigationTitle("Meeting group")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        Task {
                            guard store.scopeId == visit, let revision = target.response.event.revision else { return }
                            if await store.linkGroup(id: target.key.eventId, revision: revision, group: selected.isEmpty ? nil : selected), store.scopeId == visit {
                                _ = await store.loadEvent(target.key)
                                if store.scopeId == visit { dismiss() }
                            }
                        }
                    }.disabled(store.busy("event:" + target.key.eventId) || store.revisionConflict || target.response.event.revision == nil
                        || (!selected.isEmpty && !available.contains { $0.id == selected }))
                        .accessibilityIdentifier("calendar-save-linked-group")
                }
            }
        }
        .font(.geist(14)).tint(Theme.navy).interactiveDismissDisabled(store.busy("event:" + target.key.eventId))
        .onChange(of: store.scopeId) { _, _ in dismiss() }
    }
}

struct CalendarPlacementScreen: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    let target: CalendarManagementTarget
    @State private var selected: String
    private let visit: UUID

    init(store: CalendarStore, target: CalendarManagementTarget) {
        self.store = store; self.target = target; visit = store.scopeId
        _selected = State(initialValue: target.response.event.my?.calendarId ?? store.folders.first(where: { $0.isDefault == true })?.id ?? store.folders.first?.id ?? "")
    }
    var body: some View {
        NavigationStack {
            Form {
                if let error = store.lastError { Text(error).foregroundStyle(Color.red) }
                Picker("Your calendar", selection: $selected) { ForEach(store.folders) { Text($0.name ?? "Calendar").tag($0.id) } }
                    .accessibilityIdentifier("calendar-placement-picker")
                Text("This changes where the invitation appears for you. It does not move the organizer's event or change your response.")
                    .font(.geist(12)).foregroundStyle(Theme.secondary)
            }
            .formStyle(.grouped).navigationTitle("Move invitation")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Move") {
                        Task {
                            guard store.scopeId == visit else { return }
                            if await store.placeInvitation(id: target.key.eventId, calendar: selected), store.scopeId == visit {
                                _ = await store.loadEvent(target.key)
                                if store.scopeId == visit { dismiss() }
                            }
                        }
                    }.disabled(!store.folders.contains { $0.id == selected } || store.busy("placement:" + target.key.eventId))
                        .accessibilityIdentifier("calendar-save-placement")
                }
            }
        }
        .font(.geist(14)).tint(Theme.navy).interactiveDismissDisabled(store.busy("placement:" + target.key.eventId))
        .onChange(of: store.scopeId) { _, _ in dismiss() }
    }
}
