import SwiftUI
import UniformTypeIdentifiers

struct CalendarFileDocument: FileDocument {
    static let contentType = UTType(filenameExtension: "ics") ?? .calendarEvent
    static var readableContentTypes: [UTType] { [contentType] }
    let data: Data
    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws {
        guard let data = configuration.file.regularFileContents else { throw CalendarImportProblem.file }
        self.data = data
    }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}

struct CalendarImportScreen: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    @State private var choosing = false
    @State private var destination = ""
    @State private var preview: CalendarImportPreview?
    @State private var selected: Set<Int> = []
    @State private var confirmation: CalendarImportConfirmation?
    @State private var result: CalendarImportResult?
    @State private var problem: String?
    @State private var leaveUnconfirmed = false
    @State private var presentation = UUID()
    @State private var visible = true
    private let visit: UUID

    init(store: CalendarStore) { self.store = store; visit = store.scopeId }
    private var busy: Bool { store.busy("import-preview") || confirmation.map { store.busy("import-confirm:" + $0.draft) } == true }
    var body: some View {
        NavigationStack {
            Form {
                if let error = problem ?? store.lastError { Text(error).foregroundStyle(Color.red).accessibilityIdentifier("calendar-import-error") }
                if let result {
                    Section {
                        Label("Imported \(result.created?.count ?? 0) events", systemImage: "checkmark.circle")
                            .accessibilityIdentifier("calendar-import-complete")
                        Text("These are private copies. No invitations were sent.")
                        Button("Import another file") { reset() }
                    }
                } else if let preview {
                    Section("Import into") {
                        Text(store.folders.first { $0.id == preview.calendarId }?.name ?? "Your calendar")
                        if let expires = preview.expiresAt, confirmation == nil {
                            Text("Preview expires " + CalendarPresentation.instant(expires, zone: store.zone)).font(.geist(12)).foregroundStyle(Theme.secondary)
                        }
                    }
                    Section("Choose events") {
                        ForEach(preview.events ?? []) { event in
                            VStack(alignment: .leading, spacing: 5) {
                                Toggle(isOn: Binding(get: { selected.contains(event.index) }, set: { included in
                                    if included { selected.insert(event.index) } else { selected.remove(event.index) }
                                })) {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(event.title ?? "Untitled event")
                                        Text(event.allDay == true ? (event.startDate ?? "All day") : (event.start ?? ""))
                                            .font(.geist(12)).foregroundStyle(Theme.secondary)
                                    }
                                }.disabled(event.importable != true || confirmation != nil)
                                    .accessibilityIdentifier("calendar-import-entry:" + String(event.index))
                                if event.importable != true { Text("Cannot import this event").font(.geist(12)).foregroundStyle(Theme.secondary) }
                                ForEach(Array((event.warnings ?? []).enumerated()), id: \.offset) { _, warning in
                                    Text(warning).font(.geist(12)).foregroundStyle(Theme.secondary)
                                }
                            }
                        }
                    }
                    if let warnings = preview.warnings, !warnings.isEmpty {
                        Section("File warnings") { ForEach(Array(warnings.enumerated()), id: \.offset) { _, warning in Text(warning) } }
                    }
                    Section {
                        Text("Imports create private copies. Guests, organizers and alarms from the file are not imported. Remote attachments and links are not fetched.")
                            .font(.geist(12)).foregroundStyle(Theme.secondary)
                        if confirmation != nil {
                            Text("Confirmation is not yet acknowledged. Retry keeps the same selection and calendar. Check your calendar before importing this file as a new draft.")
                                .font(.geist(12)).accessibilityIdentifier("calendar-import-unconfirmed")
                        }
                        Button(busy ? "Importing…" : confirmation == nil ? "Import selected events" : "Retry confirmation") { Task { await confirm() } }
                            .disabled(busy || selected.isEmpty || (confirmation == nil && (preview.expiresAt ?? Int.max) <= Int(Date().timeIntervalSince1970)))
                            .accessibilityIdentifier("calendar-import-confirm")
                        if confirmation == nil { Button("Choose a different file") { reset() } }
                    }
                } else {
                    Section {
                        Picker("Calendar", selection: $destination) {
                            Text("Default calendar").tag("")
                            ForEach(store.folders) { Text($0.name ?? "Calendar").tag($0.id) }
                        }
                        Text("Choose an .ics file, up to 512 KB. Review its events before importing anything.")
                            .font(.geist(12)).foregroundStyle(Theme.secondary)
                        Button(busy ? "Reading calendar…" : "Choose calendar file") {
                            choosing = true
                            #if DEBUG
                            CalendarPickerTrace.record("import.choose", choosing: choosing, visible: visible, current: store.scopeId == visit)
                            #endif
                        }.disabled(busy)
                            .accessibilityIdentifier("calendar-import-choose-file")
                    }
                }
            }
            .accessibilityIdentifier("calendar-import-form")
            .disabled(busy).formStyle(.grouped).navigationTitle("Import calendar")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") {
                        if confirmation != nil && result == nil { leaveUnconfirmed = true } else { dismiss() }
                    }.disabled(busy)
                }
            }
        }
        .font(.geist(14)).tint(Theme.navy).interactiveDismissDisabled(busy || (confirmation != nil && result == nil))
        #if DEBUG
        .task {
            // Exercise preview/confirmation UI through the reserved fixture transport. The file
            // reader and real postbox import are qualified separately; this does not test a picker.
            if Fixtures.enabled, CommandLine.arguments.contains("-calendar-import-preview-fixture"), preview == nil {
                let loaded = await store.previewImport(data: Data("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".utf8), calendar: nil)
                guard store.scopeId == visit, !Task.isCancelled else { return }
                preview = loaded; selected = Set((loaded?.events ?? []).filter { $0.importable == true }.map(\.index))
            }
        }
        #endif
        .onAppear {
            visible = true
            #if DEBUG
            CalendarPickerTrace.record("import.appear", choosing: choosing, visible: visible, current: store.scopeId == visit)
            #endif
        }
        .onChange(of: store.scopeId) { _, _ in presentation = UUID(); reset(); dismiss() }
        .onDisappear {
            #if DEBUG
            CalendarPickerTrace.record("import.disappear", choosing: choosing, visible: visible, current: store.scopeId == visit)
            #endif
            visible = false; presentation = UUID(); reset()
        }
        .fileImporter(isPresented: $choosing, allowedContentTypes: [CalendarFileDocument.contentType], allowsMultipleSelection: false) { choice in
            #if DEBUG
            CalendarPickerTrace.record("import.callback", choosing: choosing, visible: visible, current: store.scopeId == visit)
            #endif
            Task {
                #if DEBUG
                CalendarPickerTrace.record("import.task", choosing: choosing, visible: visible, current: store.scopeId == visit)
                #endif
                guard store.scopeId == visit, visible else { return }
                let screen = presentation
                do {
                    guard let url = try choice.get().first else { return }
                    let data = try CalendarImportFile.read(url)
                    #if DEBUG
                    CalendarPickerTrace.record("import.read", choosing: choosing, visible: visible, current: store.scopeId == visit)
                    #endif
                    problem = nil; store.clearError()
                    let loaded = await store.previewImport(data: data, calendar: destination.isEmpty ? nil : destination)
                    #if DEBUG
                    CalendarPickerTrace.record("import.response", choosing: choosing, visible: visible, current: store.scopeId == visit)
                    #endif
                    guard store.scopeId == visit, visible, screen == presentation, !Task.isCancelled else { return }
                    preview = loaded
                    selected = Set((loaded?.events ?? []).filter { $0.importable == true }.map(\.index))
                } catch let error as CocoaError where error.code == .userCancelled { }
                catch { if store.scopeId == visit { problem = error.localizedDescription } }
            }
        }
        .confirmationDialog("Leave this unconfirmed import?", isPresented: $leaveUnconfirmed, titleVisibility: .visible) {
            Button("Leave import", role: .destructive) { dismiss() }
            Button("Keep reviewing", role: .cancel) {}
        } message: { Text("It may have completed. Check your calendar before importing this file again.") }
    }

    private func reset() { preview = nil; selected = []; confirmation = nil; result = nil; problem = nil }
    private func confirm() async {
        guard store.scopeId == visit, visible, let preview else { return }
        let screen = presentation
        do {
            let attempt = try confirmation ?? CalendarImportConfirmation(preview: preview, selected: selected)
            confirmation = attempt; problem = nil
            let created = await store.confirmImport(draft: attempt.draft, selected: attempt.selected, calendar: attempt.calendar)
            guard store.scopeId == visit, visible, screen == presentation, !Task.isCancelled else { return }
            result = created
        } catch { problem = error.localizedDescription }
    }
}

