//  Conversations, their subjects, and the thread — three columns.
//
//  The desktop shape, shared by the Mac and by an iPad with room for it. It began as the Mac's
//  `MacInboxView`; the columns, the rows, the mailbox switcher and the thread menu are the same code
//  on both now, and only what genuinely differs is fenced: the Mac's thread pane (a composer that
//  sends on Return, a find bar, drag and drop) against the phone's (a photo picker, a paged
//  history), and a few AppKit-only modifiers.
//
//  What stays with each platform's caller is the lifecycle around it — the long poll, push
//  registration, the Dock badge, Settings and the new-conversation sheet — because those were
//  already different, and a shared view that owned them would be a third copy of each.
//
//  `NavigationSplitView` is the native shape on both: real columns, and a detail pane that does not
//  have to be pushed and popped. The middle column is the one a phone has no room for — subjects
//  are a strip across the top of the thread there, which is the wrong shape for what is plainly a
//  list once there is room for a list.

import SwiftUI

struct DesktopInboxView: View {
    /// The conversation being read. The sidebar's selection.
    @Binding var peer: String?
    /// The subject inside it. The middle column's selection; `nil` means the whole conversation.
    @Binding var subthread: String?
    let newConversation: () -> Void
    let openSettings: () -> Void
    let showPeer: (Conversation) -> Void
    /// Switch to another of this account's mailboxes. The caller resets what it owns.
    let visit: (Mailbox) -> Void

    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox

    @State private var openingThread: ThreadTarget?
    /// The subject a right-click asked to delete, held until the question is answered. Deleting mail
    /// is not undoable and not something to do on one stray click.
    @State private var deletingThread: Subthread?
    /// Whether the sender search is open. Closed by default: the field is worth its row only while
    /// somebody is using it, and a permanently visible search box in a narrow column is mostly a
    /// permanently narrower column.
    @State private var searching = false
    /// Whether the mailbox list is open under the bar.
    @State private var switchingMailbox = false
    /// All three columns, always. An iPad in portrait would otherwise fold the sidebar away and
    /// open on a thread with nothing beside it, which is not the desktop layout at all.
    @State private var columns = NavigationSplitViewVisibility.all
    /// The window's width. Under 1000 pt — every iPad in portrait but the 13-inch — the subjects
    /// column takes its narrowest width, or the conversation itself is left a 210 pt strip beside
    /// the iPad's 320 pt sidebar (iPad mini), its title truncated and its composer wrapping word by
    /// word.
    @State private var windowWidth: CGFloat = 0
    private var roomy: Bool { windowWidth == 0 || windowWidth >= 1000 }

    /// A peer, wrapped so `.sheet(item:)` will take it. A bare `String?` is the natural shape and
    /// the one SwiftUI will not accept.
    private struct ThreadTarget: Identifiable { let id: String }

