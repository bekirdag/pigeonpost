//  Conversations on the left, the thread on the right.
//
//  `NavigationSplitView` is what the phone already uses, and on the Mac it is the native shape
//  rather than an adaptation — a real sidebar the person can resize, and a detail pane that does
//  not have to be pushed and popped.

import AppKit
import SwiftUI

struct MacInboxView: View {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(PushService.self) private var push
    /// What is picked in the sidebar. A conversation, or one subject inside one — the column shows
    /// both, so the selection has to be able to say which.
    @State private var selection: SidebarItem?
    @State private var openingThread: ThreadTarget?

    /// A peer, wrapped so `.sheet(item:)` will take it. A bare `String?` is the natural shape and
    /// the one SwiftUI will not accept.
    private struct ThreadTarget: Identifiable { let id: String }

    private enum SidebarItem: Hashable {
        case conversation(String)
        case thread(peer: String, id: String)

        var peer: String {
            switch self {
            case let .conversation(peer): return peer
            case let .thread(peer, _): return peer
            }
        }

        var thread: String? {
            switch self {
            case .conversation: return nil
            case let .thread(_, id): return id
            }
        }
    }

    private var peer: String? { selection?.peer }
    private var subthread: String? { selection?.thread }
    @State private var showingNew = false
    /// Whether the sender search is open. Closed by default: the field is worth its row only while
    /// somebody is using it, and a permanently visible search box in a narrow column is mostly a
    /// permanently narrower column.
    @State private var searching = false
    /// Whether the mailbox list is open under the bar.
    @State private var switchingMailbox = false
    @State private var sheet: Sheet?

    private enum Sheet: String, Identifiable {
        case settings, peer
        var id: String { rawValue }
    }