#if DEBUG
@MainActor enum CalendarPickerTrace {
    static func record(_ stage: String, choosing: Bool = false, visible: Bool = true, current: Bool = true) {
        guard Fixtures.enabled, CommandLine.arguments.contains("-calendar-picker-trace") else { return }
        let key = "pp_calendar_picker_trace"
        var rows = UserDefaults.standard.stringArray(forKey: key) ?? []
        rows.append(stage + " choosing=" + String(choosing) + " visible=" + String(visible) + " current=" + String(current))
        UserDefaults.standard.set(Array(rows.suffix(96)), forKey: key)
        UserDefaults.standard.synchronize()
    }
}
#endif

struct CalendarFeedScreen: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    let folder: CalendarFolder
    @State private var status: CalendarFeed?
    @State private var projection = "busy"
    @State private var issuedURL: String?
    @State private var confirmRotation = false
    @State private var confirmRevocation = false
    @State private var copied = false
    @State private var presentation = UUID()
    @State private var visible = true
    private let visit: UUID

    init(store: CalendarStore, folder: CalendarFolder) { self.store = store; self.folder = folder; visit = store.scopeId }
    private var busy: Bool { store.busy("feed:" + folder.id) }
    var body: some View {
        Form {
            if let error = store.lastError { Text(error).foregroundStyle(Color.red) }
            Section(folder.name ?? "Calendar") {
                if let status {
                    Label(status.enabled == true ? "Subscription enabled" : "Subscription off", systemImage: status.enabled == true ? "link" : "link.badge.plus")
                    if status.enabled == true, let expires = status.expiresAt {
                        Text("Expires " + CalendarPresentation.instant(expires, zone: store.zone)).font(.geist(12)).foregroundStyle(Theme.secondary)
                    }
                } else {
                    Button("Load subscription status") { Task { await load() } }.disabled(busy)
                }
                Picker("Include", selection: $projection) {
                    Text("Busy times only").tag("busy")
                    Text("My event titles and locations").tag("personal_redacted")
                }.accessibilityIdentifier("calendar-feed-projection")
                Text(projection == "busy" ? "All events appear as Busy. Titles, locations, descriptions and attendees stay hidden."
                     : "Shows titles and locations of your own non-private events. Invitations and private events appear as Busy. Descriptions, attendees and call links stay hidden.")
                    .font(.geist(12)).foregroundStyle(Theme.secondary)
            }
            if let issuedURL {
                Section("Your new subscription link") {
                    Text(issuedURL).font(.geistMono(12)).textSelection(.enabled).privacySensitive()
                        .accessibilityIdentifier("calendar-feed-issued-link")
                    Button(copied ? "Copied" : "Copy subscription link") { Clipboard.copy(issuedURL); copied = true }
                        .accessibilityIdentifier("calendar-feed-copy")
                    Text("Shown only here, once. Save it in your calendar app before leaving this screen.")
                        .font(.geist(12)).foregroundStyle(Theme.secondary)
                }
            }
            Section {
                Text("Anyone with this link can read the selected calendar information. Keep it private. Links expire after one year; replacing or disabling a link stops the old one from working.")
                    .font(.geist(12)).foregroundStyle(Theme.secondary)
                Button(status?.enabled == true ? "Replace subscription link" : "Create subscription link") {
                    if status?.enabled == true { confirmRotation = true } else { Task { await change(enabled: true) } }
                }.disabled(status == nil || busy).accessibilityIdentifier("calendar-feed-create")
                if status?.enabled == true {
                    Button("Disable subscription", role: .destructive) { confirmRevocation = true }.disabled(busy)
                        .accessibilityIdentifier("calendar-feed-disable")
                }
                Text("Changing what is included takes effect when you create or replace the link.").font(.geist(12)).foregroundStyle(Theme.secondary)
            }
        }
        .accessibilityIdentifier("calendar-feed-form")
        .formStyle(.grouped).navigationTitle("Calendar subscription").font(.geist(14)).tint(Theme.navy)
        .task { await load() }
        .onAppear { visible = true }
        .onDisappear { visible = false; presentation = UUID(); issuedURL = nil; copied = false }
        .onChange(of: store.scopeId) { _, _ in presentation = UUID(); issuedURL = nil; status = nil; dismiss() }
        .confirmationDialog("Replace this subscription link?", isPresented: $confirmRotation, titleVisibility: .visible) {
            Button("Replace link", role: .destructive) { Task { await change(enabled: true) } }
            Button("Keep link", role: .cancel) {}
        } message: { Text("The current link will stop working. Update every calendar app that uses it with the new link.") }
        .confirmationDialog("Disable this subscription?", isPresented: $confirmRevocation, titleVisibility: .visible) {
            Button("Disable subscription", role: .destructive) { Task { await change(enabled: false) } }
            Button("Keep subscription", role: .cancel) {}
        } message: { Text("Calendar apps will no longer be able to refresh using this link. Copies they already downloaded may remain there.") }
    }

    private func load() async {
        guard store.scopeId == visit, visible else { return }
        let screen = presentation
        let loaded = await store.feedStatus(id: folder.id)
        guard store.scopeId == visit, visible, screen == presentation, !Task.isCancelled else { return }
        status = loaded
        if let value = loaded?.projection, ["busy", "personal_redacted"].contains(value) { projection = value }
    }
    private func change(enabled: Bool) async {
        guard store.scopeId == visit, visible else { return }
        let screen = presentation
        issuedURL = nil; copied = false
        let changed = await store.changeFeed(id: folder.id, enabled: enabled, projection: projection)
        guard store.scopeId == visit, visible, screen == presentation, !Task.isCancelled else { return }
        if let changed {
            issuedURL = enabled ? changed.url : nil
            status = CalendarFeed(enabled: enabled, projection: projection, createdAt: changed.createdAt, expiresAt: changed.expiresAt)
        }
    }
}
