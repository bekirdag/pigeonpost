import SwiftUI

struct CalendarLinkEntry: View {
    let request: CalendarLinkRequest
    let visit: (Mailbox) -> Void
    let join: (CallHost.ScheduledJoin) -> Void
    var body: some View {
        switch request.link {
        case .guest(let url): CalendarGuestLinkBrowser(url: url)
        case .member(let code): CalendarMemberMeeting(code: code, visit: visit, join: join)
        }
    }
}

private struct CalendarMemberMeeting: View {
    let code: String
    let visit: (Mailbox) -> Void
    let join: (CallHost.ScheduledJoin) -> Void
    @Environment(Account.self) private var account
    @Environment(CalendarStore.self) private var calendar: CalendarStore?
    @Environment(CallHost.self) private var calls: CallHost?
    @Environment(\.dismiss) private var dismiss
    @State private var lookup: CalendarMeetingLookup
    @State private var selected: String?
    @State private var preview: Context?
    @State private var preparationError: String?
    @State private var reload = UUID()

    private struct Context: Equatable {
        let mailbox: String?
        let subject: String?
        let scope: UUID?
        let signedIn: Bool
        let enabled: Bool
    }
    private struct Load: Equatable { let context: Context; let reload: UUID }
    private var context: Context {
        Context(mailbox: account.me?.address, subject: account.session.subject, scope: calendar?.scopeId,
                signedIn: account.session.status == .signedIn,
                enabled: account.features.calendar && calendar?.enabled == true)
    }
    private var choices: [CalendarMeetingOccurrence] {
        preview == context ? lookup.information?.choices ?? [] : []
    }
    private var canJoin: Bool {
        preview == context && context.enabled && context.signedIn && !lookup.loading
            && preparationError == nil && lookup.error == nil && calls?.mode == .hidden
            && calls?.preparingScheduled != true && choices.contains(where: { $0.id == selected })
    }

    init(code: String, visit: @escaping (Mailbox) -> Void, join: @escaping (CallHost.ScheduledJoin) -> Void) {
        self.code = code; self.visit = visit; self.join = join
        #if DEBUG
        _lookup = State(initialValue: CalendarFixtures.meetingLookup())
        #else
        _lookup = State(initialValue: CalendarMeetingLookup())
        #endif
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(String(code.prefix(5)) + "-" + String(code.suffix(5)))
                        .font(.system(size: 22, weight: .semibold, design: .monospaced))
                    Text("Choose your inbox and call type.").foregroundStyle(Theme.secondary)
                }
                if context.enabled, context.signedIn {
                    Section("Join as") {
                        Picker("Inbox", selection: Binding(get: { account.me?.address ?? "" }, set: chooseInbox)) {
                            ForEach(account.mailboxes, id: \.address) { box in
                                Text(PeerFace.displayName(box.handle ?? box.address)).tag(box.address)
                            }
                        }.disabled(calls?.mode != .hidden || calls?.preparingScheduled == true)
                    }
                    if let error = preparationError ?? lookup.error {
                        Section { Label(error, systemImage: "exclamationmark.circle").foregroundStyle(Color.red) }
                    } else if lookup.loading || lookup.information == nil || preview != context {
                        Section { ProgressView("Loading meeting…") }
                    } else if choices.isEmpty {
                        Section { Text("This meeting is not open for this inbox.").foregroundStyle(Theme.secondary) }
                    } else {
                        Section("Occurrence") {
                            ForEach(choices) { choice in
                                Button { selected = choice.id } label: {
                                    HStack {
                                        Text(time(choice))
                                        Spacer()
                                        if selected == choice.id { Image(systemName: "checkmark") }
                                    }.frame(minHeight: 44).contentShape(Rectangle())
                                }.buttonStyle(.plain)
                                    .accessibilityIdentifier("meeting-link-occurrence:" + choice.id)
                                    .accessibilityAddTraits(selected == choice.id ? [.isSelected] : [])
                            }
                        }
                    }
                    Section {
                        HStack {
                            Button { start(video: false) } label: { Label("Voice call", systemImage: "phone.fill").frame(maxWidth: .infinity, minHeight: 44) }
                                .accessibilityIdentifier("meeting-link-voice")
                            Button { start(video: true) } label: { Label("Video call", systemImage: "video.fill").frame(maxWidth: .infinity, minHeight: 44) }
                                .accessibilityIdentifier("meeting-link-video")
                        }.buttonStyle(.borderedProminent).tint(Theme.navy).disabled(!canJoin)
                    }
                } else { Section { Text("Calendar is unavailable for this account.") } }
            }
            .font(.geist(14)).tint(Theme.navy)
            .navigationTitle("Scheduled meeting")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .primaryAction) { Button("Refresh") { reload = UUID() }.disabled(!context.enabled) }
            }
        }
        .task(id: Load(context: context, reload: reload)) { await load() }
        .onDisappear { lookup.cancel(); preview = nil; selected = nil }
        #if os(macOS)
        .frame(width: 620, height: 560)
        #endif
    }

    private func load() async {
        lookup.cancel(); selected = nil; preview = nil; preparationError = nil
        let snapshot = context
        guard snapshot.enabled, snapshot.signedIn, let mailbox = snapshot.mailbox,
              calendar?.mailbox == mailbox else { return }
        let source: TokenProviding
        #if DEBUG
        source = CalendarFixtures.meetingTokens(session: account.session)
        #else
        source = account.session
        #endif
        do {
            _ = try await source.token()
            guard !Task.isCancelled, context == snapshot, calendar?.mailbox == mailbox else { return }
            preview = snapshot
            await lookup.load(code: code, tokens: source) { context == snapshot && calendar?.mailbox == mailbox }
            guard !Task.isCancelled, context == snapshot else { return }
            if choices.count == 1 { selected = choices.first?.id }
        } catch is CancellationError {
        } catch {
            if context == snapshot, !Task.isCancelled { preparationError = "Could not verify your sign-in. Try again." }
        }
    }

    private func chooseInbox(_ address: String) {
        guard calls?.mode == .hidden, calls?.preparingScheduled != true,
              let box = account.mailboxes.first(where: { $0.address == address }) else { return }
        lookup.cancel(); selected = nil; preview = nil
        visit(box)
    }

    private func start(video: Bool) {
        guard canJoin, let mailbox = context.mailbox, calendar?.mailbox == mailbox,
              let selected, let request = CallHost.ScheduledJoin(mailbox: mailbox, code: code,
                    occurrenceId: selected, title: "Scheduled meeting", video: video) else { return }
        join(request)
    }

    private func time(_ choice: CalendarMeetingOccurrence) -> String {
        guard let start = choice.startsAt, let end = choice.endsAt else { return "Occurrence" }
        let format = DateFormatter(); format.locale = .autoupdatingCurrent
        format.timeZone = calendar?.zone ?? .current; format.dateStyle = .medium; format.timeStyle = .short
        return format.string(from: Date(timeIntervalSince1970: Double(start))) + " – "
            + format.string(from: Date(timeIntervalSince1970: Double(end)))
    }
}
