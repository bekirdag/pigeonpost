//  Conversations, their subjects, and the thread — three columns.
//
//  `NavigationSplitView` is what the phone already uses, and on the Mac it is the native shape
//  rather than an adaptation: real columns the person can resize, and a detail pane that does not
//  have to be pushed and popped. The middle column is the one the phone has no room for — subjects
//  were a strip across the top of the thread there, and indented under the conversation here, and
//  both were the wrong shape for what is plainly a list.

import AppKit
import SwiftUI

struct MacInboxView: View {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(PushService.self) private var push

    /// The conversation being read. The sidebar's selection.
    @State private var peer: String?
    /// The subject inside it. The middle column's selection; `nil` means the whole conversation.
    @State private var subthread: String?
    @State private var openingThread: ThreadTarget?
    /// The subject a right-click asked to delete, held until the question is answered. Deleting mail
    /// is not undoable and not something to do on one stray click.
    @State private var deletingThread: Subthread?
    @State private var showingNew = false
    /// Whether the sender search is open. Closed by default: the field is worth its row only while
    /// somebody is using it, and a permanently visible search box in a narrow column is mostly a
    /// permanently narrower column.
    @State private var searching = false
    /// Whether the mailbox list is open under the bar.
    @State private var switchingMailbox = false
    @State private var sheet: Sheet?

    /// A peer, wrapped so `.sheet(item:)` will take it. A bare `String?` is the natural shape and
    /// the one SwiftUI will not accept.
    private struct ThreadTarget: Identifiable { let id: String }

    private enum Sheet: String, Identifiable {
        case settings, peer
        var id: String { rawValue }
    }

    // The three columns are properties rather than one expression. Written inline, the type checker
    // gives up on the whole body — "unable to type-check this expression in reasonable time", and
    // then blames whichever line it happened to be looking at.
    var body: some View {
        @Bindable var inbox = inbox

        return NavigationSplitView {
            sidebar
        } content: {
            threadsColumn
        } detail: {
            detailColumn
        }
        // Empty on purpose. The sender is drawn as a button in the detail toolbar, and an
        // unset title falls back to the application's name — which put "Pigeonpost Desktop"
        // immediately beside "bdya" and made the bar read as two titles.
        .navigationTitle("")
        // Mail for another conversation, said inside the window rather than by Notification Centre.
        // A desktop notification is right when the app is behind something and wrong when it is in
        // front — see `Announcer`, and `Inbox.tell(about:)` for which of the two happens when.
        .announcements($inbox.announcement) { arrived in
            peer = arrived
            subthread = nil
        }
        .task {
            push.attach(to: account)
            push.attach(to: inbox)
            account.push = push
            // Asked on the way in rather than at first launch. A desktop app that wants to notify
            // you before it has shown you anything is a dialog people dismiss.
            await push.askIfNeeded()
            // The app is already long-polling the postbox, so it hears about mail before APNs would
            // and can say so with its own name and icon on the notification. Announcing it from
            // here is what replaced `osascript display notification`, which had neither and could
            // not be clicked.
            //
            // Only reached when the app is *behind* something — `Inbox` shows a line in the window
            // instead when it is in front. A desktop notification about a message you can already
            // see, in a window you are already looking at, is a thing to dismiss and nothing else.
            let mailbox = account.me.map { $0.handle ?? $0.label ?? $0.address } ?? ""
            inbox.onArrival = { arrivals in
                for message in arrivals {
                    LocalNotifier.announce(
                        title: PeerFace.displayName(message.peerKey),
                        subtitle: PeerFace.displayName(mailbox),
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
        .onChange(of: peer) { _, picked in
            inbox.reading = picked
            // A different conversation has its own subjects; carrying the last one's id across
            // would filter this one down to nothing.
            subthread = nil
        }
        // `initial: true` because the count is already right when this appears — the first listing
        // lands before anybody changes anything, and waiting for a change would leave the Dock
        // blank through the one moment somebody is most likely to look at it.
        .onChange(of: inbox.unreadCount, initial: true) { _, count in
            DockBadge.show(count)
            MenuBarItem.shared.show(unread: count)
        }
        // A clicked notification names the conversation it was about; opening anything else would
        // answer a different question from the one that was asked.
        .onChange(of: push.pendingPeer) { _, tapped in
            guard let tapped else { return }
            peer = tapped
            subthread = nil
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
            MacNewConversationSheet { started in
                peer = started
                subthread = nil
            }
        }
        .sheet(item: $openingThread) { target in
            MacNewThreadSheet(peer: target.id) { id in
                peer = target.id
                subthread = id
            }
        }
        .confirmationDialog(
            "Delete this thread?",
            isPresented: Binding(get: { deletingThread != nil }, set: { if !$0 { deletingThread = nil } }),
            presenting: deletingThread
        ) { thread in
            Button("Delete \(thread.name)", role: .destructive) {
                let peerForDelete = peer
                deletingThread = nil
                guard let peerForDelete else { return }
                if subthread == thread.id { subthread = nil }
                Task {
                    do { try await inbox.deleteThread(thread.id, with: peerForDelete) }
                    catch { inbox.toast = "Could not delete that thread." }
                }
            }
            Button("Cancel", role: .cancel) { deletingThread = nil }
        } message: { thread in
            Text("\(thread.messages.count) message\(thread.messages.count == 1 ? "" : "s") in “\(thread.name)” will be deleted from this mailbox. The other side keeps their copy.")
        }
        // One sheet at a time. Two `.sheet(isPresented:)` on one view are not reliably both
        // honoured — the phone lost Settings entirely to that shape once.
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
                        self.peer = nil
                        inbox.reset()
                    }
                    .frame(width: 520, height: 620)
                }
            }
        }
    }

