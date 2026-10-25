import SwiftUI

struct CalendarEditor: View {
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    let saved: (CalendarEventResponse) -> Void
    @State private var draft: CalendarEventDraft
    @State private var problem: String?
    @State private var needsFold = false
    @State private var confirmReload = false
    @State private var started = false
    private let visit: UUID

    init(store: CalendarStore, draft: CalendarEventDraft, saved: @escaping (CalendarEventResponse) -> Void) {
        self.store = store; self.saved = saved; visit = store.scopeId
        _draft = State(initialValue: draft)
    }

    private var busy: Bool { store.busy(draft.target.map { "event:" + $0.key.eventId } ?? "create-event") }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Title", text: $draft.title).accessibilityIdentifier("calendar-title")
                    TextField("Location", text: $draft.location)
                    TextField("Description", text: $draft.description, axis: .vertical).lineLimit(3...8)
                    if !draft.isOccurrenceEdit {
                        Picker("Calendar", selection: $draft.calendarId) {
                            Text("Default calendar").tag(String?.none)
                            ForEach(store.folders) { Text($0.name ?? "Calendar").tag(Optional($0.id)) }
                        }
                    }
                }
                Section("When") {
                    Toggle("All day", isOn: $draft.allDay).disabled(draft.meeting.enabled)
                    DatePicker("Starts", selection: dateBinding(start: true), displayedComponents: draft.allDay ? [.date] : [.date, .hourAndMinute])
                        .accessibilityIdentifier("calendar-start")
                    DatePicker(draft.allDay ? "Last day" : "Ends", selection: dateBinding(start: false), displayedComponents: draft.allDay ? [.date] : [.date, .hourAndMinute])
                        .accessibilityIdentifier("calendar-end")
                    if !draft.allDay {
                        TextField("Timezone, e.g. Europe/Istanbul", text: $draft.tzid).autocorrectionDisabled()
                            .accessibilityIdentifier("calendar-timezone")
                        Text("The time stays as entered in this timezone, including for recurring events.")
                            .font(.geist(12)).foregroundStyle(Theme.secondary)
                        if needsFold || draft.fold != nil {
                            Picker("This time happens twice", selection: Binding(get: { draft.fold?.rawValue ?? "" }, set: { draft.fold = CalendarFold(rawValue: $0) })) {
                                Text("Choose an offset").tag("")
                                Text("Earlier").tag("earlier")
                                Text("Later").tag("later")
                            }.accessibilityIdentifier("calendar-fold")
                        }
                    }
                    if let target = draft.target {
                        Text(scopeLabel(target.scope)).font(.geist(12, .semibold)).foregroundStyle(Theme.secondary)
                    }
                }
                .environment(\.timeZone, CalendarDay.utc.timeZone)
                .environment(\.calendar, CalendarDay.utc)
                if !draft.isOccurrenceEdit {
                    Section("Repeat") {
                        Picker("Repeats", selection: repeatBinding) {
                            Text("Never").tag("")
                            Text("Every day").tag("FREQ=DAILY")
                            Text("Every week").tag("FREQ=WEEKLY")
                            Text("Every weekday").tag("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR")
                            Text("Every month").tag("FREQ=MONTHLY")
                            Text("Every year").tag("FREQ=YEARLY")
                            Text("Custom rule").tag("custom")
                        }
                        if repeatChoice == "custom" {
                            TextField("Recurrence rule", text: $draft.rrule).autocorrectionDisabled()
                        }
                    }
                    meeting
                    Section("Availability") {
                        Picker("Show as", selection: $draft.transparency) {
                            Text("Busy").tag("opaque"); Text("Free").tag("transparent")
                        }
                        Picker("Event privacy", selection: $draft.privacy) {
                            Text("Default").tag("default"); Text("Private").tag("private")
                        }
                        Text("Private events appear as Busy in subscription feeds. Invited attendees still see the event, and the service can read its details.")
                            .font(.geist(12)).foregroundStyle(Theme.secondary)
                    }
                }
                if draft.target == nil {
                    invitees
                    Section("Reminder") {
                        Picker("Notify me", selection: $draft.reminderMinutes) {
                            Text("No reminder").tag(Int?.none)
                            ForEach([0, 5, 10, 15, 30, 60, 1440], id: \.self) { minutes in
                                Text(minutes == 0 ? "At start time" : minutes == 1440 ? "1 day before" : "\(minutes) minutes before").tag(Optional(minutes))
                            }
                        }
                    }
                }
            }
            .formStyle(.grouped)
            .safeAreaInset(edge: .top, spacing: 0) {
                if let message = problem ?? store.lastError {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(message).foregroundStyle(Color.red).accessibilityIdentifier("calendar-editor-error")
                        if store.revisionConflict {
                            Button("Review current version") { confirmReload = true }
                                .accessibilityIdentifier("calendar-review-conflict")
                                .confirmationDialog("Replace your unsaved draft with the current event?", isPresented: $confirmReload, titleVisibility: .visible) {
                                    Button("Load current event", role: .destructive) { Task { await reload() } }
                                        .accessibilityIdentifier("calendar-confirm-reload")
                                    Button("Keep editing", role: .cancel) {}
                                }
                            Text("Your unsaved changes are still here. Loading the current version replaces this draft.").font(.geist(12)).foregroundStyle(Theme.secondary)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading).padding(16).background(Theme.ground)
                }
            }
            .navigationTitle(draft.target == nil ? "New event" : "Edit event")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() }.disabled(busy) }
                ToolbarItem(placement: .confirmationAction) {
                    Button(busy ? "Saving…" : "Save") { Task { await save() } }
                        .disabled(busy || store.scopeId != visit || !store.enabled || store.revisionConflict)
                        .accessibilityIdentifier("calendar-save")
                }
            }
        }
        .font(.geist(14)).tint(Theme.navy)
        .interactiveDismissDisabled(busy)
        .onAppear { if !started { started = true; store.clearError() } }
        .onChange(of: [draft.startDay.id, draft.lastDay.id, draft.startTime, draft.endTime, draft.tzid, String(draft.allDay)]) { _, _ in
            draft.changeTime(); needsFold = false
        }
    }

    private var meeting: some View {
        Section("Call") {
            Toggle("Include a Pigeonpost call", isOn: $draft.meeting.enabled).disabled(draft.allDay)
                .accessibilityIdentifier("calendar-include-call")
            if draft.allDay { Text("Calls need a start and end time.").font(.geist(12)).foregroundStyle(Theme.secondary) }
            if draft.meeting.enabled {
                Picker("Size", selection: $draft.meeting.transport) {
                    Text("Up to 6 people").tag("mesh"); Text("Up to 30 people").tag("sfu")
                }
                Picker("Who waits in the lobby", selection: $draft.meeting.waiting) {
                    Text("Guests").tag("guests"); Text("Everyone except hosts").tag("everyone_except_hosts"); Text("No invited participants").tag("none")
                }
                Picker("Who admits participants", selection: $draft.meeting.admit) {
                    Text("Organizer").tag("organizer"); Text("Organizer and co-hosts").tag("hosts"); Text("Hosts and selected attendees").tag("delegated")
                }
                if draft.meeting.admit == "delegated" {
                    TextField("Selected attendees, one per line", text: Binding(get: { draft.meeting.admitters.joined(separator: "\n") }, set: {
                        draft.meeting.admitters = $0.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
                    }), axis: .vertical)
                }
                Stepper("Open \(draft.meeting.joinBeforeMinutes) minutes before", value: $draft.meeting.joinBeforeMinutes, in: 0...10)
                Toggle("Allow people with the link to knock", isOn: $draft.meeting.allowKnock)
                Toggle("Guests can see the participant list", isOn: $draft.meeting.guestsSeeList)
                Text("Invitation and host permissions are checked when each person joins.").font(.geist(12)).foregroundStyle(Theme.secondary)
            }
        }
    }

    private var invitees: some View {
        Section("Invite people") {
            ForEach($draft.invitees) { $invitee in
                VStack(alignment: .leading, spacing: 8) {
                    TextField("Pigeonpost handle or contact address", text: $invitee.destination).autocorrectionDisabled()
                    TextField("Name (optional)", text: $invitee.name)
                    Toggle("Optional attendee", isOn: $invitee.optional)
                    if invitee.destination.trimmingCharacters(in: .whitespacesAndNewlines).hasPrefix("/") { Toggle("Co-host", isOn: $invitee.cohost) }
                    Button("Remove", role: .destructive) { draft.invitees.removeAll { $0.id == invitee.id } }
                        .buttonStyle(.borderless)
                }
            }
            Button("Add a person") { draft.invitees.append(CalendarInviteeDraft()) }
            Picker("Guest invitation language", selection: $draft.locale) { Text("English").tag("en"); Text("Türkçe").tag("tr") }
            if !store.emailEnabled {
                Text("Invitations to contact addresses are not sent automatically. After saving, copy each guest's personal invitation link from the event.")
                    .font(.geist(12)).foregroundStyle(Theme.secondary)
            }
        }
    }

    private func dateBinding(start: Bool) -> Binding<Date> {
        Binding {
            let day = start ? draft.startDay : draft.lastDay
            let clock = (start ? draft.startTime : draft.endTime).split(separator: ":").compactMap { Int($0) }
            return CalendarDay.utc.date(from: DateComponents(year: day.year, month: day.month, day: day.day,
                hour: clock.first ?? 9, minute: clock.dropFirst().first ?? 0, second: clock.dropFirst(2).first ?? 0)) ?? day.pickerDate
        } set: { date in
            let day = CalendarDay(date: date, zone: CalendarDay.utc.timeZone)
            let clock = CalendarDay.utc.dateComponents([.hour, .minute, .second], from: date)
            let text = String(format: "%02d:%02d:%02d", clock.hour ?? 0, clock.minute ?? 0, clock.second ?? 0)
            if start { draft.startDay = day; draft.startTime = text } else { draft.lastDay = day; draft.endTime = text }
        }
    }

    private var repeatChoice: String {
        ["", "FREQ=DAILY", "FREQ=WEEKLY", "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR", "FREQ=MONTHLY", "FREQ=YEARLY"].contains(draft.rrule) ? draft.rrule : "custom"
    }
    private var repeatBinding: Binding<String> {
        Binding(get: { repeatChoice }, set: { draft.rrule = $0 == "custom" ? "FREQ=WEEKLY;COUNT=6" : $0 })
    }
    private func scopeLabel(_ scope: CalendarEventScope) -> String {
        switch scope { case .all: return "Changing the whole series."; case .this: return "Changing only this occurrence."; case .following: return "Changing this and following occurrences." }
    }
    private func explain(_ error: Error) -> String { (error as? CalendarDraftError)?.errorDescription ?? CalendarProblems.message(error) }

    private func save() async {
        guard store.scopeId == visit, !store.revisionConflict else { return }
        problem = nil
        do {
            let fields = try draft.fields()
            let result: CalendarEventResponse?
            if let target = draft.target {
                guard let revision = draft.revision else { problem = "Reload this event before editing."; return }
                if fields.isEmpty { dismiss(); return }
                result = await store.updateEvent(id: target.key.eventId, revision: revision, scope: target.scope, occurrenceId: draft.occurrenceId, fields: fields)
            } else { result = await store.createEvent(fields: fields) }
            if let result, store.scopeId == visit { saved(result); dismiss() }
        } catch {
            if case CalendarTimeError.ambiguous = error { needsFold = true }
            problem = explain(error)
        }
    }

    private func reload() async {
        guard let target = draft.target, let response = await store.loadEvent(target.key), store.scopeId == visit else { return }
        guard response.event.canEdit == true else { problem = "You can no longer edit this event."; return }
        do {
            draft = try CalendarEventDraft(target: .init(response: response, key: target.key, scope: target.scope), fallbackZone: store.zone)
            problem = nil; store.clearError()
        } catch { problem = explain(error) }
    }
}
