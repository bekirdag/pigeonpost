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
    @Environment(GroupStore.self) private var groups
    @Environment(\.scenePhase) private var scenePhase
    @Environment(CallHost.self) private var calls
    // Phase 1b. Optional, so a host that does not provide them simply shows none of it.
    @Environment(CallLog.self) private var callLog: CallLog?
    @Environment(FilteredMail.self) private var filtered: FilteredMail?
    @Environment(CalendarStore.self) private var calendar: CalendarStore?
    @Environment(CalendarLinkRouter.self) private var calendarLinks: CalendarLinkRouter?
    @Environment(DeviceApprovalRouter.self) private var deviceApprovals: DeviceApprovalRouter?

    @Environment(\.horizontalSizeClass) private var sizeClass

    @State private var selection: String?
    /// The subject chosen in the desktop layout's middle column. The phone keeps its own, inside
    /// the thread.
    @State private var subthread: String?
    /// A mailbox chosen from a sender's panel, switched to once that panel has gone.
    @State private var pendingMailbox: Mailbox?
    @State private var pendingMeeting: (request: CallHost.ScheduledJoin, scope: UUID)?
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
        case identities, settings, new, scanner, newGroup, joinLink, calls, filtered, calendar, organizationRooms, meetingNotes
        case peer(Conversation)
        case join(GroupStore.JoinLink)
        case meetingLink(CalendarLinkRequest)
        case deviceApproval(DeviceApprovalRequest)

        init?(staged: String) {
            switch staged {
            case "identities": self = .identities
            case "settings", "privacy", "blocked", "profile": self = .settings
            case "calls": self = .calls
            case "filtered": self = .filtered
            case "new": self = .new
            case "scanner": self = .scanner
            case "new-group": self = .newGroup
            case "calendar": self = .calendar
            case "org-rooms": self = .organizationRooms
            case "meeting-notes": self = .meetingNotes
            case "device-approval": self = .deviceApproval(DeviceApprovalRequest(code: nil))
            default: return nil
            }
        }

        var id: String {
            switch self {
            case .identities: return "identities"
            case .settings: return "settings"
            case .new: return "new"
            case .scanner: return "scanner"
            case .newGroup: return "new-group"
            case .joinLink: return "join-link"
            case .join(let link): return "join:" + link.id
            case .calls: return "calls"
            case .filtered: return "filtered"
            case .calendar: return "calendar"
            case .organizationRooms: return "org-rooms"
            case .meetingNotes: return "meeting-notes"
            case .peer(let conversation): return "peer:" + conversation.peer
            case .meetingLink(let request): return "meeting-link:" + request.id.uuidString
            case .deviceApproval(let request): return "device-approval:" + request.id.uuidString
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
        // Groups: their own cadence (no event stream here), restarted with the mailbox and paused in
        // the background like the long poll.
        .task(id: GroupTaskKey(mailbox: account.me?.address, phase: scenePhase, on: account.features.groups, authorization: account.session.authorizationId, actor: account.actingGeneration)) {
            guard scenePhase == .active else { return }
            await groups.attach()
            async let cadence: Void = groups.live()
            async let expiry: Void = pruneDirectMessages()
            _ = await (cadence, expiry)
        }
        .onChange(of: account.session.authorizationId.uuidString + account.actingGeneration.uuidString) { _, _ in
            if selection?.hasPrefix("grp_") == true { selection = nil; subthread = nil }
            switch sheet {
            case .newGroup, .joinLink, .join: sheet = nil
            default: break
            }
        }
        .onChange(of: groups.pendingJoin, initial: true) { _, link in
            guard let link else { return }
            groups.pendingJoin = nil
            sheet = .join(link)
        }
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
            await refreshPhase1b()
            await inbox.live()
        }
        // A call that has just ended is a new row in the history, and maybe a missed one.
        .onChange(of: calls.mode) { _, mode in
            guard mode == .hidden, !Fixtures.enabled else { return }
            Task { await callLog?.refresh() }
        }
        // A tapped notification names the conversation it was about. Opening anything else would be
        // answering a different question from the one that was asked.
        .onChange(of: push.pendingPeer) { _, peer in
            guard let peer else { return }
            selection = peer
            push.pendingPeer = nil
        }
        .calendarNotices(canPresent: sheet == nil || sheet?.id == "calendar", open: { sheet = .calendar }, visit: visit)
        .calendarMeetingLinks(canPresent: sheet == nil) { sheet = .meetingLink($0) }
        .deviceApprovalLinks(canPresent: sheet == nil) { sheet = .deviceApproval($0) }
        .sheet(item: $sheet, onDismiss: finishMailboxVisit) { which in
            switch which {
            case .identities:
                IdentityPickerSheet { mailbox in visit(mailbox) }
            case .settings: SettingsSheet()
            case .deviceApproval(let request):
                if account.features.orgs { DeviceApprovalScreen(session: account.session, code: request.code).id(request.id) }
            case .calendar:
                if let calendar {
                    CalendarDashboard(store: calendar, joinMeeting: { request in
                        guard calendar.mailbox == request.mailbox, account.me?.address == request.mailbox else { return }
                        pendingMeeting = (request, calendar.scopeId)
                        sheet = nil
                    }).id(calendar.scopeId)
                }
            case .meetingLink(let link):
                CalendarLinkEntry(request: link, visit: visit) { request in
                    guard let calendar, calendar.mailbox == request.mailbox, account.me?.address == request.mailbox else { return }
                    pendingMeeting = (request, calendar.scopeId)
                    sheet = nil
                }
            case .scanner: ScanView()
            case .calls:
                if let callLog {
                    CallsSheet(callLog: callLog, open: { peer in
                        selection = peer
                        sheet = nil
                    }, callBack: callBack)
                }
            case .filtered:
                if let filtered { FilteredSheet(filtered: filtered) }
            case .new: NewConversationSheet { peer in selection = peer }
            case .newGroup: NewGroupSheet { id in selection = id }
            case .organizationRooms:
                if account.features.orgs && account.features.groups {
                    OrganizationRoomsScreen(account: account, visit: visit) { id in selection = id; subthread = nil }
                }
            case .meetingNotes:
                if account.features.meetingSummaries { MeetingNotesScreen() }
            case .joinLink: JoinLinkSheet { link in sheet = .join(link) }
            case .join(let link): JoinGroupSheet(link: link) { id in selection = id }
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
                visit: visit,
                newGroup: { sheet = .newGroup },
                joinLink: { sheet = .joinLink },
                openFiltered: { sheet = .filtered },
                openCalendar: { sheet = .calendar },
                openRooms: { sheet = .organizationRooms },
                openNotes: { sheet = .meetingNotes }
            )
        } else {
            phone
        }
    }

    /// The call history's badge and Filtered's senders, for the mailbox being read.
    private func refreshPhase1b() async {
        async let log: Void = callLog?.refresh() ?? ()
        async let mail: Void = filtered?.refresh() ?? ()
        _ = await (log, mail)
    }

    /// Ring somebody back from the call log: the sheet goes first, so the call is not under it.
    private func callBack(_ peer: String, _ video: Bool) {
        sheet = nil
        selection = peer
        Task {
            try? await Task.sleep(nanoseconds: 450_000_000)
            calls.call(peer, video: video)
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
        if let meeting = pendingMeeting {
            pendingMeeting = nil
            guard account.me?.address == meeting.request.mailbox, calendar?.scopeId == meeting.scope,
                  account.features.calendar else { return }
            guard calls.isAttached else { inbox.toast = "Calling isn’t ready. Reopen your inbox and try again."; return }
            Task { await calls.joinScheduled(meeting.request) }
            return
        }
        if calendarLinks?.finishScanner() == true { pendingMailbox = nil; return }
        if deviceApprovals?.finishScanner() == true { pendingMailbox = nil; return }
        guard let mailbox = pendingMailbox else { return }
        pendingMailbox = nil
        guard account.mailboxes.contains(where: { $0.address == mailbox.address }) else { return }
        visit(mailbox)
    }

    private var phone: some View {
        @Bindable var inbox = inbox

        return NavigationSplitView {
            list
                .safeAreaInset(edge: .top, spacing: 0) {
                    VStack(spacing: 0) {
                        if let calendar, account.features.calendar {
                            CalendarEntry(count: calendar.pendingCount) { sheet = .calendar }
                        }
                        if account.features.meetingSummaries {
                            MeetingNotesEntry { sheet = .meetingNotes }
                        }
                    }
                    .padding(.horizontal, 16).background(Theme.sidebar)
                }
                .navigationTitle(inbox.viewingArchive ? "Archive" : "Inbox")
                .navigationBarTitleDisplayMode(.inline)
                .searchable(text: $inbox.filter, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search")
                .toolbar { toolbar }
                .toolbarBackground(Theme.sidebar, for: .navigationBar)
                .toolbarBackground(.visible, for: .navigationBar)
        } detail: {
            if let selection, GroupSelection.isGroup(selection) {
                GroupView(groupId: selection)
                    .id(selection)
            } else if let selection, inbox.conversation(with: selection) != nil {
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
                    .font(.geist(12.5))
                    .foregroundStyle(Theme.muted)
                    .listRowBackground(Theme.wash)
            }
            if groups.enabled, !inbox.viewingArchive, inbox.filter.isEmpty {
                GroupInvitations()
                    .listRowBackground(Theme.sidebar)
                    .listRowSeparatorTint(Theme.rule)
            }
            ForEach(entries) { entry in
                switch entry {
                case let .group(group):
                    GroupRow(group: group, isSelected: group.id == selection)
                        .tag(group.id)
                        .listRowBackground(group.id == selection ? Theme.selected : Theme.sidebar)
                        .listRowSeparatorTint(Theme.rule)
                case let .conversation(conversation):
                    conversationRow(conversation)
                }
            }
            // Filtered mail, at the foot of the list: looked at on purpose, never among the rest.
            if let filtered, !inbox.viewingArchive, inbox.filter.isEmpty,
               case let senders = filtered.senders(contacts: inbox.contacts), !senders.isEmpty {
                Button { sheet = .filtered } label: { FilteredEntryRow(count: senders.count) }
                    .buttonStyle(.plain)
                    .listRowBackground(Theme.sidebar)
                    .listRowSeparatorTint(Theme.rule)
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(Theme.sidebar)
        .overlay { emptyState }
        .refreshable {
            await inbox.loadAll()
            await groups.refresh()
            await refreshPhase1b()
        }
    }

    /// Conversations and groups together, unless the archive (which holds no groups) is open.
    private var entries: [InboxEntry] {
        InboxEntry.merge(inbox.visible, groups.enabled && !inbox.viewingArchive ? groups.listed(filter: inbox.filter) : [])
    }

    private func pruneDirectMessages() async {
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            inbox.pruneExpired()
        }
    }

    private struct GroupTaskKey: Equatable {
        let mailbox: String?
        let phase: ScenePhase
        let on: Bool
        let authorization: UUID
        let actor: UUID
    }

    private func conversationRow(_ conversation: Conversation) -> some View {
        ConversationRow(conversation: conversation, isSelected: conversation.peer == selection)
            .tag(conversation.peer)
            .listRowBackground(conversation.peer == selection ? Theme.selected : Theme.sidebar)
            .listRowSeparatorTint(Theme.rule)
            .swipeActions(edge: .trailing) {
                Button {
                    Task { await inbox.setArchived(conversation.peer, archived: !inbox.viewingArchive) }
                } label: {
                    Label(inbox.viewingArchive ? "Unarchive" : "Archive", systemImage: "archivebox")
                }
                .tint(Theme.navy)
            }
    }

    @ViewBuilder
    private var emptyState: some View {
        if inbox.visible.isEmpty, inbox.hasLoaded, entries.isEmpty, groups.invitations.isEmpty || !groups.enabled {
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
                    Avatar(peer: account.me?.key, size: 28)
                    Text(actingName)
                        .font(.geistMono(14.5, .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        // Bounded, so a long handle cannot push the trailing buttons into an
                        // overflow menu of the system's own making (see below).
                        .frame(maxWidth: 132, alignment: .leading)
                    Image(systemName: "chevron.down")
                        .font(.geist(11, .semibold))
                        .foregroundStyle(Theme.secondary)
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
        // Calls, when the postbox keeps call history. A third trailing button, which is why the
        // mailbox name beside it is bounded: three fit on the narrowest phone with that limit, and
        // `PhoneTopBarTests` holds it to that.
        if let callLog, account.features.callHistory {
            ToolbarItem(placement: .topBarTrailing) {
                Button { sheet = .calls } label: { CallsButtonLabel(unseen: callLog.unseen) }
                    .accessibilityIdentifier("callsButton")
            }
        }
        ToolbarItem(placement: .topBarTrailing) {
            if groups.enabled || account.features.orgs {
                Menu {
                    Button { sheet = .new } label: { Label("New conversation", systemImage: "square.and.pencil") }
                    if groups.enabled {
                        Button { sheet = .newGroup } label: { Label("New group", systemImage: "person.3") }
                        Button { sheet = .joinLink } label: { Label("Join with a link", systemImage: "link") }
                        if account.features.orgs {
                            Button { sheet = .organizationRooms } label: { Label("Organization rooms", systemImage: "building.2") }
                                .accessibilityIdentifier("org-rooms-open")
                        }
                    }
                    if account.features.orgs {
                        Button { sheet = .deviceApproval(DeviceApprovalRequest(code: nil)) } label: { Label("Sign in a machine", systemImage: "desktopcomputer") }
                            .disabled(calls.mode != .hidden || calls.preparingScheduled)
                            .accessibilityIdentifier("device-approval-open")
                    }
                } label: {
                    Image(systemName: "square.and.pencil").foregroundStyle(Theme.secondary)
                }
                .accessibilityLabel("New conversation or group")
                .accessibilityIdentifier("composeMenu")
            } else {
                Button { sheet = .new } label: { Image(systemName: "square.and.pencil").foregroundStyle(Theme.secondary) }
                    .accessibilityLabel("New conversation")
            }
        }
        ToolbarItem(placement: .topBarTrailing) {
            Button { sheet = .settings } label: { Image(systemName: "gearshape").foregroundStyle(Theme.secondary) }
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
    /// A selected row sits on a quiet grey card (see the list), so its text keeps its colours.
    var isSelected: Bool = false

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            Avatar(peer: conversation.peer, size: 40, name: conversation.name)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(conversation.name)
                        .font(.geistMono(14.5, .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if conversation.last > 0 {
                        Text(Time.listTime(conversation.last))
                            .font(.geist(11.5))
                            .foregroundStyle(Theme.muted)
                    }
                }
                HStack(alignment: .center, spacing: 8) {
                    Text(preview)
                        .font(.geist(13.5))
                        .foregroundStyle(Theme.secondary)
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