    // ---- the columns ---------------------------------------------------------------------------

    /// Conversations, under the mailbox this window is reading.
    private var sidebar: some View {
        @Bindable var inbox = inbox
        return VStack(spacing: 0) {
            mailboxBar
            if switchingMailbox { mailboxList }
            // Its own row rather than a second line inside the bar. Everything about this column's
            // width has been fragile, and a flat stack of rows is the shape with the fewest
            // opinions in it.
            if searching {
                TextField("Search senders", text: $inbox.filter)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 12))
                    .padding(.horizontal, 10)
                    .padding(.bottom, 7)
                    // Escape closes it *and* clears it. A filter left behind by a box nobody can
                    // see is a sidebar quietly hiding conversations for no visible reason.
                    .onExitCommand {
                        inbox.filter = ""
                        searching = false
                    }
            }
            Divider()
            List(selection: $peer) {
                if inbox.offline {
                    Label("Not connected. Showing what was last loaded.", systemImage: "wifi.slash")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.muted)
                }
                ForEach(inbox.visible) { conversation in
                    MacConversationRow(conversation: conversation, isSelected: conversation.peer == peer)
                        .tag(conversation.peer)
                }
            }
            .listStyle(.sidebar)
            .scrollContentBackground(.hidden)
        }
        // Opaque, rather than the sidebar's usual vibrancy.
        //
        // A macOS sidebar is translucent by default and takes its tone from whatever is behind the
        // window. This app draws its own colours rather than the system's, so with a white window
        // behind it the material lightened while the text stayed light, and the names washed out.
        // Vibrancy only works when the text colour is the system's to adjust.
        .background(Theme.wash)
        .overlay(alignment: .trailing) { seam }
        .navigationSplitViewColumnWidth(min: 240, ideal: 300, max: 420)
        // In the window's own bar, beside the button that collapses this column — which is where a
        // Mac keeps the things that act on a window rather than on what is in it.
        //
        // In the toolbar rather than in a row inside the column, and that is not a style
        // preference: a `Button` anywhere in this column's header collapses the List beneath it and
        // every conversation name truncates to nothing. Four encounters with that now.
        .toolbar {
            ToolbarItemGroup(placement: .automatic) {
                Button {
                    searching.toggle()
                    if !searching { inbox.filter = "" }
                } label: {
                    Image(systemName: "magnifyingglass")
                }
                .help("Search senders")

                Button { showingNew = true } label: {
                    Image(systemName: "square.and.pencil")
                }
                .help("New conversation")

                Button { sheet = .settings } label: {
                    Image(systemName: "gearshape")
                }
                // Without this VoiceOver reads the symbol's own name, because the label is an image
                // with nothing to say. The other two get sensible names from the system.
                .accessibilityLabel("Settings")
                .help("Settings")
            }
        }
    }

    /// The subjects of the conversation being read, as a column of their own.
    ///
    /// Shaped after the web app's: the peer's name over the word "Threads", a `+` for a new one on
    /// the right of that header, and the subjects beneath. The `+` lives in the header rather than
    /// as a row in the list — a list of things is not the place to keep the button that makes
    /// another one.
    @ViewBuilder
    private var threadsColumn: some View {
        if let peer, let conversation = inbox.conversation(with: peer) {
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(conversation.name)
                            .font(.system(size: 12.5, weight: .semibold))
                            .foregroundStyle(Theme.ink)
                            .lineLimit(1)
                        Text("Threads")
                            .font(.system(size: 11))
                            .foregroundStyle(Theme.muted)
                    }
                    Spacer(minLength: 0)
                    // A tapped image, not a `Button`, for the reason in the toolbar note above.
                    Image(systemName: "plus")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Theme.muted)
                        .frame(width: 16, height: 16)
                        .contentShape(Rectangle())
                        .onTapGesture { openingThread = ThreadTarget(id: peer) }
                        .help("Start a thread about a new subject")
                        .accessibilityLabel("New thread")
                        .accessibilityAddTraits(.isButton)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                Divider()

                List(selection: $subthread) {
                    ForEach(inbox.subthreads(of: peer)) { thread in
                        MacThreadRow(thread: thread, isSelected: subthread == thread.id)
                            .tag(thread.id)
                            .contextMenu {
                                Button("Delete Thread", role: .destructive) {
                                    deletingThread = thread
                                }
                                // The default thread is where mail with no subject lands, and there
                                // is exactly one per correspondent. Deleting it would leave the next
                                // message with nowhere to go.
                                .disabled(thread.isDefault)
                            }
                    }
                }
                .listStyle(.sidebar)
                .scrollContentBackground(.hidden)
            }
            .background(Theme.wash)
            .overlay(alignment: .trailing) { seam }
            .navigationSplitViewColumnWidth(min: 170, ideal: 210, max: 340)
        } else {
            // Nothing picked yet. An empty column rather than a message: the sidebar beside it is
            // already saying what to do.
            Color.clear
                .background(Theme.wash)
                .overlay(alignment: .trailing) { seam }
                .navigationSplitViewColumnWidth(min: 170, ideal: 210, max: 340)
        }
    }

    /// The conversation itself, with who it is with drawn over it.
    @ViewBuilder
    private var detailColumn: some View {
        if let peer, let conversation = inbox.conversation(with: peer) {
            // Keyed on the peer alone. Changing subject inside one conversation is not a different
            // screen, and rebuilding it would throw away the scroll position along with the draft.
            MacThreadView(peer: peer, subthread: subthread)
                .id(peer)
                // Who you are writing to, over the conversation — and the name is the button, not a
                // label beside one. A name at the top of a conversation is the thing people click
                // to ask "who is this".
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

    // ---- pieces ---------------------------------------------------------------------------------

    /// The seam between two columns.
    ///
    /// The split view draws its own divider only while a column is the system's translucent
    /// material; painting them opaque takes that away, and two flat panels meeting with nothing
    /// between them read as one panel with a colour change in it.
    private var seam: some View {
        Rectangle()
            .fill(Theme.rule)
            .frame(width: 1)
            .ignoresSafeArea()
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
                        peer = nil
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

/// One subject, in the threads column.
private struct MacThreadRow: View {
    let thread: Subthread
    let isSelected: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Text(thread.name)
                    .font(.system(size: 12.5, weight: isSelected ? .semibold : .regular))
                    .foregroundStyle(isSelected ? Color.white : Theme.ink)
                    .lineLimit(1)
                Spacer(minLength: 0)
                if thread.unread > 0 {
                    Circle()
                        .fill(isSelected ? Color.white : Theme.blue)
                        .frame(width: 6, height: 6)
                        .fixedSize()
                }
            }
            if thread.last > 0 {
                Text(Time.listTime(thread.last))
                    .font(.system(size: 10.5))
                    .foregroundStyle(isSelected ? Color.white.opacity(0.75) : Theme.muted)
                    .fixedSize()
            }
        }
        .padding(.vertical, 3)
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