    // The three columns are properties rather than one expression. Written inline, the type checker
    // gives up on the whole body — "unable to type-check this expression in reasonable time", and
    // then blames whichever line it happened to be looking at.
    var body: some View {
        NavigationSplitView(columnVisibility: $columns) {
            sidebar
        } content: {
            threadsColumn
        } detail: {
            detailColumn
                #if os(iOS)
                // The way back. On an iPad the system's own toggle lives in the leftmost visible
                // column's bar — and the subjects column hides its bar — so once the sidebar was
                // folded away there was nothing on screen to bring it back.
                .toolbar {
                    if columns != .all {
                        ToolbarItem(placement: .topBarLeading) {
                            Button {
                                withAnimation { columns = .all }
                            } label: {
                                Image(systemName: "sidebar.left")
                            }
                            .accessibilityLabel("Show conversations")
                            .accessibilityIdentifier("showSidebar")
                        }
                    }
                }
                #endif
        }
        .navigationSplitViewStyle(.balanced)
        .background {
            GeometryReader { geometry in
                Color.clear
                    .onAppear { windowWidth = geometry.size.width }
                    .onChange(of: geometry.size.width) { _, width in windowWidth = width }
            }
        }
        #if os(macOS)
        // The window's bar in the conversation's own ground, as the conversation design's header
        // is, instead of the system's grey strip.
        .toolbarBackground(Theme.wash, for: .windowToolbar)
        #endif
        // Empty on purpose. The sender is drawn as a button in the detail toolbar, and an
        // unset title falls back to the application's name — which put "Pigeonpost Desktop"
        // immediately beside "bdya" and made the bar read as two titles.
        .navigationTitle("")
        .onChange(of: peer) { _, picked in
            inbox.reading = picked
            inbox.readingSubthread = nil
            // A different conversation has its own subjects; carrying the last one's id across
            // would filter this one down to nothing.
            subthread = nil
        }
        .onChange(of: subthread) { _, picked in inbox.readingSubthread = picked }
        .onChange(of: account.me?.address) { _, _ in
            openingThread = nil
            deletingThread = nil
        }
        .sheet(item: $openingThread) { target in
            NewThreadSheet(peer: target.id) { id in
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
    }

    // ---- the columns ---------------------------------------------------------------------------

    /// Conversations, under the mailbox this window is reading.
    private var sidebar: some View {
        @Bindable var inbox = inbox
        return VStack(spacing: 0) {
            mailboxBar
            if switchingMailbox { mailboxList }
            if inbox.loading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text(inbox.hasLoaded ? "Updating inbox…" : "Loading inbox…")
                        .font(.geist(12))
                        .foregroundStyle(Theme.muted)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("inboxLoadingStatus")
            }
            // Its own row rather than a second line inside the bar. Everything about this column's
            // width has been fragile, and a flat stack of rows is the shape with the fewest
            // opinions in it.
            if searching {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass")
                        .font(.geist(13, .medium))
                        .foregroundStyle(Theme.muted)
                    TextField("Search senders", text: $inbox.filter)
                        .textFieldStyle(.plain)
                        .font(.geist(13.5))
                        .foregroundStyle(Theme.ink)
                }
                .padding(.horizontal, 10)
                .frame(height: 36)
                .background(Theme.field, in: RoundedRectangle(cornerRadius: 9))
                .padding(.horizontal, 12)
                .padding(.bottom, 10)
                    // Escape closes it *and* clears it. A filter left behind by a box nobody can
                    // see is a sidebar quietly hiding conversations for no visible reason.
                    #if os(macOS)
                    .onExitCommand {
                        inbox.filter = ""
                        searching = false
                    }
                    #endif
            }
            conversationList
            // Which build this window is. Settings carries it too, but a released app is asked
            // "is the fix in this one?" far more often than Settings is opened, and the answer was
            // not visible anywhere in the window.
            Text("Pigeonpost \(Config.version)")
                .font(.geist(12))
                .foregroundStyle(Theme.muted)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .overlay(alignment: .top) { Rectangle().fill(Theme.rule).frame(height: 1) }
                .textSelection(.enabled)
                .accessibilityIdentifier("appVersion")
        }
        // Opaque, rather than the sidebar's usual vibrancy.
        //
        // A macOS sidebar is translucent by default and takes its tone from whatever is behind the
        // window. This app draws its own colours rather than the system's, so with a white window
        // behind it the material lightened while the text stayed light, and the names washed out.
        // Vibrancy only works when the text colour is the system's to adjust.
        .background(Theme.sidebar)
        .overlay(alignment: .trailing) { seam }
        .navigationSplitViewColumnWidth(min: 240, ideal: 300, max: 420)
        #if os(iOS)
        .toolbarBackground(Theme.sidebar, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        // The phone's word for the one state the sidebar cannot otherwise show: that it is the
        // archive rather than the inbox. Settings is where it is opened.
        .navigationTitle(inbox.viewingArchive ? "Archive" : "")
        .navigationBarTitleDisplayMode(.inline)
        // One pair of controls, ours: hide here, show from the thread's bar.
        .toolbar(removing: .sidebarToggle)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button {
                    withAnimation { columns = .detailOnly }
                } label: {
                    Image(systemName: "sidebar.left")
                }
                .accessibilityLabel("Hide conversations")
                .accessibilityIdentifier("hideSidebar")
            }
        }
        #endif
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
                        .foregroundStyle(Theme.secondary)
                }
                .help("Search senders")
                .accessibilityLabel("Search senders")

                Button(action: newConversation) {
                    Image(systemName: "square.and.pencil")
                        .foregroundStyle(Theme.secondary)
                }
                .help("New conversation")
                .accessibilityLabel("New conversation")

                Button(action: openSettings) {
                    Image(systemName: "gearshape")
                        .foregroundStyle(Theme.secondary)
                }
                // Without this VoiceOver reads the symbol's own name, because the label is an image
                // with nothing to say.
                .accessibilityLabel("Settings")
                .help("Settings")
            }
        }
    }

    /// The conversations themselves.
    ///
    /// Rows choose themselves rather than through `List(selection:)`, so the selected row can be
    /// drawn in this app's colours: a selection-driven list paints the system accent behind the row
    /// on both platforms, and on the Mac there is no way to ask it not to.
    private var conversationList: some View {
        List {
            if inbox.offline, inbox.hasLoaded {
                VStack(alignment: .leading, spacing: 6) {
                    Label("Not connected. Showing what was last loaded.", systemImage: "wifi.slash")
                        .font(.geist(12))
                        .foregroundStyle(Theme.muted)
                    Button("Try again") { Task { await inbox.loadAll() } }
                        .disabled(inbox.loading)
                }
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
            }
            ForEach(inbox.visible) { conversation in
                let isSelected = conversation.peer == peer
                DesktopConversationRow(conversation: conversation, isSelected: isSelected)
                    .selectable(isSelected, radius: 10) { peer = conversation.peer }
                    .accessibilityIdentifier("conversation:" + conversation.peer)
                    #if os(iOS)
                    .swipeActions(edge: .trailing) {
                        Button {
                            Task { await inbox.setArchived(conversation.peer, archived: !inbox.viewingArchive) }
                        } label: {
                            Label(inbox.viewingArchive ? "Unarchive" : "Archive", systemImage: "archivebox")
                        }
                        .tint(Theme.navy)
                    }
                    #endif
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        #if os(iOS)
        .refreshable { await inbox.loadAll() }
        #else
        // The arrow keys still walk the list, as they did while the system owned the selection.
        .focusable()
        .focusEffectDisabled()
        .onMoveCommand { direction in
            let peers = inbox.visible.map(\.peer)
            guard !peers.isEmpty else { return }
            let at = peer.flatMap { peers.firstIndex(of: $0) }
            switch direction {
            case .down: peer = peers[min((at ?? -1) + 1, peers.count - 1)]
            case .up: peer = peers[max((at ?? peers.count) - 1, 0)]
            default: break
            }
        }
        #endif
        .overlay { emptyConversations }
    }

    @ViewBuilder
    private var emptyConversations: some View {
        if inbox.hasLoaded, inbox.visible.isEmpty {
            #if os(iOS)
            if !inbox.filter.isEmpty {
                ContentUnavailableView.search(text: inbox.filter)
            } else if inbox.viewingArchive {
                ContentUnavailableView("Nothing archived", systemImage: "archivebox",
                                       description: Text("Conversations you file out of sight land here."))
            } else {
                ContentUnavailableView("No mail yet", systemImage: "tray",
                                       description: Text("When an agent writes to this mailbox, it appears here."))
            }
            #else
            ContentUnavailableView(
                inbox.filter.isEmpty ? "No conversations yet" : "No matching conversations",
                systemImage: "tray",
                description: Text(inbox.filter.isEmpty
                    ? "Messages for this inbox will appear here."
                    : "Try a different sender or search term.")
            )
            #endif
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
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Threads")
                            .font(.geist(11))
                            .textCase(.uppercase)
                            .kerning(0.66)
                            .foregroundStyle(Theme.muted)
                        Text(conversation.name)
                            .font(.geistMono(12.5))
                            .foregroundStyle(Theme.quiet)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: 0)
                    // A tapped image, not a `Button`, for the reason in the toolbar note above.
                    Image(systemName: "plus")
                        .font(.geist(15, .medium))
                        .foregroundStyle(Theme.ink)
                        .frame(width: 32, height: 32)
                        .background(Theme.field, in: RoundedRectangle(cornerRadius: 8))
                        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.strokeStrong, lineWidth: 1))
                        .contentShape(Rectangle())
                        .onTapGesture { openingThread = ThreadTarget(id: peer) }
                        .help("Start a thread about a new subject")
                        .accessibilityLabel("New thread")
                        .accessibilityAddTraits(.isButton)
                }
                .padding(.leading, 18)
                .padding(.trailing, 12)
                .frame(height: 64)
                .overlay(alignment: .bottom) { Rectangle().fill(Theme.rule).frame(height: 1) }

                List {
                    ForEach(inbox.subthreads(of: peer)) { thread in
                        let isSelected = subthread == thread.id
                        DesktopThreadRow(thread: thread, isSelected: isSelected)
                            .selectable(isSelected, radius: 9) { subthread = isSelected ? nil : thread.id }
                            .accessibilityIdentifier("subthread:" + thread.name)
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
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
                .contentMargins(.top, 10, for: .scrollContent)
            }
            .background(Theme.column)
            .overlay(alignment: .trailing) { seam }
            .onAppear { if Fixtures.sheet == "thread" { openingThread = ThreadTarget(id: peer) } }
            .navigationSplitViewColumnWidth(min: 170, ideal: roomy ? 210 : 170, max: 340)
            #if os(iOS)
            .toolbar(.hidden, for: .navigationBar)
            #endif
        } else {
            // Nothing picked yet. An empty column rather than a message: the sidebar beside it is
            // already saying what to do.
            Color.clear
                .background(Theme.column)
                .overlay(alignment: .trailing) { seam }
                .navigationSplitViewColumnWidth(min: 170, ideal: roomy ? 210 : 170, max: 340)
                #if os(iOS)
                .toolbar(.hidden, for: .navigationBar)
                #endif
        }
    }

    /// The conversation itself, with who it is with drawn over it.
    @ViewBuilder
    private var detailColumn: some View {
        if !inbox.hasLoaded {
            VStack(spacing: 14) {
                if inbox.offline, !inbox.loading {
                    Image(systemName: "wifi.exclamationmark")
                        .font(.geist(30))
                        .foregroundStyle(Theme.muted)
                    Text("Couldn't load this inbox")
                        .font(.headline)
                    Text("Check your connection and try again. You can also open another inbox.")
                        .foregroundStyle(Theme.muted)
                        .multilineTextAlignment(.center)
                    Button("Try again") { Task { await inbox.loadAll() } }
                        .buttonStyle(.borderedProminent)
                        .tint(Theme.navy)
                        .accessibilityIdentifier("retryInboxLoad")
                } else {
                    ProgressView("Loading inbox…")
                        .controlSize(.large)
                        .accessibilityIdentifier("inboxLoading")
                    Text("Fetching conversations and inbox details.")
                        .foregroundStyle(Theme.muted)
                }
                if let address = account.me?.key {
                    Text(address)
                        .font(.callout)
                        .foregroundStyle(Theme.muted)
                        .textSelection(.enabled)
                }
            }
            .padding(30)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let peer, let conversation = inbox.conversation(with: peer) {
            // Keyed on the peer alone. Changing subject inside one conversation is not a different
            // screen, and rebuilding it would throw away the scroll position along with the draft.
            threadPane(peer: peer)
                .id(peer)
                // Who you are writing to, over the conversation — and the name is the button, not a
                // label beside one. A name at the top of a conversation is the thing people click
                // to ask "who is this".
                .toolbar {
                    ToolbarItem(placement: .navigation) {
                        Button { showPeer(conversation) } label: {
                            HStack(spacing: 10) {
                                Avatar(peer: conversation.peer, size: 28)
                                Text(conversation.name)
                                    .font(.geistMono(14, .semibold))
                                    .foregroundStyle(Theme.ink)
                                    .lineLimit(1)
                            }
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("About \(conversation.name)")
                        .help("About this sender")
                        .keyboardShortcut("i", modifiers: .command)
                    }
                }
        } else if inbox.conversations.isEmpty {
            ContentUnavailableView("Inbox is up to date", systemImage: "checkmark.circle",
                                   description: Text("Start a conversation or open another inbox."))
        } else {
            ContentUnavailableView("Pick a conversation", systemImage: "tray")
        }
    }

    /// The one part of the window that is genuinely each platform's own.
    @ViewBuilder
    private func threadPane(peer: String) -> some View {
        #if os(macOS)
        MacThreadView(peer: peer, subthread: subthread)
        #else
        ThreadView(peer: peer, subthread: $subthread)
        #endif
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
        HStack(spacing: 10) {
            Avatar(peer: account.me?.key, size: 30)
            Text(account.me?.handle.map(PeerFace.displayName) ?? account.me?.label ?? "—")
                .font(.geistMono(13, .medium))
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 0)
            if account.mailboxes.count > 1 {
                Image(systemName: "chevron.down")
                    .font(.geist(12, .semibold))
                    .foregroundStyle(Theme.secondary)
                    .rotationEffect(.degrees(switchingMailbox ? 180 : 0))
                    .fixedSize()
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(Theme.field.opacity(0.9), in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.stroke, lineWidth: 1))
        .contentShape(Rectangle())
        .onTapGesture {
            guard account.mailboxes.count > 1 else { return }
            switchingMailbox.toggle()
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("mailboxSwitcher")
        .padding(.horizontal, 12)
        .padding(.top, 12)
        .padding(.bottom, 12)
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
                    HStack(spacing: 4) {
                        HStack(spacing: 8) {
                            Avatar(peer: mailbox.handle ?? mailbox.address, size: 24)
                            Text(mailbox.handle.map(PeerFace.displayName) ?? mailbox.label ?? mailbox.address)
                                .font(.geistMono(12.5))
                                .foregroundStyle(Theme.ink)
                                .lineLimit(1)
                            Spacer(minLength: 0)
                            if isCurrent {
                                Image(systemName: "checkmark")
                                    .font(.geist(10, .semibold))
                                    .foregroundStyle(Theme.navy)
                                    .fixedSize()
                            }
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 6)
                        .background(isCurrent ? Theme.selected : Color.clear, in: RoundedRectangle(cornerRadius: 8))
                        .contentShape(Rectangle())
                        .onTapGesture { switchTo(mailbox) }
                        .accessibilityElement(children: .combine)
                        .accessibilityAddTraits(.isButton)
                        .accessibilityIdentifier("mailbox:" + mailbox.key)
                        CopyAddressButton(address: mailbox.key)
                    }
                    .padding(.horizontal, 8)
                }
            }
        }
        .frame(maxHeight: 260)
        .padding(.bottom, 10)
    }

    private func switchTo(_ mailbox: Mailbox) {
        switchingMailbox = false
        guard mailbox.address != account.me?.address else { return }
        openingThread = nil
        deletingThread = nil
        visit(mailbox)
    }
}

