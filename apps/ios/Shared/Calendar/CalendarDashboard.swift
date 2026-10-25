import SwiftUI

struct CalendarDashboard: View {
    @Environment(Account.self) private var account
    @Environment(\.dismiss) private var dismiss
    let store: CalendarStore
    var joinMeeting: ((CallHost.ScheduledJoin) -> Void)?
    @State private var panel: Panel?

    private enum Panel: Identifiable {
        case event(CalendarEventKey), new, invitations, settings, calendars, importing
        var id: String {
            switch self {
            case .event(let key): return "event:" + key.id
            case .new: return "new"
            case .invitations: return "invitations"
            case .settings: return "settings"
            case .calendars: return "calendars"
            case .importing: return "import"
            }
        }
    }

    var body: some View {
        NavigationStack {
            Group {
                if store.enabled, store.mailbox == account.me?.address {
                    content
                } else { ContentUnavailableView("Calendar unavailable", systemImage: "calendar") }
            }
            .navigationTitle("Calendar")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .primaryAction) {
                    if store.enabled {
                        Button { panel = .new } label: { Label("New event", systemImage: "plus") }
                            .accessibilityIdentifier("calendar-new-event")
                    }
                }
                #if os(iOS)
                ToolbarItem(placement: .primaryAction) { optionsMenu }
                #endif
            }
        }
        .font(.geist(14))
        .tint(Theme.navy)
        .onAppear { store.isOpen = true }
        .onDisappear { store.isOpen = false; store.openEvent = nil }
        .onChange(of: store.scopeId) { _, _ in panel = nil }
        .sheet(item: $panel) { which in
            Group {
                switch which {
                case .new:
                    CalendarEditor(store: store, draft: CalendarEventDraft(day: store.selectedDay, zone: store.zone)) { _ in panel = nil }
                case .event(let key): CalendarDetail(store: store, key: key, joinMeeting: joinMeeting)
                case .invitations: CalendarInvitationsScreen(store: store, joinMeeting: joinMeeting)
                case .settings: CalendarSettingsScreen(store: store)
                case .calendars: CalendarFoldersScreen(store: store)
                case .importing: CalendarImportScreen(store: store)
                }
            }
            .id(store.scopeId)
            #if os(macOS)
            .frame(width: 640, height: 660)
            #endif
        }
        #if os(macOS)
        .frame(width: 760, height: 700)
        #endif
    }

    private var optionsMenu: some View {
        Menu {
            Button("Invitations") { panel = .invitations }
            Button("Manage calendars") { panel = .calendars }
            Button("Import calendar file") { panel = .importing }.accessibilityIdentifier("calendar-import-file")
            Button("Calendar settings") { panel = .settings }
            Button("Refresh") { Task { await store.refresh() } }
        } label: { Label("Calendar options", systemImage: "ellipsis.circle") }
        .accessibilityIdentifier("calendar-options")
    }

    private var content: some View {
        List {
            if let problem = store.lastError {
                Section { Label(problem, systemImage: "exclamationmark.circle").foregroundStyle(Color.red) }
            }
            Section {
                month
                Text("Times shown in \(store.zone.identifier)")
                    .font(.geist(12)).foregroundStyle(Theme.secondary)
                if let zone = store.settings.timezone, TimeZone(identifier: zone) == nil {
                    Text("Saved timezone \(zone) is unavailable. Showing the device timezone; choose a timezone in Calendar settings.")
                        .font(.geist(12)).foregroundStyle(Color.red)
                }
            }
            Section {
                Button { panel = .invitations } label: {
                    HStack {
                        Label("Invitations", systemImage: "envelope")
                        Spacer()
                        Text("\(store.pendingCount)").foregroundStyle(Theme.secondary)
                    }
                }.accessibilityIdentifier("calendar-invitations")
                Menu {
                    Button("All calendars") { Task { await store.selectFolders(nil) } }
                    Button("No calendars") { Task { await store.selectFolders([]) } }
                    ForEach(store.folders) { folder in
                        Button {
                            var picked = store.selectedFolders ?? Set(store.folders.map(\.id))
                            if picked.contains(folder.id) { picked.remove(folder.id) } else { picked.insert(folder.id) }
                            Task { await store.selectFolders(picked) }
                        } label: {
                            if store.selectedFolders?.contains(folder.id) ?? true { Label(folder.name ?? "Calendar", systemImage: "checkmark") }
                            else { Text(folder.name ?? "Calendar") }
                        }
                    }
                } label: { Label("Calendars to show", systemImage: "line.3.horizontal.decrease.circle") }
            }
            Section(CalendarPresentation.day(store.selectedDay)) {
                if store.loading && store.occurrences.isEmpty { ProgressView("Loading calendar…") }
                else if agenda.isEmpty {
                    Text(store.selectedFolders?.isEmpty == true ? "No calendars selected." : "No events on this day.")
                        .foregroundStyle(Theme.secondary)
                }
                ForEach(agenda) { item in
                    Button { panel = .event(.init(eventId: item.eventId, occurrenceId: item.occurrenceId)) } label: {
                        HStack(alignment: .top, spacing: 12) {
                            RoundedRectangle(cornerRadius: 2).fill(CalendarPresentation.color(item.color)).frame(width: 4)
                            VStack(alignment: .leading, spacing: 5) {
                                Text(item.title ?? "Event").font(.geist(15, .semibold))
                                    .strikethrough(item.status == "cancelled")
                                Text(CalendarPresentation.time(item, zone: store.zone)).font(.geist(13))
                                if let location = item.location, !location.isEmpty { Text(location).font(.geist(12)).foregroundStyle(Theme.secondary) }
                                if item.status == "cancelled" { Text("Cancelled").foregroundStyle(Color.red) }
                                else if item.invited == true { Text("Invitation · \(CalendarPresentation.rsvp(item.myRsvp))").foregroundStyle(Theme.secondary) }
                            }
                            Spacer(minLength: 0)
                            if item.meeting != nil { Image(systemName: "video").accessibilityLabel("Scheduled call") }
                        }
                        .foregroundStyle(Theme.ink).padding(.vertical, 5)
                    }.accessibilityIdentifier("calendar-event:" + item.occurrenceId)
                }
            }
            Section {
                Text("Calendar details are readable by the service to deliver invitations and reminders.")
                    .font(.geist(12)).foregroundStyle(Theme.secondary)
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.app)
        .refreshable { await store.refresh() }
    }

    private var month: some View {
        let days = (try? CalendarMonthWindow(containing: store.selectedDay, weekStart: store.weekStart).days) ?? []
        return VStack(spacing: 8) {
            HStack {
                Button { moveMonth(-1) } label: { Image(systemName: "chevron.left").frame(width: 44, height: 44).contentShape(Rectangle()) }.accessibilityLabel("Previous month")
                Spacer()
                Text(CalendarPresentation.month(store.selectedDay)).font(.geist(17, .semibold))
                Spacer()
                Button { moveMonth(1) } label: { Image(systemName: "chevron.right").frame(width: 44, height: 44).contentShape(Rectangle()) }.accessibilityLabel("Next month")
                Button { Task { await store.today() } } label: { Text("Today").frame(minHeight: 44).contentShape(Rectangle()) }
                #if os(macOS)
                // macOS NavigationStack can omit a second primary toolbar item in a sheet.
                // Keep this menu in the content header, alongside the month controls.
                optionsMenu.frame(maxWidth: 170)
                #endif
            }
            HStack(spacing: 2) {
                ForEach(0..<7) { index in
                    Text(CalendarPresentation.weekday((store.weekStart + index) % 7)).font(.geist(12, .semibold)).foregroundStyle(Theme.secondary)
                        .frame(maxWidth: .infinity)
                }
            }
            // All42 cells are small. Eager week rows give List a stable measured height; a lazy
            // grid's estimated row height can oscillate during UIKit self-sizing and trap.
            VStack(spacing: 2) {
                ForEach(0..<6) { week in
                    HStack(spacing: 2) {
                        ForEach(Array(days.dropFirst(week * 7).prefix(7))) { day in
                            Button { Task { await store.select(day) } } label: {
                                VStack(spacing: 3) {
                                    Text("\(day.day)").font(.geist(14, day == store.selectedDay ? .bold : .regular))
                                    Circle().fill(hasEvents(day) ? Theme.navy : .clear).frame(width: 4, height: 4)
                                }
                                .foregroundStyle(day == store.selectedDay ? Theme.onAccent : day.month == store.selectedDay.month ? Theme.ink : Theme.secondary)
                                .frame(maxWidth: .infinity).frame(height: 44)
                                .background(day == store.selectedDay ? Theme.accent : .clear, in: RoundedRectangle(cornerRadius: 8))
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel(CalendarPresentation.day(day))
                            .accessibilityAddTraits(day == store.selectedDay ? [.isSelected] : [])
                            .accessibilityIdentifier("calendar-day:" + day.id)
                        }
                    }
                }
            }
        }
        // List's automatic row button style would invoke Previous, Next and Today together.
        .buttonStyle(.plain)
    }

    private var agenda: [EventOccurrence] {
        CalendarPresentation.events(store.occurrences, day: store.selectedDay, zone: store.zone).sorted {
            if ($0.allDay == true) != ($1.allDay == true) { return $0.allDay == true }
            return ($0.startsAt ?? 0, $0.occurrenceId) < ($1.startsAt ?? 0, $1.occurrenceId)
        }
    }
    private func hasEvents(_ day: CalendarDay) -> Bool { !CalendarPresentation.events(store.occurrences, day: day, zone: store.zone).isEmpty }
    private func moveMonth(_ amount: Int) {
        guard let first = try? CalendarDay(String(format: "%04d-%02d-01", store.selectedDay.year, store.selectedDay.month)),
              let date = CalendarDay.utc.date(byAdding: .month, value: amount, to: first.pickerDate) else { return }
        Task { await store.select(CalendarDay(date: date, zone: CalendarDay.utc.timeZone)) }
    }
}

enum CalendarPresentation {
    static func formatter(_ pattern: String, zone: TimeZone) -> DateFormatter {
        let f = DateFormatter(); f.calendar = Calendar(identifier: .gregorian); f.locale = .current; f.timeZone = zone; f.dateFormat = pattern
        return f
    }
    static func day(_ value: CalendarDay) -> String { formatter("EEEE, d MMMM yyyy", zone: CalendarDay.utc.timeZone).string(from: value.pickerDate) }
    static func month(_ value: CalendarDay) -> String { formatter("MMMM yyyy", zone: CalendarDay.utc.timeZone).string(from: value.pickerDate) }
    static func weekday(_ sundayIndex: Int) -> String { DateFormatter().shortWeekdaySymbols[sundayIndex] }
    static func instant(_ seconds: Int?, zone: TimeZone) -> String {
        seconds.map { formatter("d MMM, HH:mm", zone: zone).string(from: Date(timeIntervalSince1970: Double($0))) } ?? "Time unavailable"
    }
    static func time(_ value: EventOccurrence, zone: TimeZone) -> String {
        if value.allDay == true { return "All day" }
        return instant(value.startsAt, zone: zone) + " – " + instant(value.endsAt, zone: zone)
    }
    static func rsvp(_ value: String?) -> String {
        switch value { case "accepted": return "Going"; case "tentative": return "Maybe"; case "declined": return "Not going"; default: return "Needs a response" }
    }
    static func color(_ value: String?) -> Color {
        switch value { case "green": return .green; case "orange": return .orange; case "red": return Color.red; case "purple": return .purple; case "teal": return .teal; case "gray": return .gray; default: return .blue }
    }
    static func events(_ items: [EventOccurrence], day: CalendarDay, zone: TimeZone) -> [EventOccurrence] {
        // Resolve real day boundaries once per day, including23/25-hour days, not once per row.
        let start = try? day.start(in: zone).timeIntervalSince1970
        let end = try? day.adding(days: 1).start(in: zone).timeIntervalSince1970
        return items.filter { item in
            if item.allDay == true {
                guard let first = item.startDate, let last = item.endDate else { return false }
                return first <= day.id && day.id < last
            }
            guard let first = item.startsAt, let last = item.endsAt, let start, let end else { return false }
            return Double(first) < end && Double(last) > start
        }
    }
}
