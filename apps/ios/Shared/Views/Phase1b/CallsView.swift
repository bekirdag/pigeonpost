//  The Calls page (SDS §12.1): who called, who was called, which of them were missed, and the way
//  back to each — call back, open the conversation, or block and report.
//
//  One list, two homes. A phone opens it as a sheet from the top bar; an iPad or a Mac shows it in
//  the sidebar in place of the conversations, as the web inbox does.

import SwiftUI

struct CallLogList: View {
    let callLog: CallLog
    /// Open the conversation with this peer.
    let open: (String) -> Void
    /// Call this peer back, by video when `true`.
    let callBack: (String, Bool) -> Void

    @Environment(Inbox.self) private var inbox
    @State private var blocking: BlockTarget?
    @State private var confirmingClear = false

    var body: some View {
        @Bindable var log = callLog
        List {
            Picker("Show", selection: $log.filter) {
                ForEach(CallLog.Filter.allCases) { filter in
                    Text(filter.title).tag(filter)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .listRowBackground(Color.clear)
            .listRowSeparator(.hidden)
            .accessibilityIdentifier("callsFilter")

            if let error = callLog.error {
                Text(error)
                    .font(.geist(13))
                    .foregroundStyle(Theme.muted)
                    .listRowBackground(Color.clear)
            }
            ForEach(callLog.entries) { entry in
                CallRow(entry: entry, name: name(entry), open: open, callBack: callBack, actions: actions(entry))
                    .listRowBackground(Color.clear)
                    .listRowSeparatorTint(Theme.rule)
            }
            if callLog.next != nil {
                Button("Load more") { Task { await callLog.load(more: true) } }
                    .font(.geist(14, .medium))
                    .disabled(callLog.loading)
                    .listRowBackground(Color.clear)
                    .accessibilityIdentifier("callsMore")
            }
            if !callLog.entries.isEmpty, callLog.filter != .filtered {
                Button("Clear all calls", role: .destructive) { confirmingClear = true }
                    .font(.geist(14))
                    .listRowBackground(Color.clear)
                    .listRowSeparator(.hidden)
                    .accessibilityIdentifier("callsClear")
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .overlay {
            if callLog.loaded, callLog.entries.isEmpty, callLog.error == nil {
                ContentUnavailableView(emptyTitle, systemImage: "phone",
                                       description: Text(callLog.filter == .filtered
                                                         ? "Calls your privacy settings silenced appear here."
                                                         : "Voice and video calls appear here."))
            } else if !callLog.loaded, callLog.loading {
                ProgressView()
            }
        }
        #if os(iOS)
        .refreshable { await callLog.load() }
        #endif
        .task(id: callLog.filter) {
            await callLog.load()
            await callLog.markSeen()
        }
        .confirmationDialog("Clear your call history?", isPresented: $confirmingClear, titleVisibility: .visible) {
            Button("Clear all", role: .destructive) { Task { await callLog.clear() } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Every call in this mailbox’s history is deleted. The other side keeps theirs.")
        }
        .sheet(item: $blocking) { target in BlockSheet(target: target) }
        .toast($log.toast)
    }

    private var emptyTitle: String {
        switch callLog.filter {
        case .all: return "No calls yet"
        case .missed: return "No missed calls"
        case .filtered: return "No filtered calls"
        }
    }

    private func name(_ entry: CallLogEntry) -> String {
        if let key = entry.peerKey {
            return inbox.conversation(with: key)?.name ?? PeerFace.displayName(key)
        }
        let people = entry.participants?.count ?? 0
        return people > 0 ? "\(people) people" : "Call"
    }

    private func actions(_ entry: CallLogEntry) -> CallRow.Actions {
        CallRow.Actions(
            delete: { Task { await callLog.delete(entry) } },
            block: entry.peerKey.map { key in
                { blocking = BlockTarget(peer: key, name: name(entry), origin: .call,
                                         callId: entry.isIncoming ? entry.id : nil) }
            },
            report: entry.isIncoming && entry.peerKey != nil ? { Task { await callLog.report(entry) } } : nil
        )
    }
}

struct CallRow: View {
    struct Actions {
        let delete: () -> Void
        let block: (() -> Void)?
        let report: (() -> Void)?
    }

    let entry: CallLogEntry
    let name: String
    let open: (String) -> Void
    let callBack: (String, Bool) -> Void
    let actions: Actions

    var body: some View {
        HStack(spacing: 10) {
            HStack(spacing: 11) {
                Avatar(peer: entry.peerKey ?? "/guest", size: 38)
                VStack(alignment: .leading, spacing: 2) {
                    Text(name)
                        .font(.geistMono(13.5, .semibold))
                        .foregroundStyle(entry.isMissed ? Theme.Pill.blockedText : Theme.ink)
                        .lineLimit(1)
                    Text(entry.summary)
                        .font(.geist(12.5))
                        .foregroundStyle(Theme.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                Text(Time.listTime(entry.startedAt))
                    .font(.geist(11.5))
                    .foregroundStyle(Theme.muted)
                    .fixedSize()
            }
            .contentShape(Rectangle())
            .onTapGesture { if let key = entry.peerKey { open(key) } }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isButton)
            .accessibilityLabel("\(name), \(entry.outcomeLabel.lowercased()) \(entry.isVideo ? "video" : "voice") call, \(Time.listTime(entry.startedAt))")
            .accessibilityIdentifier("call:" + entry.id)

            if let key = entry.peerKey {
                Button { callBack(key, entry.isVideo) } label: {
                    Image(systemName: entry.isVideo ? "video" : "phone")
                        .font(.geist(15, .medium))
                        .foregroundStyle(Theme.secondary)
                        .frame(width: 34, height: 34)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.borderless)
                .accessibilityLabel("Call \(name) back, \(entry.isVideo ? "video" : "voice")")
                .accessibilityIdentifier("callBack:" + entry.id)
            }
            Menu {
                Button(role: .destructive, action: actions.delete) {
                    Label("Delete from history", systemImage: "trash")
                }
                if let block = actions.block {
                    Button(role: .destructive, action: block) {
                        Label("Block", systemImage: "hand.raised")
                    }
                }
                if let report = actions.report {
                    Button(role: .destructive, action: report) {
                        Label("Report spam", systemImage: "exclamationmark.bubble")
                    }
                }
            } label: {
                Image(systemName: "ellipsis")
                    .font(.geist(15, .medium))
                    .foregroundStyle(Theme.secondary)
                    .frame(width: 30, height: 34)
                    .contentShape(Rectangle())
            }
            .menuIndicator(.hidden)
            .buttonStyle(.borderless)
            .fixedSize()
            .accessibilityLabel("More actions for this call")
            .accessibilityIdentifier("callMore:" + entry.id)
        }
        .padding(.vertical, 4)
    }
}

/// The phone's Calls page: the list, in a sheet of its own.
struct CallsSheet: View {
    let callLog: CallLog
    let open: (String) -> Void
    let callBack: (String, Bool) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            CallLogList(callLog: callLog, open: open, callBack: callBack)
                .background(Theme.app)
                .navigationTitle("Calls")
                .inlineTitle()
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                }
        }
        .accessibilityIdentifier("callsPage")
    }
}

/// The missed-call badge on the Calls button. Same shape as the unread badge.
struct CallsButtonLabel: View {
    let unseen: Int

    var body: some View {
        Image(systemName: "phone")
            .foregroundStyle(Theme.secondary)
            .overlay(alignment: .topTrailing) {
                if unseen > 0 {
                    UnreadBadge(count: min(unseen, 99))
                        .scaleEffect(0.8)
                        .offset(x: 10, y: -9)
                        .allowsHitTesting(false)
                }
            }
            .accessibilityLabel(unseen > 0 ? "Calls, \(unseen) missed" : "Calls")
    }
}
