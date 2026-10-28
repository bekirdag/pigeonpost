//  Meeting notes: the list of meetings whose notes this account agreed to, and each brief opened
//  on this device. The native twin of the web inbox's reader (site-inbox/meeting-notes.js).

import SwiftUI

extension CallHost: MeetingNotesKeys {
    func meetingNotesKey() async throws -> MeetingNotesKey {
        let key = try await notesKey()
        return MeetingNotesKey(id: key.id, publicKey: key.publicKey, label: key.label)
    }
    func openMeetingBrief(_ meeting: [String: Any]) async throws -> Data { try await openBrief(meeting) }
    func refusal(_ error: Error) -> String? {
        if case let .refused(code)? = error as? BridgeError { return code }
        return nil
    }
}

/// The row that opens it: beside Calendar in the sidebar, and at the top of the phone's list.
struct MeetingNotesEntry: View {
    let open: () -> Void
    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Image(systemName: "doc.text")
                Text("Meeting notes").font(.geist(14, .medium))
                Spacer()
                Image(systemName: "chevron.right").font(.geist(11))
            }
            .foregroundStyle(Theme.ink).frame(maxWidth: .infinity, minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Meeting notes")
        .accessibilityIdentifier("meetingNotesButton")
    }
}

struct MeetingNotesScreen: View {
    @Environment(Account.self) private var account
    @Environment(CallHost.self) private var calls
    @Environment(\.dismiss) private var dismiss
    @State private var store: MeetingNotesStore?

    var body: some View {
        NavigationStack {
            Group {
                if let store { MeetingNotesList(store: store) } else { ProgressView() }
            }
            .navigationTitle("Meeting notes")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }.accessibilityIdentifier("meetingNotesClose")
                }
            }
        }
        // Over the whole stack, so a brief's Copied or Deleted shows wherever the reader is.
        .toast(Binding(get: { store?.toast }, set: { store?.toast = $0 }))
        #if os(macOS)
        .frame(minWidth: 560, minHeight: 520)
        #endif
        .task {
            guard store == nil else { return }
            let account = account
            let made = MeetingNotesStore(client: { [weak account] in account?.scopedClient() }, keys: calls,
                                         fixtures: Fixtures.enabled && !CallHost.debugEnabled)
            store = made
            async let list: Void = made.load()
            async let key: Void = made.register()
            _ = await (list, key)
        }
    }
}

private struct MeetingNotesList: View {
    let store: MeetingNotesStore

    var body: some View {
        List {
            Section {
                Text(MeetingNotesText.intro)
                    .font(.geist(12.5)).foregroundStyle(Theme.secondary)
            }
            switch store.list {
            case .loading:
                ProgressView().frame(maxWidth: .infinity)
            case .failed:
                Text("Couldn't load your meeting notes.").accessibilityIdentifier("meetingNotesFailed")
            case let .loaded(rows) where rows.isEmpty:
                Text("No meeting notes yet.").accessibilityIdentifier("meetingNotesEmpty")
            case let .loaded(rows):
                ForEach(rows) { row in
                    NavigationLink {
                        MeetingNoteDetail(store: store, meeting: row)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(MeetingNotesText.when(row.createdAt)).font(.geist(14, .semibold)).foregroundStyle(Theme.ink)
                            Text(MeetingNotesText.meta(row)).font(.geist(12)).foregroundStyle(Theme.secondary)
                        }
                        .padding(.vertical, 2)
                    }
                    .accessibilityIdentifier("meeting-" + row.id)
                }
            }
        }
        .refreshable { await store.load() }
    }
}

private struct MeetingNoteDetail: View {
    let store: MeetingNotesStore
    let meeting: MeetingSummary
    @Environment(\.dismiss) private var dismiss
    @State private var confirmingDelete = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                content
            }
            .padding(18)
            .frame(maxWidth: 760, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .background(Theme.wash)
        .navigationTitle(title)
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .task { if store.opened[meeting.id] == nil { await store.open(meeting.id) } }
        .confirmationDialog("Delete your copy of these notes?", isPresented: $confirmingDelete, titleVisibility: .visible) {
            Button("Delete my copy", role: .destructive) {
                Task { if await store.delete(meeting.id) { dismiss() } }
            }
            Button("Cancel", role: .cancel) {}
        } message: { Text("Other attendees keep theirs.") }
    }

    private var title: String {
        if case let .brief(_, brief)? = store.opened[meeting.id], let t = brief.title, !t.isEmpty { return t }
        return "Meeting notes"
    }

    @ViewBuilder
    private var content: some View {
        switch store.opened[meeting.id] ?? .loading {
        case .loading:
            ProgressView().frame(maxWidth: .infinity)
        case let .brief(_, brief):
            BriefView(brief: brief)
            actions(brief)
        case let .pending(m):
            note(m.state == "failed" ? MeetingNotesText.failure(m.failure) : MeetingNotesText.preparing)
            actions(nil)
        case .otherDevices:
            note(MeetingNotesText.otherDevices)
            actions(nil)
        case .cannotOpen:
            note(MeetingNotesText.cannotOpen)
            actions(nil)
        case .gone:
            note(MeetingNotesText.gone)
        }
    }

    private func note(_ text: String) -> some View {
        Text(text).font(.geist(14)).foregroundStyle(Theme.body).accessibilityIdentifier("meetingNoteMessage")
    }

    private func actions(_ brief: MeetingBrief?) -> some View {
        HStack(spacing: 10) {
            if let brief {
                Button("Copy as text") {
                    Clipboard.copy(MeetingNotesText.plain(brief))
                    store.toast = "Copied."
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("meetingNoteCopy")
            }
            Button("Delete my copy", role: .destructive) { confirmingDelete = true }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("meetingNoteDelete")
        }
        .padding(.top, 6)
    }
}

private struct BriefView: View {
    let brief: MeetingBrief

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            let header = MeetingNotesText.header(brief)
            if !header.isEmpty { Text(header).font(.geist(12.5)).foregroundStyle(Theme.secondary) }
            if brief.draft == true {
                Text(MeetingNotesText.draft).font(.geist(13)).foregroundStyle(Theme.amber)
            }
            if let summary = brief.summary, !summary.isEmpty {
                heading("Summary")
                Text(summary).font(.geist(14.5)).foregroundStyle(Theme.ink).textSelection(.enabled)
                    .accessibilityIdentifier("meetingNoteSummary")
            }
            bullets("Decisions", brief.decisions ?? [])
            bullets("Action items", (brief.actionItems ?? []).map(MeetingNotesText.action))
            bullets("Open questions", brief.openQuestions ?? [])
            if let lines = brief.transcript, !lines.isEmpty {
                heading("Transcript")
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(line.at ?? "").font(.geistMono(11.5)).foregroundStyle(Theme.muted)
                        (Text((line.speaker ?? "") + ": ").bold() + Text(line.text ?? ""))
                            .font(.geist(13.5)).foregroundStyle(Theme.body)
                    }
                    .textSelection(.enabled)
                }
            }
            Text(MeetingNotesText.generated(brief)).font(.geist(11.5)).foregroundStyle(Theme.muted).padding(.top, 4)
        }
    }

    private func heading(_ text: String) -> some View {
        Text(text).font(.geist(15, .semibold)).foregroundStyle(Theme.ink).padding(.top, 4)
    }

    @ViewBuilder
    private func bullets(_ title: String, _ items: [String]) -> some View {
        if !items.isEmpty {
            heading(title)
            ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text("•").foregroundStyle(Theme.muted)
                    Text(item).font(.geist(14)).foregroundStyle(Theme.body).textSelection(.enabled)
                }
            }
        }
    }
}