    var body: some View {
        @Bindable var inbox = inbox

        NavigationSplitView {
            // Stacked above the List rather than inset into its safe area. `.safeAreaInset` would
            // read better, but a plain VStack is the shape whose width behaviour is obvious, and
            // this column has already cost enough guessing — see the note on the Menu below.
            VStack(spacing: 0) {
                sidebarActions
                mailboxBar
                if switchingMailbox { mailboxList }
                // Its own row rather than a second line inside the bar. Everything about this
                // column's width has been fragile, and a flat stack of rows is the shape with the
                // fewest opinions in it.
                if searching {
                    TextField("Search senders", text: $inbox.filter)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 12))
                        .padding(.horizontal, 10)
                        .padding(.bottom, 7)
                        // Escape closes it *and* clears it. A filter left behind by a box nobody
                        // can see is a sidebar quietly hiding conversations for no visible reason.
                        .onExitCommand {
                            inbox.filter = ""
                            searching = false
                        }
                }
                Divider()
                List(selection: $selection) {
                    if inbox.offline {
                        Label("Not connected. Showing what was last loaded.", systemImage: "wifi.slash")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.muted)
                    }
                    ForEach(inbox.visible) { conversation in
                        rows(for: conversation)
                    }
                }
                .listStyle(.sidebar)
                .scrollContentBackground(.hidden)
            }
            // Opaque, rather than the sidebar's usual vibrancy.
            //
            // A macOS sidebar is translucent by default and takes its tone from whatever is behind
            // the window. This app draws its own colours rather than the system's, so with a white
            // window behind it the material lightened while the text stayed light, and the names
            // washed out. Vibrancy only works when the text colour is the system's to adjust.
            .background(Theme.wash)
            // The seam between the sidebar and the conversation. The split view draws its own
            // divider only while the sidebar is the system's translucent material; painting it
            // opaque takes that away, and two flat panels meeting with nothing between them read as
            // one panel with a colour change in it.
            .overlay(alignment: .trailing) {
                Rectangle()
                    .fill(Theme.rule)
                    .frame(width: 1)
                    .ignoresSafeArea()
            }
            .navigationSplitViewColumnWidth(min: 240, ideal: 300, max: 420)
        } detail: {
            if let peer, let conversation = inbox.conversation(with: peer) {
                // Keyed on the peer alone. Changing subject inside one conversation is not a
                // different screen, and rebuilding it would throw away the scroll position along
                // with the draft.
                MacThreadView(peer: peer, subthread: subthread)
                    .id(peer)
                    // Who you are writing to, over the conversation rather than in the sidebar's
                    // toolbar — and the name is the button, not a label beside one. A name at the
                    // top of a conversation is the thing people click to ask "who is this".
                    .toolbar {
                        ToolbarItem(placement: .navigation) {
                            Button { sheet = .peer } label: {
                                HStack(spacing: 6) {
                                    Avatar(peer: conversation.peer, size: 18)
                                    Text(conversation.name)
                                        .font(.system(size: 13, weight: .semibold))
                                        .foregroundStyle(Theme.ink)
                                }
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("About \(conversation.name)")
                            .help("About this sender")
                        }
                    }
            } else {
                ContentUnavailableView("Pick a conversation", systemImage: "tray")
            }
        }
        .task {
            push.attach(to: account)
            account.push = push
            // Asked on the way in rather than at first launch. A desktop app that wants to notify
            // you before it has shown you anything is a dialog people dismiss.
            await push.askIfNeeded()
            // The app is already long-polling the postbox, so it hears about mail before APNs would
            // and can say so with its own name and icon on the notification. Announcing it from
            // here is what replaced `osascript display notification`, which had neither and could
            // not be clicked.
            inbox.onArrival = { arrivals in
                for message in arrivals {
                    LocalNotifier.announce(
                        title: PeerFace.displayName(message.peerKey),
                        body: ConversationBuilder.preview(body: message.body),
                        peer: message.peerKey,
                        messageId: message.messageId
                    )
                }
            }
            await inbox.loadAll()
            await inbox.live()
        }
        // What is on screen is not news. Told to the inbox rather than checked in the closure
        // above, because that closure is captured once and would go on believing whatever was
        // selected the moment it was made.
        .onChange(of: selection) { _, picked in
            inbox.reading = picked?.peer
        }
        // `initial: true` because the count is already right when this appears — the first listing
        // lands before anybody changes anything, and waiting for a change would leave the Dock
        // blank through the one moment somebody is most likely to look at it.
        .onChange(of: inbox.unreadCount, initial: true) { _, count in
            DockBadge.show(count)
        }
        // A clicked notification names the conversation it was about; opening anything else would
        // answer a different question from the one that was asked.
        .onChange(of: push.pendingPeer) { _, peer in
            guard let peer else { return }
            selection = .conversation(peer)
            push.pendingPeer = nil
            NSApplication.shared.activate(ignoringOtherApps: true)
        }
        .onReceive(NotificationCenter.default.publisher(for: .refreshInbox)) { _ in
            Task { await inbox.loadAll() }
        }
        .onReceive(NotificationCenter.default.publisher(for: .newConversation)) { _ in
            showingNew = true
        }
        .sheet(isPresented: $showingNew) {
            MacNewConversationSheet { peer in selection = .conversation(peer) }
        }
        // One sheet at a time. Two `.sheet(isPresented:)` on one view are not reliably both
        // honoured — the phone lost Settings entirely to that shape once.
        .sheet(item: $openingThread) { target in
            MacNewThreadSheet(peer: target.id) { id in
                selection = .thread(peer: target.id, id: id)
            }
        }
        .sheet(item: $sheet) { which in
            switch which {
            case .settings:
                // The phone's, now shared. A Mac sheet takes its size from its content, and a Form
                // with no width asks for the narrowest one that fits its longest label.
                SettingsSheet()
                    .frame(width: 520, height: 620)
            case .peer:
                if let peer, let conversation = inbox.conversation(with: peer) {
                    PeerInfoSheet(conversation: conversation) { mailbox in
                        account.act(as: mailbox)
                        selection = nil
                        inbox.reset()
                    }
                    .frame(width: 520, height: 620)
                }
            }
        }
    }

    /// Which mailbox this window is reading, and a way to change it. On the phone this is a
    /// toolbar button; here it sits above the list, where a Mac user looks for the account.
    /// A conversation, and — when it is the one being read — its subjects beneath it.
    ///
    /// Pulled out of `body` because the whole sidebar in one expression is more than the type
    /// checker will take: it gave up with "unable to type-check this expression in reasonable
    /// time", which is the compiler asking for exactly this.
    @ViewBuilder
    private func rows(for conversation: Conversation) -> some View {
        MacConversationRow(conversation: conversation, isSelected: conversation.peer == peer)
            .tag(SidebarItem.conversation(conversation.peer))

        // The subjects were a strip across the top of the thread. Here they are where a Mac user
        // looks for a hierarchy, and they cost the conversation none of its own height.
        if conversation.peer == peer {
            let threads = inbox.subthreads(of: conversation.peer)
            if threads.count > 1 {
                ForEach(threads) { thread in
                    MacThreadRow(thread: thread, isSelected: subthread == thread.id)
                        .tag(SidebarItem.thread(peer: conversation.peer, id: thread.id))
                }
            }
            newThreadRow(for: conversation.peer)
        }
    }

    /// Not a `Button`. One in this column has collapsed the whole sidebar twice; a List row is
    /// already tappable and needs no help.
    private func newThreadRow(for peer: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: "plus").font(.system(size: 10, weight: .semibold))
            Text("New thread").font(.system(size: 12))
            Spacer(minLength: 0)
        }
        .foregroundStyle(Theme.muted)
        .padding(.leading, 35)
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .onTapGesture { openingThread = ThreadTarget(id: peer) }
    }

    /// Search, compose and settings, at the top left of the column.
    ///
    /// Tapped images rather than `Button`s, and that is load-bearing: a real button anywhere in
    /// this header collapses the List beneath it, every conversation name truncating to nothing. It
    /// has happened three times. A `contentShape` and an `onTapGesture` do the same job and ask the
    /// layout for nothing.
    private var sidebarActions: some View {
        HStack(spacing: 14) {
            sidebarAction("magnifyingglass", "Search senders", active: searching) {
                searching.toggle()
                if !searching { inbox.filter = "" }
            }
            sidebarAction("square.and.pencil", "New conversation") { showingNew = true }
            sidebarAction("gearshape", "Settings") { sheet = .settings }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .padding(.top, 8)
        .padding(.bottom, 2)
    }

    private func sidebarAction(
        _ symbol: String,
        _ label: String,
        active: Bool = false,
        action: @escaping () -> Void
    ) -> some View {
        Image(systemName: symbol)
            .font(.system(size: 13))
            .foregroundStyle(active ? Theme.navy : Theme.muted)
            .frame(width: 16, height: 16)
            .contentShape(Rectangle())
            .onTapGesture(perform: action)
            .help(label)
            .accessibilityLabel(label)
            .accessibilityAddTraits(.isButton)
    }

    /// Which mailbox is being read, and the way to change it.
    ///
    /// The whole row is the target and there is no `Menu`: a menu here popped a floating list over
    /// the window, and one of its modifiers collapsed the column beneath it for a while. The web
    /// app drops its identity list *inside* the sidebar, spanning the column, and that is both the
    /// nicer shape and the one that cannot fight this List for width.
    private var mailboxBar: some View {
        HStack(spacing: 8) {
            Text(account.me?.handle.map(PeerFace.displayName) ?? account.me?.label ?? "—")
                .font(.system(size: 12.5, weight: .semibold))
                .foregroundStyle(Theme.ink)
            Spacer(minLength: 0)
            if account.mailboxes.count > 1 {
                Image(systemName: "chevron.down")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Theme.muted)
                    .rotationEffect(.degrees(switchingMailbox ? 180 : 0))
                    .fixedSize()
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .contentShape(Rectangle())
        .onTapGesture {
            guard account.mailboxes.count > 1 else { return }
            switchingMailbox.toggle()
        }
    }

    /// The other mailboxes on this account, as rows in the column rather than a popup over it.
    private var mailboxList: some View {
        // Capped and scrolling, the way the web app caps its identity list at 60vh. An account with
        // seventeen mailboxes would otherwise push the conversations off the bottom of the column,
        // which is the opposite of what opening a switcher is for.
        ScrollView {
            VStack(spacing: 0) {
                ForEach(account.mailboxes, id: \.address) { mailbox in
                    let isCurrent = mailbox.address == account.me?.address
                    HStack(spacing: 8) {
                        Avatar(peer: mailbox.handle ?? mailbox.address, size: 20)
                        Text(mailbox.handle.map(PeerFace.displayName) ?? mailbox.label ?? mailbox.address)
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.ink)
                            .lineLimit(1)
                        Spacer(minLength: 0)
                        if isCurrent {
                            Image(systemName: "checkmark")
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle(Theme.navy)
                                .fixedSize()
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 5)
                    .contentShape(Rectangle())
                    .onTapGesture {
                        switchingMailbox = false
                        guard !isCurrent else { return }
                        selection = nil
                        inbox.reset()
                        account.act(as: mailbox)
                    }
                }
            }
        }
        .frame(maxHeight: 260)
        .padding(.bottom, 4)
    }
}

/// One subject inside a conversation, in the sidebar under the conversation it belongs to.
///
/// Indented to the width of the avatar beside it, so the subjects line up with the name above them
/// rather than with the picture.
private struct MacThreadRow: View {
    let thread: Subthread
    let isSelected: Bool

    var body: some View {
        HStack(spacing: 6) {
            Text(thread.name)
                .font(.system(size: 12, weight: isSelected ? .semibold : .regular))
                .foregroundStyle(isSelected ? Color.white : Theme.body)
                .lineLimit(1)
            Spacer(minLength: 0)
            if thread.unread > 0 {
                Circle()
                    .fill(isSelected ? Color.white : Theme.blue)
                    .frame(width: 6, height: 6)
                    .fixedSize()
            }
        }
        .padding(.leading, 35)
        .padding(.vertical, 2)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }
}

private struct MacConversationRow: View {
    let conversation: Conversation
    /// A selected row is filled with the accent, and text that stays as it was is unreadable on it.
    /// SwiftUI inverts only the colours it chose itself, and every colour here was chosen
    /// deliberately, so the row has to do it.
    var isSelected: Bool = false

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
                        .foregroundStyle(isSelected ? Color.white : Theme.ink)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if conversation.last > 0 {
                        Text(Time.listTime(conversation.last))
                            .font(.system(size: 10.5))
                            .foregroundStyle(isSelected ? Color.white.opacity(0.75) : Theme.muted)
                            .fixedSize()
                    }
                }
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(conversation.messages.last.map(ConversationBuilder.preview) ?? "")
                        .font(.system(size: 12))
                        .foregroundStyle(isSelected ? Color.white.opacity(0.85) : Theme.body)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    // The accessories are rigid and the text beside them is what gives way. Say it
                    // this way round rather than with a `.layoutPriority` on the preview: priority
                    // hands the winner everything it asks for first, which left the unread count a
                    // one-pixel blue sliver at the edge of the column.
                    if conversation.held > 0 { PillView(text: "held", kind: .held).fixedSize() }
                    if conversation.unread > 0 {
                        UnreadBadge(count: conversation.unread, inverted: isSelected)
                    }
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
