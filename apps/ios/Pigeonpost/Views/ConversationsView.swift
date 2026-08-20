//  The conversations, and the thread beside them.
//
//  A split view rather than a stack: on a phone it is one pane at a time, exactly as the web app is,
//  and on an iPad it is the two-pane messenger the web app becomes on a wide screen. The same view
//  code either way — the platform makes that decision better than a width check would.

import SwiftUI

struct ConversationsView: View {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(\.scenePhase) private var scenePhase

    @State private var selection: String?
    @State private var showingIdentities = false
    @State private var showingSettings = false
    @State private var showingNew = false

    var body: some View {
        @Bindable var inbox = inbox

        NavigationSplitView {
            list
                .navigationTitle(inbox.viewingArchive ? "Archive" : "Inbox")
                .navigationBarTitleDisplayMode(.inline)
                .searchable(text: $inbox.filter, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search")
                .toolbar { toolbar }
        } detail: {
            if let selection, inbox.conversation(with: selection) != nil {
                ThreadView(peer: selection)
                    .id(selection)
            } else {
                ContentUnavailableView("Pick a conversation", systemImage: "tray")
                    .background { DoodleBackground() }
            }
        }
        .navigationSplitViewStyle(.balanced)
        // The poll lives exactly as long as the app is in front of somebody. `.task(id:)` restarts
        // it when the mailbox changes and cancels it on the way to the background — a long poll held
        // open by a suspended app is a socket the system kills anyway, and one this app would then
        // believe in.
        .task(id: TaskKey(mailbox: account.me?.address, phase: scenePhase)) {
            if let peer = Fixtures.openPeer, selection == nil { selection = peer }
            switch Fixtures.sheet {
            case "settings": showingSettings = true
            case "new": showingNew = true
            case "identities": showingIdentities = true
            default: break
            }
            guard !Fixtures.enabled else { return }
            guard scenePhase == .active, account.me != nil else { return }
            await inbox.loadAll()
            await inbox.live()
        }
        .sheet(isPresented: $showingIdentities) {
            IdentityPickerSheet { mailbox in
                guard mailbox.address != account.me?.address else { return }
                selection = nil
                inbox.reset()
                account.act(as: mailbox)
            }
        }
        .sheet(isPresented: $showingSettings) { SettingsSheet() }
        .sheet(isPresented: $showingNew) {
            NewConversationSheet { peer in selection = peer }
        }
        .toast($inbox.toast)
    }

    /// What the live task is keyed on. Either changing means the old poll is wrong.
    private struct TaskKey: Equatable {
        let mailbox: String?
        let phase: ScenePhase
    }

    private var list: some View {
        List(selection: $selection) {
            if inbox.offline {
                Label("Not connected. Showing what was last loaded.", systemImage: "wifi.slash")
                    .font(.system(size: 12.5))
                    .foregroundStyle(Theme.muted)
                    .listRowBackground(Theme.wash)
            }
            ForEach(inbox.visible) { conversation in
                ConversationRow(conversation: conversation, isSelected: conversation.peer == selection)
                    .tag(conversation.peer)
                    .swipeActions(edge: .trailing) {
                        Button {
                            Task { await inbox.setArchived(conversation.peer, archived: !inbox.viewingArchive) }
                        } label: {
                            Label(inbox.viewingArchive ? "Unarchive" : "Archive", systemImage: "archivebox")
                        }
                        .tint(Theme.navy)
                    }
            }
        }
        .listStyle(.plain)
        .overlay { emptyState }
        .refreshable { await inbox.loadAll() }
    }

    @ViewBuilder
    private var emptyState: some View {
        if inbox.visible.isEmpty, inbox.hasLoaded {
            if !inbox.filter.isEmpty {
                ContentUnavailableView.search(text: inbox.filter)
            } else if inbox.viewingArchive {
                ContentUnavailableView("Nothing archived", systemImage: "archivebox",
                                       description: Text("Conversations you file out of sight land here."))
            } else {
                ContentUnavailableView("No mail yet", systemImage: "tray",
                                       description: Text("When an agent writes to this mailbox, it appears here."))
            }
        } else if inbox.loading {
            ProgressView()
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
            Button { showingIdentities = true } label: {
                HStack(spacing: 7) {
                    Avatar(peer: account.me?.key, size: 26)
                    Text(actingName)
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                    Image(systemName: "chevron.down")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.muted)
                }
            }
            .accessibilityLabel("Acting as \(actingName). Change mailbox")
        }
        ToolbarItemGroup(placement: .topBarTrailing) {
            Button { showingNew = true } label: { Image(systemName: "square.and.pencil") }
                .accessibilityLabel("New conversation")
            Button { showingSettings = true } label: { Image(systemName: "gearshape") }
                .accessibilityLabel("Settings")
        }
    }

    private var actingName: String {
        guard let me = account.me else { return "…" }
        if let handle = me.handle { return PeerFace.displayName(handle) }
        return me.label ?? PeerFace.displayName(me.address)
    }
}

struct ConversationRow: View {
    let conversation: Conversation
    /// A selected row is filled with the tint, which is the navy this app is drawn in. Text that
    /// stays dark on it is unreadable, and SwiftUI cannot invert it for us — it does that only for
    /// text whose colour it chose, and every colour here is chosen deliberately.
    var isSelected: Bool = false

    var body: some View {
        HStack(alignment: .top, spacing: 11) {
            Avatar(peer: conversation.peer)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(conversation.name)
                        .font(.system(size: 15.5, weight: .semibold))
                        .foregroundStyle(isSelected ? Color.white : Theme.ink)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if conversation.last > 0 {
                        Text(Time.listTime(conversation.last))
                            .font(.system(size: 11.5))
                            .foregroundStyle(isSelected ? Color.white.opacity(0.75) : Theme.muted)
                    }
                }
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(preview)
                        .font(.system(size: 13.5))
                        .foregroundStyle(isSelected ? Color.white.opacity(0.85) : Theme.body)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if conversation.held > 0 { PillView(text: "held", kind: .held) }
                    if conversation.isBlocked { PillView(text: "blocked", kind: .blocked) }
                    if conversation.unread > 0 {
                        // Navy on navy is a badge nobody can count. On a selected row the two swap.
                        Text("\(conversation.unread)")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(isSelected ? Theme.navy : Color.white)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(isSelected ? Color.white : Theme.navy, in: Capsule())
                    }
                }
            }
        }
        .padding(.vertical, 4)
    }

    /// A silent agent should say what it is, not "no messages yet" — the handle is the useful fact,
    /// and an unnamed mailbox is worth flagging because handle-based trust will never match it.
    private var preview: String {
        guard let last = conversation.messages.last else {
            if conversation.mine, conversation.identity?.handle == nil {
                return "No handle — fleet trust will not match it"
            }
            return conversation.peer
        }
        return (last.kind == .outgoing ? "You: " : "") + ConversationBuilder.preview(last)
    }
}