private extension View {
    /// A list row that chooses itself when tapped, drawn selected in this app's own colours: a
    /// quiet grey card behind the row, as in the conversation design, rather than a fill of the
    /// accent.
    func selectable(_ isSelected: Bool, radius: CGFloat, choose: @escaping () -> Void) -> some View {
        self
            .padding(.horizontal, 10)
            .padding(.vertical, radius == 10 ? 10 : 9)
            .background(isSelected ? Theme.selected : Color.clear, in: RoundedRectangle(cornerRadius: radius))
            .contentShape(Rectangle())
            .onTapGesture(perform: choose)
            .listRowInsets(EdgeInsets(top: 1, leading: 8, bottom: 1, trailing: 8))
            .listRowBackground(Color.clear)
            .listRowSeparator(.hidden)
            .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
    }
}

/// One subject, in the threads column.
private struct DesktopThreadRow: View {
    let thread: Subthread
    let isSelected: Bool

    /// A subject nobody named is drawn as the placeholder it is.
    private var unnamed: Bool { (thread.title ?? "").isEmpty && !thread.isDefault }

    var body: some View {
        HStack(spacing: 10) {
            Text(thread.name)
                .font(.geist(13.5, isSelected ? .medium : .regular))
                .italic(unnamed)
                .foregroundStyle(unnamed ? Theme.secondary : Theme.ink)
                .lineLimit(1)
            Spacer(minLength: 0)
            if thread.unread > 0 {
                Circle()
                    .fill(Theme.accent)
                    .frame(width: 6, height: 6)
                    .fixedSize()
            }
            if thread.last > 0 {
                Text(Time.listTime(thread.last))
                    .font(.geist(11.5))
                    .foregroundStyle(Theme.muted)
                    .fixedSize()
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct DesktopConversationRow: View {
    let conversation: Conversation
    var isSelected: Bool = false

    var body: some View {
        // The spacers live *inside* the two rows, never beside the column that holds them. Put one
        // next to the column and it takes the width instead: the name and preview collapse to two
        // characters and an ellipsis while the sidebar sits there half empty.
        HStack(alignment: .center, spacing: 11) {
            Avatar(peer: conversation.peer, size: 38)
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(conversation.name)
                        .font(.geistMono(13, .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if conversation.last > 0 {
                        Text(Time.listTime(conversation.last))
                            .font(.geist(11.5))
                            .foregroundStyle(Theme.muted)
                            .fixedSize()
                    }
                }
                HStack(alignment: .center, spacing: 8) {
                    Text(conversation.messages.last.map(ConversationBuilder.preview) ?? "")
                        .font(.geist(13))
                        .foregroundStyle(Theme.secondary)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    // The accessories are rigid and the text beside them is what gives way. Say it
                    // this way round rather than with a `.layoutPriority` on the preview: priority
                    // hands the winner everything it asks for first, which left the unread count a
                    // one-pixel blue sliver at the edge of the column.
                    if conversation.held > 0 { PillView(text: "held", kind: .held).fixedSize() }
                    if conversation.isBlocked { PillView(text: "blocked", kind: .blocked).fixedSize() }
                    if conversation.unread > 0 {
                        UnreadBadge(count: conversation.unread, inverted: isSelected)
                    }
                }
            }
        }
        // Take the full row so the whole strip is the click target, not just the text.
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
