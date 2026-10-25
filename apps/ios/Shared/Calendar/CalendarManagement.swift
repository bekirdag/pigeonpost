import SwiftUI

struct CalendarInvitationsScreen: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    var joinMeeting: ((CallHost.ScheduledJoin) -> Void)?
    @State private var filtered = false
    @State private var selected: CalendarEventKey?

    var body: some View {
        NavigationStack {
            List {
                Picker("Invitations", selection: $filtered) { Text("Pending").tag(false); Text("Filtered").tag(true) }.pickerStyle(.segmented)
                if filtered {
                    Text("Filtered invitations stay off your agenda and send no reminders until you respond. A response does not add a contact.")
                        .font(.geist(12)).foregroundStyle(Theme.secondary)
                }
                if let error = store.lastError { Text(error).foregroundStyle(Color.red) }
                let items = filtered ? store.filteredInvitations : store.invitations
                if items.isEmpty { Text(filtered ? "No filtered invitations." : "No pending invitations.").foregroundStyle(Theme.secondary) }
                ForEach(items) { item in
                    Button { selected = .init(eventId: item.eventId, occurrenceId: item.occurrenceId) } label: {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(item.title ?? "Invitation").font(.geist(16, .semibold)).foregroundStyle(Theme.ink)
                            Text(item.allDay == true ? "All day · " + (item.startDate ?? "") : CalendarPresentation.instant(item.startsAt, zone: store.zone)).foregroundStyle(Theme.secondary)
                            Text(item.organizer?.name ?? item.organizer?.handle ?? item.organizer?.address ?? "Organizer").font(.geist(12)).foregroundStyle(Theme.secondary)
                        }.padding(.vertical, 6)
                    }.accessibilityIdentifier("calendar-invitation:" + item.id)
                }
            }
            .navigationTitle("Invitations")
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } } }
            .refreshable { await store.refresh() }
        }
        .font(.geist(14)).tint(Theme.navy)
        .sheet(item: $selected) { key in
            CalendarDetail(store: store, key: key, joinMeeting: joinMeeting)
                #if os(macOS)
                .frame(width: 620, height: 660)
                #endif
        }
    }
}

struct CalendarFoldersScreen: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    @State private var name = ""
    @State private var color = "blue"
    private let visit: UUID
    static let colors = ["blue", "green", "orange", "red", "purple", "teal", "gray"]

    init(store: CalendarStore) { self.store = store; visit = store.scopeId }

    var body: some View {
        NavigationStack {
            Form {
                if let error = store.lastError { Text(error).foregroundStyle(Color.red) }
                Section("Your calendars") {
                    if store.folders.isEmpty { Text("Your default calendar is created when you first save an event.").foregroundStyle(Theme.secondary) }
                    ForEach(store.folders) { folder in
                        NavigationLink {
                            CalendarFolderEditor(store: store, folder: folder)
                        } label: {
                            HStack {
                                Circle().fill(CalendarPresentation.color(folder.color)).frame(width: 10, height: 10)
                                Text(folder.name ?? "Calendar")
                                if folder.isDefault == true { Text("Default").font(.geist(12)).foregroundStyle(Theme.secondary) }
                            }
                        }
                    }
                }
                Section("New calendar") {
                    TextField("Name", text: $name).accessibilityIdentifier("calendar-folder-name")
                    Picker("Color", selection: $color) { ForEach(Self.colors, id: \.self) { Text($0.capitalized).tag($0) } }
                    Button(store.busy("create-folder") ? "Creating…" : "Create calendar") {
                        Task {
                            if await store.createFolder(name: name, color: color) != nil, store.scopeId == visit { name = "" }
                        }
                    }.disabled(store.busy("create-folder") || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || store.folders.count >= 20)
                }
            }
            .formStyle(.grouped).navigationTitle("Calendars")
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } } }
        }
        .font(.geist(14)).tint(Theme.navy)
    }
}

private struct CalendarFolderEditor: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    let folder: CalendarFolder
    @State private var name: String
    @State private var color: String
    @State private var makeDefault = false
    @State private var confirmDelete = false
    private let visit: UUID

    init(store: CalendarStore, folder: CalendarFolder) {
        self.store = store; self.folder = folder; visit = store.scopeId
        _name = State(initialValue: folder.name ?? ""); _color = State(initialValue: folder.color ?? "blue")
    }
    var body: some View {
        Form {
            if let error = store.lastError { Text(error).foregroundStyle(Color.red) }
            TextField("Name", text: $name)
            Picker("Color", selection: $color) { ForEach(CalendarFoldersScreen.colors, id: \.self) { Text($0.capitalized).tag($0) } }
            if folder.isDefault != true { Toggle("Make this the default calendar", isOn: $makeDefault) }
            Button("Save changes") {
                Task {
                    guard store.scopeId == visit, let revision = folder.revision else { return }
                    var fields: [String: Any] = ["name": name, "color": color]
                    if makeDefault { fields["is_default"] = true }
                    if await store.updateFolder(id: folder.id, revision: revision, fields: fields) { dismiss() }
                }
            }.disabled(store.busy("folder:" + folder.id) || folder.revision == nil || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || store.revisionConflict)
            Section {
                Button("Delete calendar", role: .destructive) { confirmDelete = true }.disabled(store.busy("folder:" + folder.id) || folder.revision == nil)
            }
        }
        .formStyle(.grouped).navigationTitle(folder.name ?? "Calendar")
        .confirmationDialog("Delete this calendar?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete calendar", role: .destructive) {
                Task {
                    guard store.scopeId == visit, let revision = folder.revision else { return }
                    if await store.deleteFolder(id: folder.id, revision: revision) { dismiss() }
                }
            }
            Button("Keep calendar", role: .cancel) {}
        } message: {
            Text("Events you organize in it will be cancelled and attendees notified. Received invitations will move to your default calendar.")
        }
        .onAppear { store.clearError() }
    }
}

