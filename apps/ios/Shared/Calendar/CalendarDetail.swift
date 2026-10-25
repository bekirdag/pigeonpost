import SwiftUI
import UniformTypeIdentifiers

struct CalendarDetail: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    var joinMeeting: ((CallHost.ScheduledJoin) -> Void)?
    @State private var key: CalendarEventKey
    @State private var edit: CalendarEditTarget?
    @State private var management: CalendarManagementTarget?
    @State private var exportDocument: CalendarFileDocument?
    @State private var exporting = false
    @State private var presentation = UUID()
    @State private var visible = true
    @State private var cancellation: Cancellation?
    @State private var answerScope = CalendarAnswerScope.all
    @State private var answerNote = ""
    @State private var cancellationNote = ""
    @State private var reminderText = ""
    @State private var problem: String?
    @State private var notice: String?
    private let visit: UUID
    private struct Cancellation {
        let key: CalendarEventKey
        let revision: Int
        let scope: CalendarEventScope
    }

    init(store: CalendarStore, key: CalendarEventKey, joinMeeting: ((CallHost.ScheduledJoin) -> Void)? = nil) {
        self.store = store; self.joinMeeting = joinMeeting; visit = store.scopeId; _key = State(initialValue: key)
    }

    private var response: CalendarEventResponse? { store.scopeId == visit ? store.details[key] : nil }
    private var busy: Bool { store.busy("event:" + key.eventId) || store.busy("answer:" + key.eventId) }
    private var recurring: Bool { response?.event.rrule?.isEmpty == false }

    var body: some View {
        NavigationStack {
            Group {
                if let response { content(response) }
                else if let error = store.lastError { ContentUnavailableView(error, systemImage: "calendar.badge.exclamationmark") }
                else { ProgressView("Loading event…") }
            }
            .navigationTitle("Event")
            .safeAreaInset(edge: .top, spacing: 0) {
                if problem != nil || store.lastError != nil || notice != nil {
                    VStack(alignment: .leading, spacing: 8) {
                        if let message = problem ?? store.lastError { Text(message).foregroundStyle(Color.red).accessibilityIdentifier("calendar-detail-error") }
                        if let notice { Text(notice).foregroundStyle(Theme.secondary) }
                        if store.revisionConflict {
                            Button("Load current event") {
                                Task { store.clearError(); _ = await store.loadEvent(key) }
                            }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading).padding(16).background(Theme.ground)
                }
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .primaryAction) {
                    if let response, response.event.canEdit == true, response.event.status != "cancelled" {
                        Menu("Edit") {
                            if recurring, key.occurrenceId != nil {
                                Button("Only this occurrence") { openEditor(response, .this) }
                                Button("This and following occurrences") { openEditor(response, .following) }
                            }
                            Button(recurring ? "Whole series" : "Edit event") { openEditor(response, .all) }
                        }.disabled(busy).accessibilityIdentifier("calendar-edit")
                    }
                }
            }
        }
        .font(.geist(14)).tint(Theme.navy)
        .task(id: key) {
            guard store.scopeId == visit else { return }
            store.openEvent = key
            if let loaded = await store.loadEvent(key), store.scopeId == visit {
                reminderText = (loaded.event.reminders ?? []).map { String($0.minutesBefore) }.joined(separator: ", ")
            }
        }
        .onAppear { visible = true }
        .onDisappear {
            visible = false; presentation = UUID(); exportDocument = nil; exporting = false
            if store.openEvent == key { store.openEvent = nil }
        }
        .onChange(of: store.scopeId) { _, _ in management = nil; edit = nil; exportDocument = nil; exporting = false }
        .sheet(item: $edit) { target in
            if let draft = try? CalendarEventDraft(target: target, fallbackZone: store.zone) {
                CalendarEditor(store: store, draft: draft) { updated in
                    edit = nil
                    if updated.event.id != key.eventId {
                        key = CalendarEventKey(eventId: updated.event.id)
                        notice = "Following occurrences now belong to a new series."
                    }
                    let dropped = (updated.dropped ?? [:]).values.reduce(0, +)
                    if dropped > 0 { notice = "The recurrence change dropped \(dropped) exceptions or occurrence responses. Review the updated series." }
                }
                #if os(macOS)
                .frame(width: 620, height: 660)
                #endif
            }
        }
        .sheet(item: $management) { target in
            Group {
                switch target.kind {
                case .attendees: CalendarAttendeesScreen(store: store, target: target)
                case .group: CalendarLinkedGroupScreen(store: store, target: target)
                case .placement: CalendarPlacementScreen(store: store, target: target)
                }
            }
            .id(visit)
            #if os(macOS)
            .frame(width: 620, height: 660)
            #endif
        }
        .fileExporter(isPresented: $exporting, document: exportDocument, contentType: .calendarEvent, defaultFilename: "Pigeonpost-event") { saved in
            exportDocument = nil
            if case .failure(let error) = saved, (error as? CocoaError)?.code != .userCancelled, store.scopeId == visit {
                problem = "Could not save the calendar file."
            }
        }
        .confirmationDialog("Cancel this event?", isPresented: Binding(get: { cancellation != nil }, set: { if !$0 { cancellation = nil } }), presenting: cancellation) { target in
            Button(cancelTitle(target.scope), role: .destructive) { Task { await cancel(target) } }
            Button("Keep event", role: .cancel) { cancellation = nil }
        } message: { target in
            Text(cancelTitle(target.scope) + ". Attendees will be notified. This cannot be undone.")
        }
    }

    private func content(_ response: CalendarEventResponse) -> some View {
        let event = response.event, occurrence = response.occurrence
        return List {
            Section {
                Text(occurrence?.title ?? event.title ?? "Event").font(.geist(23, .bold)).textSelection(.enabled)
                if event.status == "cancelled" || occurrence?.status == "cancelled" { Label("Cancelled", systemImage: "xmark.circle").foregroundStyle(Color.red) }
                if occurrence?.allDay ?? event.allDay ?? false {
                    Text("All day · " + (occurrence?.startDate ?? event.startDate ?? ""))
                    if let end = occurrence?.endDate ?? event.endDate, let day = try? CalendarDay(end) { Text("Through " + CalendarPresentation.day(day.adding(days: -1))) }
                } else {
                    Text(CalendarPresentation.instant(occurrence?.startsAt ?? event.startsAt, zone: store.zone) + " – " + CalendarPresentation.instant(occurrence?.endsAt ?? event.endsAt, zone: store.zone))
                    Text("Shown in \(store.zone.identifier) · Event timezone: \(occurrence?.tzid ?? event.tzid ?? "Unknown")").font(.geist(12)).foregroundStyle(Theme.secondary)
                }
                if let rule = event.rrule { Label(rule, systemImage: "repeat").font(.geist(12)) }
                if let location = occurrence?.location ?? event.location, !location.isEmpty { Label(location, systemImage: "mappin.and.ellipse").textSelection(.enabled) }
                if let description = occurrence?.description ?? event.description, !description.isEmpty { Text(description).textSelection(.enabled) }
                if let organizer = event.organizer { Text("Organized by " + (organizer.name ?? organizer.handle ?? organizer.address ?? "Organizer")).foregroundStyle(Theme.secondary) }
            }
            if event.my?.attendeeId != nil, event.status != "cancelled", occurrence?.status != "cancelled" {
                Section("Your response") {
                    Text(CalendarPresentation.rsvp(occurrence?.myRsvp ?? event.my?.rsvp))
                    if recurring, key.occurrenceId != nil {
                        Picker("Apply to", selection: $answerScope) { Text("Whole series").tag(CalendarAnswerScope.all); Text("Only this occurrence").tag(CalendarAnswerScope.this) }
                    }
                    TextField("Note to organizer (optional)", text: $answerNote, axis: .vertical)
                    HStack {
                        Button("Going") { Task { await answer(.accepted) } }
                        Button("Maybe") { Task { await answer(.tentative) } }
                        Button("Not going") { Task { await answer(.declined) } }
                    }.disabled(busy).buttonStyle(.bordered)
                    Text("Responding does not add the organizer as a contact.").font(.geist(12)).foregroundStyle(Theme.secondary)
                }
            }
            if let meeting = event.meeting, meeting.enabled != false, let code = meeting.code {
                Section("Scheduled call") {
                    Text(code.count == 10 ? String(code.prefix(5)) + "-" + String(code.suffix(5)) : code).font(.geistMono(17, .semibold))
                    Text(meeting.transport == "sfu" ? "Up to 30 people" : "Up to 6 people")
                    Text("Hosts and lobby admission follow the event's permissions.").font(.geist(12)).foregroundStyle(Theme.secondary)
                    if event.status != "cancelled", occurrence?.status != "cancelled", let joinMeeting {
                        if let mailbox = store.mailbox, let occurrenceId = key.occurrenceId ?? occurrence?.occurrenceId,
                           let voice = CallHost.ScheduledJoin(mailbox: mailbox, code: code, occurrenceId: occurrenceId, title: occurrence?.title ?? event.title ?? "Meeting", video: false),
                           let video = CallHost.ScheduledJoin(mailbox: mailbox, code: code, occurrenceId: occurrenceId, title: voice.title, video: true) {
                            HStack {
                                Button { joinMeeting(voice) } label: { Label("Join voice", systemImage: "phone").frame(minHeight: 44) }
                                    .accessibilityIdentifier("calendar-join-voice")
                                Button { joinMeeting(video) } label: { Label("Join video", systemImage: "video").frame(minHeight: 44) }
                                    .accessibilityIdentifier("calendar-join-video")
                            }.buttonStyle(.bordered).disabled(store.scopeId != visit || busy || (occurrence?.myRsvp ?? event.my?.rsvp) == "declined")
                        } else { Text("Choose an occurrence from the calendar to join this meeting.").font(.geist(12)).foregroundStyle(Theme.secondary) }
                    }
                }
            }
            Section(recurring ? "Series participants" : "People") {
                ForEach((event.attendees ?? []).filter { $0.invited != false }) { person in
                    VStack(alignment: .leading, spacing: 5) {
                        Text(person.name ?? person.handle ?? (person.kind == "external" ? "Guest" : person.address ?? "Attendee"))
                        Text((person.role == "cohost" ? "Co-host · " : "") + CalendarPresentation.rsvp(person.rsvp)).font(.geist(12)).foregroundStyle(Theme.secondary)
                        // The organizer projection alone can expose contact details and personal links.
                        if event.canEdit == true, person.kind == "external" {
                            if let contact = person.contact { Text(contact).font(.geist(12)).textSelection(.enabled) }
                            if let delivery = person.delivery { Text("Invitation: " + delivery).font(.geist(12)).foregroundStyle(Theme.secondary) }
                            Button("Copy personal invitation link") { Task { await copyLink(person) } }.disabled(store.busy("guest-link:" + person.id))
                                .buttonStyle(.borderless)
                        }
                    }
                }
                if recurring { Text("Responses shown here apply to the whole series.").font(.geist(12)).foregroundStyle(Theme.secondary) }
                if event.canEdit == true, event.status != "cancelled", event.revision != nil {
                    Button("Manage participants") { management = CalendarManagementTarget(kind: .attendees, response: response, key: key) }
                        .disabled(busy).accessibilityIdentifier("calendar-manage-participants")
                }
            }
            Section("Group chat") {
                Text(event.linkedGroup == nil ? "No group linked." : "A group is linked to this event.").foregroundStyle(Theme.secondary)
                if event.canEdit == true, event.status != "cancelled", event.revision != nil {
                    Button("Choose linked group") { management = CalendarManagementTarget(kind: .group, response: response, key: key) }
                        .disabled(busy).accessibilityIdentifier("calendar-manage-linked-group")
                }
            }
            Section("Your calendar") {
                if event.my?.attendeeId != nil {
                    if let id = event.my?.calendarId, let folder = store.folders.first(where: { $0.id == id }) {
                        Text("In " + (folder.name ?? "your calendar")).foregroundStyle(Theme.secondary)
                            .accessibilityIdentifier("calendar-current-placement")
                    }
                    Button("Move invitation") { management = CalendarManagementTarget(kind: .placement, response: response, key: key) }
                        .accessibilityIdentifier("calendar-move-invitation")
                }
                Button("Export event to calendar file") { Task { await export() } }
                    .disabled(store.busy("export:" + key.eventId)).accessibilityIdentifier("calendar-export-event")
                Text("The file contains your own calendar copy. It does not grant invitation or call access.").font(.geist(12)).foregroundStyle(Theme.secondary)
            }
            if event.status != "cancelled" {
                Section("Your reminders") {
                    TextField("Minutes before, separated by commas", text: $reminderText).accessibilityIdentifier("calendar-reminders")
                    Text("Up to five push reminders, from 0 to 40320 minutes before. Leave empty for none.").font(.geist(12)).foregroundStyle(Theme.secondary)
                    Button("Save reminders") { Task { await saveReminders() } }.disabled(store.busy("reminders:" + key.eventId))
                }
            }
            if event.canEdit == true, event.status != "cancelled", event.revision != nil {
                Section {
                    TextField("Cancellation message (optional)", text: $cancellationNote)
                    Menu {
                        if recurring, key.occurrenceId != nil {
                            Button("Cancel only this occurrence", role: .destructive) { stageCancellation(event, .this) }
                            Button("Cancel this and following occurrences", role: .destructive) { stageCancellation(event, .following) }
                        }
                        Button(recurring ? "Cancel whole series" : "Cancel event", role: .destructive) { stageCancellation(event, .all) }
                    } label: { Label("Cancel event", systemImage: "calendar.badge.minus") }
                    .foregroundStyle(Color.red).disabled(busy).accessibilityIdentifier("calendar-cancel-event")
                }
            }
        }
        .accessibilityIdentifier("calendar-event-details")
        .scrollContentBackground(.hidden).background(Theme.app)
        .refreshable { _ = await store.loadEvent(key) }
    }

    private func openEditor(_ response: CalendarEventResponse, _ scope: CalendarEventScope) {
        let target = CalendarEditTarget(response: response, key: key, scope: scope)
        do { _ = try CalendarEventDraft(target: target, fallbackZone: store.zone); edit = target; problem = nil }
        catch { problem = CalendarProblems.message(error) }
    }
    private func answer(_ value: CalendarAnswer) async {
        guard store.scopeId == visit else { return }
        guard answerNote.count <= 300 else { problem = "Keep your response note within 300 characters."; return }
        problem = nil
        if await store.answer(id: key.eventId, status: value, scope: answerScope, occurrenceId: answerScope == .this ? key.occurrenceId : nil, note: answerNote) { answerNote = "" }
    }
    private func cancelTitle(_ scope: CalendarEventScope) -> String {
        switch scope { case .all: return recurring ? "Cancel whole series" : "Cancel event"; case .this: return "Cancel only this occurrence"; case .following: return "Cancel this and following occurrences" }
    }
    private func stageCancellation(_ event: ScheduledEvent, _ scope: CalendarEventScope) {
        guard let revision = event.revision else { return }
        cancellation = Cancellation(key: key, revision: revision, scope: scope)
    }
    private func cancel(_ target: Cancellation) async {
        cancellation = nil
        guard store.scopeId == visit else { return }
        _ = await store.cancelEvent(id: target.key.eventId, revision: target.revision, scope: target.scope, occurrenceId: target.scope == .all ? nil : target.key.occurrenceId, message: cancellationNote)
    }
    private func saveReminders() async {
        guard store.scopeId == visit else { return }
        do {
            let values = try CalendarReminderInput.parse(reminderText)
            problem = nil
            if await store.saveReminders(id: key.eventId, minutes: values), store.scopeId == visit { notice = "Your reminders were saved." }
        } catch { problem = error.localizedDescription }
    }
    private func copyLink(_ person: CalendarAttendee) async {
        guard store.scopeId == visit, visible else { return }
        let screen = presentation
        guard let link = await store.guestLink(id: key.eventId, attendee: person.id), store.scopeId == visit,
              visible, screen == presentation, !Task.isCancelled else { return }
        Clipboard.copy(link.url); notice = "Personal invitation link copied. Share it only with this guest."
    }
    private func export() async {
        guard store.scopeId == visit, visible else { return }
        let screen = presentation
        guard let data = await store.exportEvent(id: key.eventId), store.scopeId == visit, visible,
              screen == presentation, !Task.isCancelled else { return }
        exportDocument = CalendarFileDocument(data: data); exporting = true
    }
}
