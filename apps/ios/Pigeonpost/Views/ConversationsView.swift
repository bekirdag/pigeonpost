//  The conversations, and the thread beside them.
//
//  Two shapes. A phone — and an iPad squeezed into a compact slice of the screen — gets the split
//  view that collapses to one pane at a time, exactly as the web app is on a narrow screen. An iPad
//  with room for it gets the desktop app's three columns: `DesktopInboxView`, the same code the Mac
//  runs. The size class decides, not the device, so an iPad dragged into Split View moves between
//  the two without losing what was open.

import SwiftUI

struct ConversationsView: View {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(PushService.self) private var push
    @Environment(\.scenePhase) private var scenePhase

    @Environment(\.horizontalSizeClass) private var sizeClass

    @State private var selection: String?
    /// The subject chosen in the desktop layout's middle column. The phone keeps its own, inside
    /// the thread.
    @State private var subthread: String?
    /// A mailbox chosen from a sender's panel, switched to once that panel has gone.
    @State private var pendingMailbox: Mailbox?
    /// One sheet at a time, named.
    ///
    /// This was four separate `.sheet(isPresented:)` modifiers stacked on the same view, and SwiftUI
    /// does not reliably honour that — it can end up presenting whichever it registered last and
    /// silently ignoring the rest, which is what happened to Settings: the button ran, the flag
    /// flipped, and nothing appeared. One `.sheet(item:)` has one presentation to resolve, so there
    /// is nothing left to race.
    @State private var sheet: Sheet?
    @State private var didStageFixtures = false

    private enum Sheet: Identifiable {
        case identities, settings, new, scanner
        case peer(Conversation)

        init?(staged: String) {
            switch staged {
            case "identities": self = .identities
            case "settings": self = .settings
            case "new": self = .new
            case "scanner": self = .scanner
            default: return nil
            }
        }

        var id: String {
            switch self {
            case .identities: return "identities"
            case .settings: return "settings"
            case .new: return "new"
            case .scanner: return "scanner"
            case .peer(let conversation): return "peer:" + conversation.peer
            }
        }
    }

    var body: some View {
        @Bindable var inbox = inbox

        layout
        // What is on screen is not news — the inbox drops arrivals for it rather than announcing
        // them, and until this was set that guard had never once been true on the phone.
        .onChange(of: selection, initial: true) { _, picked in
            inbox.reading = picked
        }
        // Mail for another conversation, said by the app rather than by the system. See
        // `Announcer`, and `PushService`'s delegate for the notifications this replaces.
        .announcements($inbox.announcement) { peer in selection = peer }
        // The poll lives exactly as long as the app is in front of somebody. `.task(id:)` restarts
        // it when the mailbox changes and cancels it on the way to the background — a long poll held
        // open by a suspended app is a socket the system kills anyway, and one this app would then
        // believe in.
        .task(id: TaskKey(mailbox: account.me?.address, phase: scenePhase)) {
            // Stage the launch destination once; switching inboxes must not reopen Settings.
            if !didStageFixtures {
                didStageFixtures = true
                #if DEBUG
                IOSUXFixtures.apply(account: account, inbox: inbox)
                #endif
                if let peer = Fixtures.openPeer, selection == nil { selection = peer }
                if let staged = Fixtures.sheet { sheet = Sheet(staged: staged) }
            }
            guard !Fixtures.enabled else { return }
            guard scenePhase == .active, account.me != nil else { return }
            // The badge counts what has not been looked at, so looking at the app clears it. And
            // re-register: the token is registered against the mailbox being read, which the
            // identity picker can change.
            push.clearBadge()
            await push.refreshRegistrationIfAuthorized()
            await push.register()
            await inbox.loadAll()
            await inbox.live()
        }
        // A tapped notification names the conversation it was about. Opening anything else would be
        // answering a different question from the one that was asked.
        .onChange(of: push.pendingPeer) { _, peer in
            guard let peer else { return }
            selection = peer
            push.pendingPeer = nil
        }
        .sheet(item: $sheet, onDismiss: finishMailboxVisit) { which in
            switch which {
            case .identities:
                IdentityPickerSheet { mailbox in visit(mailbox) }
            case .settings: SettingsSheet()
            case .scanner: ScanView()
            case .new: NewConversationSheet { peer in selection = peer }
            case .peer(let conversation):
                PeerInfoSheet(conversation: conversation) { mailbox in
                    pendingMailbox = mailbox
                    sheet = nil
                }
            }
        }
        .toast($inbox.toast)
    }

    @ViewBuilder
    private var layout: some View {
        if sizeClass == .regular {
            DesktopInboxView(
                peer: $selection,
                subthread: $subthread,
                newConversation: { sheet = .new },
                openSettings: { sheet = .settings },
                showPeer: { sheet = .peer($0) },
                visit: visit
            )
        } else {
            phone
        }
    }

    /// Reading as another of this account's mailboxes. Whatever was open belongs to the old one.
    private func visit(_ mailbox: Mailbox) {
        guard mailbox.address != account.me?.address else { return }
        selection = nil
        subthread = nil
        inbox.reset()
        account.act(as: mailbox)
    }

    private func finishMailboxVisit() {
        guard let mailbox = pendingMailbox else { return }
        pendingMailbox = nil
        guard account.mailboxes.contains(where: { $0.address == mailbox.address }) else { return }
        visit(mailbox)
    }

    private var phone: some View {
        @Bindable var inbox = inbox

        return NavigationSplitView {
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
            Button { sheet = .identities } label: {
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
        // Two trailing items, never three.
        //
        // With three, iOS decides for itself that they do not fit and folds the last of them into
        // an overflow of its own making — which on a real phone rendered Settings as "•••" that did
        // not open when tapped. The simulator, being wider, fitted all three and showed nothing
        // wrong, which is why this shipped.
        //
        // The scanner moved into Settings rather than into a menu here. It is a thing you do once
        // per machine and then forget exists, so it was never worth a permanent slot; and a plain
        // button that presents a sheet is one fewer presentation to go wrong than a menu that
        // presents a sheet.
        ToolbarItem(placement: .topBarTrailing) {
            Button { sheet = .new } label: { Image(systemName: "square.and.pencil") }
                .accessibilityLabel("New conversation")
        }
        ToolbarItem(placement: .topBarTrailing) {
            Button { sheet = .settings } label: { Image(systemName: "gearshape") }
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
                        UnreadBadge(count: conversation.unread, inverted: isSelected)
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