struct CalendarSettingsScreen: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    @State private var previews: Bool
    @State private var timezone: String
    @State private var weekStart: Int
    @State private var hoursStart: String
    @State private var hoursEnd: String
    @State private var days: Set<Int>
    @State private var problem: String?
    private let original: CalendarSettings
    private let visit: UUID

    init(store: CalendarStore) {
        self.store = store; original = store.settings; visit = store.scopeId
        _previews = State(initialValue: original.previews ?? false)
        _timezone = State(initialValue: original.timezone ?? "")
        _weekStart = State(initialValue: original.weekStart ?? 1)
        _hoursStart = State(initialValue: original.workingHours?.start ?? "09:00")
        _hoursEnd = State(initialValue: original.workingHours?.end ?? "18:00")
        _days = State(initialValue: Set(original.workingHours?.days ?? [1, 2, 3, 4, 5]))
    }

    var body: some View {
        NavigationStack {
            Form {
                if let error = problem ?? store.lastError { Text(error).foregroundStyle(Color.red) }
                Section("Privacy") {
                    Toggle("Event titles in reminder notifications", isOn: $previews).accessibilityIdentifier("calendar-notification-previews")
                    Text("When off, notifications say “Event starting soon”. Calendar data is readable by the service.").font(.geist(12)).foregroundStyle(Theme.secondary)
                }
                Section("Dates and time") {
                    TextField("Timezone (empty uses this device)", text: $timezone).autocorrectionDisabled()
                    Text("Device timezone: \(TimeZone.current.identifier)").font(.geist(12)).foregroundStyle(Theme.secondary)
                    Picker("Week starts on", selection: $weekStart) {
                        ForEach(0..<7) { Text(CalendarPresentation.weekday($0)).tag($0) }
                    }
                }
                Section("Working hours") {
                    TextField("Start, HH:mm", text: $hoursStart).accessibilityIdentifier("calendar-working-start")
                    TextField("End, HH:mm", text: $hoursEnd)
                    ForEach(0..<7) { index in
                        Toggle(CalendarPresentation.weekday(index), isOn: Binding(get: { days.contains(index) }, set: { if $0 { days.insert(index) } else { days.remove(index) } }))
                    }
                }
            }
            .formStyle(.grouped).navigationTitle("Calendar settings")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() }.disabled(store.busy("settings")) }
                ToolbarItem(placement: .confirmationAction) { Button("Save") { Task { await save() } }.disabled(store.busy("settings")) }
            }
        }
        .font(.geist(14)).tint(Theme.navy)
        .onAppear { store.clearError() }
    }
    private func save() async {
        guard store.scopeId == visit else { return }
        problem = nil
        let zone = timezone.trimmingCharacters(in: .whitespacesAndNewlines)
        guard zone.isEmpty || TimeZone(identifier: zone) != nil else { problem = "Choose a known timezone, for example Europe/Istanbul."; return }
        do {
            guard hoursStart.utf8.count == 5, hoursEnd.utf8.count == 5 else { throw CalendarTimeError.invalidFormat }
            _ = try CalendarWallTime.range(start: "2026-10-25T" + hoursStart, end: "2026-10-25T" + hoursEnd, tzid: "UTC")
        } catch { problem = "Use working hours in HH:mm format, with the end after the start."; return }
        var fields: [String: Any] = [:]
        if previews != (original.previews ?? false) { fields["previews"] = previews }
        if zone != (original.timezone ?? "") { fields["timezone"] = zone.isEmpty ? NSNull() : zone as Any }
        if weekStart != (original.weekStart ?? 1) { fields["week_start"] = weekStart }
        if hoursStart != (original.workingHours?.start ?? "09:00") || hoursEnd != (original.workingHours?.end ?? "18:00") || days != Set(original.workingHours?.days ?? [1, 2, 3, 4, 5]) {
            fields["working_hours"] = ["start": hoursStart, "end": hoursEnd, "days": days.sorted()]
        }
        if fields.isEmpty { dismiss(); return }
        if await store.saveSettings(fields: fields) { dismiss() }
    }
}
