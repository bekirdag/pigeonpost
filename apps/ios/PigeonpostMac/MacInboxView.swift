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
            // Stacked above the List rather than inset into its safe area. `.safeAreaInset` would
            // read better, but a plain VStack is the shape whose width behaviour is obvious, and
            // this column has already cost enough guessing — see the note on the Menu below.
            VStack(spacing: 0) {
                mailboxBar
                Divider()
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
                .listStyle(.sidebar)
            }
            .searchable(text: $inbox.filter, prompt: "Search")
            .navigationSplitViewColumnWidth(min: 240, ideal: 300, max: 420)
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
                // A definite width, and never `.fixedSize()`.
                //
                // `.fixedSize()` here is what squeezed the whole sidebar. It makes the Menu ask for
                // its ideal size, and that unspecified-width proposal came back out of this bar and
                // became the width the List proposed to every row — so the conversation names
                // truncated to "Pig…" while the column sat 300pt wide. Three attempts went into the
                // row before a plain `Text` in its place truncated identically and pointed here.
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .frame(width: 16)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
    }
}

private struct MacConversationRow: View {
    let conversation: Conversation

    var body: some View {
        // The spacers live *inside* the two rows, never beside the column that holds them. Put one
        // next to the column and it takes the width instead: the name and preview collapse to two
        // characters and an ellipsis while the sidebar sits there half empty. That is what the Mac
        // build did, and the iOS row had the right shape all along.
        HStack(alignment: .top, spacing: 9) {
            Avatar(peer: conversation.peer, size: 26)
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(conversation.name)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if conversation.last > 0 {
                        Text(Time.listTime(conversation.last))
                            .font(.system(size: 10.5))
                            .foregroundStyle(Theme.muted)
                            .fixedSize()
                    }
                }
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(conversation.messages.last.map(ConversationBuilder.preview) ?? "")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.body)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    // The accessories are rigid and the text beside them is what gives way. Say it
                    // this way round rather than with a `.layoutPriority` on the preview: priority
                    // hands the winner everything it asks for first, which left the unread count a
                    // one-pixel blue sliver at the edge of the column.
                    if conversation.held > 0 { PillView(text: "held", kind: .held).fixedSize() }
                    if conversation.unread > 0 { UnreadBadge(count: conversation.unread) }
                }
            }
        }
        // Take the full row so the whole strip is the click target, not just the text. This is not
        // what fixed the truncation — that was a `.fixedSize()` two views up — but a sidebar row
        // should still fill its column.
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .padding(.vertical, 3)
    }
}
