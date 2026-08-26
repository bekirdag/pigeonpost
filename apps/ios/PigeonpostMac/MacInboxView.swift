//  Conversations on the left, the thread on the right.
//
//  `NavigationSplitView` is what the phone already uses, and on the Mac it is the native shape
//  rather than an adaptation — a real sidebar the person can resize, and a detail pane that does
//  not have to be pushed and popped.

import SwiftUI

struct MacInboxView: View {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @State private var selection: String?
    @State private var showingNew = false

    var body: some View {
        @Bindable var inbox = inbox

        NavigationSplitView {
            List(selection: $selection) {
                if inbox.offline {
                    Label("Not connected. Showing what was last loaded.", systemImage: "wifi.slash")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.muted)
                }
                ForEach(inbox.visible) { conversation in
                    MacConversationRow(conversation: conversation)
                        .tag(conversation.peer)
                }
            }
            .searchable(text: $inbox.filter, prompt: "Search")
            .navigationSplitViewColumnWidth(min: 240, ideal: 300, max: 420)
            .safeAreaInset(edge: .top, spacing: 0) { mailboxBar }
        } detail: {
            if let selection, inbox.conversation(with: selection) != nil {
                MacThreadView(peer: selection)
                    .id(selection)
            } else {
                ContentUnavailableView("Pick a conversation", systemImage: "tray")
            }
        }
        .task {
            await inbox.loadAll()
            await inbox.live()
        }
        .onReceive(NotificationCenter.default.publisher(for: .refreshInbox)) { _ in
            Task { await inbox.loadAll() }
        }
        .onReceive(NotificationCenter.default.publisher(for: .newConversation)) { _ in
            showingNew = true
        }
        .sheet(isPresented: $showingNew) {
            MacNewConversationSheet { peer in selection = peer }
        }
    }

    /// Which mailbox this window is reading, and a way to change it. On the phone this is a
    /// toolbar button; here it sits above the list, where a Mac user looks for the account.
    private var mailboxBar: some View {
        HStack(spacing: 8) {
            Text(account.me?.handle.map(PeerFace.displayName) ?? account.me?.label ?? "—")
                .font(.system(size: 12.5, weight: .semibold))
                .foregroundStyle(Theme.ink)
            Spacer()
            if account.mailboxes.count > 1 {
                Menu {
                    ForEach(account.mailboxes, id: \.address) { mailbox in
                        Button(mailbox.handle.map(PeerFace.displayName) ?? mailbox.label ?? mailbox.address) {
                            guard mailbox.address != account.me?.address else { return }
                            selection = nil
                            inbox.reset()
                            account.act(as: mailbox)
                        }
                    }
                } label: {
                    Image(systemName: "chevron.down")
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
    }
}

private struct MacConversationRow: View {
    let conversation: Conversation

    var body: some View {
        HStack(spacing: 9) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(conversation.name)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                    if conversation.held > 0 { PillView(text: "held", kind: .blocked) }
                }
                Text(conversation.messages.last.map(ConversationBuilder.preview) ?? "")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.muted)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
            if conversation.unread > 0 {
                Text("\(conversation.unread)")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Theme.navy, in: Capsule())
            }
        }
        .padding(.vertical, 3)
    }
}
