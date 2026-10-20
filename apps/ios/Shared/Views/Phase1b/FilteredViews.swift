//  Filtered (SDS §13.4): the row at the foot of the conversation list, the senders behind it, and
//  what can be decided about each — let them in, block them, or report them.
//
//  Filtered mail never rang and was never unread. It is looked at on purpose, so it sits below
//  everything else rather than among it.

import SwiftUI

/// The row at the bottom of the conversation list. Present only when there is something in it.
struct FilteredEntryRow: View {
    let count: Int

    var body: some View {
        HStack(spacing: 11) {
            Image(systemName: "line.3.horizontal.decrease.circle")
                .font(.geist(20))
                .foregroundStyle(Theme.secondary)
                .frame(width: 38)
            VStack(alignment: .leading, spacing: 2) {
                Text("Filtered")
                    .font(.geist(14.5, .semibold))
                    .foregroundStyle(Theme.ink)
                Text("\(count) \(count == 1 ? "sender" : "senders") you don’t know")
                    .font(.geist(12.5))
                    .foregroundStyle(Theme.secondary)
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right")
                .font(.geist(12, .semibold))
                .foregroundStyle(Theme.muted)
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityLabel("Filtered, \(count) \(count == 1 ? "conversation" : "conversations")")
        .accessibilityIdentifier("filteredEntry")
    }
}

/// Everyone in Filtered, newest first, and one sender's mail on the next page.
struct FilteredSheet: View {
    let filtered: FilteredMail
    @Environment(Inbox.self) private var inbox
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        @Bindable var filtered = filtered
        let senders = filtered.senders(contacts: inbox.contacts)
        NavigationStack {
            List {
                Section {
                    ForEach(senders) { sender in
                        NavigationLink(value: sender.peer) {
                            HStack(spacing: 11) {
                                Avatar(peer: sender.peer, size: 36)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(PeerFace.displayName(sender.peer))
                                        .font(.geistMono(13.5, .semibold))
                                        .foregroundStyle(Theme.ink)
                                        .lineLimit(1)
                                    Text(sender.messages.last.map { ConversationBuilder.preview(body: $0.body) } ?? "")
                                        .font(.geist(12.5))
                                        .foregroundStyle(Theme.secondary)
                                        .lineLimit(1)
                                }
                                Spacer(minLength: 0)
                                Text(Time.listTime(sender.last))
                                    .font(.geist(11.5))
                                    .foregroundStyle(Theme.muted)
                            }
                        }
                        .accessibilityIdentifier("filtered:" + sender.peer)
                    }
                } footer: {
                    Text("Mail your privacy settings kept out of the inbox. It didn’t notify you and isn’t counted as unread. Change what is filtered in Settings → Privacy & filtering.")
                }
            }
            .sheetSurface()
            .overlay {
                if senders.isEmpty {
                    ContentUnavailableView("Nothing filtered", systemImage: "line.3.horizontal.decrease.circle")
                }
            }
            .navigationTitle("Filtered")
            .inlineTitle()
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .navigationDestination(for: String.self) { peer in
                if let sender = senders.first(where: { $0.peer == peer }) {
                    FilteredSenderPage(sender: sender, filtered: filtered)
                } else {
                    ContentUnavailableView("Already decided", systemImage: "checkmark.circle")
                }
            }
        }
        .toast($filtered.toast)
        .accessibilityIdentifier("filteredPage")
    }
}

/// One filtered sender: what they wrote, and the three decisions.
struct FilteredSenderPage: View {
    let sender: FilteredSender
    let filtered: FilteredMail
    @Environment(Inbox.self) private var inbox
    @State private var blocking: BlockTarget?
    @State private var working = false
    @State private var confirmingReport = false

    var body: some View {
        List {
            Section {
                ForEach(sender.messages) { message in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(Time.listTime(message.at))
                            .font(.geist(11.5))
                            .foregroundStyle(Theme.muted)
                        Text(ConversationBuilder.preview(body: message.body))
                            .font(.geist(14))
                            .foregroundStyle(Theme.body)
                            .textSelection(.enabled)
                    }
                    .padding(.vertical, 2)
                }
            } header: {
                Text("From \(sender.from)")
                    .font(.geistMono(12))
            }
            Section {
                Button(working ? "Accepting…" : "Accept") {
                    working = true
                    Task {
                        await filtered.accept(sender, inbox: inbox)
                        working = false
                    }
                }
                .font(.geist(15, .semibold))
                .disabled(working)
                .accessibilityIdentifier("filteredAccept")
                Button("Block", role: .destructive) {
                    blocking = BlockTarget(peer: sender.peer, name: PeerFace.displayName(sender.peer),
                                           origin: .filtered, messageId: sender.messages.last?.messageId)
                }
                .accessibilityIdentifier("filteredBlock")
                Button("Report spam", role: .destructive) { confirmingReport = true }
                    .accessibilityIdentifier("filteredReport")
            } footer: {
                Text("Accepting adds them to your contacts and moves their messages to the inbox. Blocking refuses their messages and calls; they aren’t told.")
            }
        }
        .sheetSurface()
        .navigationTitle(PeerFace.displayName(sender.peer))
        .inlineTitle()
        .sheet(item: $blocking) { target in BlockSheet(target: target) }
        .confirmationDialog("Report this sender as spam?", isPresented: $confirmingReport, titleVisibility: .visible) {
            Button("Report spam", role: .destructive) {
                if let last = sender.messages.last { Task { await inbox.reportSpam(messageId: last.messageId) } }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The postbox is told about their latest message. It counts against their standing; they aren’t told.")
        }
    }
}
