//  The Mac's inbox window: the shared three columns, and everything around them that only a Mac
//  does — the Dock badge, the menu bar item, desktop notifications, the menu commands.
//
//  The columns themselves are `DesktopInboxView`, which an iPad with room for it uses too.

import AppKit
import SwiftUI

struct MacInboxView: View {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(PushService.self) private var push
    @Environment(GroupStore.self) private var groups
    @Environment(CallLog.self) private var callLog: CallLog?
    @Environment(FilteredMail.self) private var filtered: FilteredMail?
    @Environment(CalendarStore.self) private var calendar: CalendarStore?
    @Environment(CallHost.self) private var calls
    @Environment(DeviceApprovalRouter.self) private var deviceApprovals: DeviceApprovalRouter?

    /// The conversation being read. The sidebar's selection.
    @State private var peer: String?
    /// The subject inside it. The middle column's selection; `nil` means the whole conversation.
    @State private var subthread: String?
    @State private var showingNew = false
    @State private var sheet: Sheet?
    @State private var pendingMailbox: Mailbox?
    @State private var pendingMeeting: (request: CallHost.ScheduledJoin, scope: UUID)?

    private enum Sheet: Identifiable {
        case settings
        case filtered
        case peer(Conversation)
        case newGroup, joinLink, calendar, organizationRooms
        case join(GroupStore.JoinLink)
        case meetingLink(CalendarLinkRequest)
        case deviceApproval(DeviceApprovalRequest)

        var id: String {
            switch self {
            case .settings: return "settings"
            case .newGroup: return "new-group"
            case .joinLink: return "join-link"
            case .join(let link): return "join:" + link.id
            case .filtered: return "filtered"
            case .calendar: return "calendar"
            case .organizationRooms: return "org-rooms"
            case .peer(let conversation): return "peer:\(conversation.peer)"
            case .meetingLink(let request): return "meeting-link:" + request.id.uuidString
            case .deviceApproval(let request): return "device-approval:" + request.id.uuidString
            }
        }
    }

    var body: some View {
        @Bindable var inbox = inbox

        return DesktopInboxView(
            peer: $peer,
            subthread: $subthread,
            newConversation: { showingNew = true },
            openSettings: { sheet = .settings },
            showPeer: { sheet = .peer($0) },
            visit: visit,
            newGroup: { sheet = .newGroup },
            joinLink: { sheet = .joinLink },
            openFiltered: { sheet = .filtered },
            openCalendar: { sheet = .calendar },
            openRooms: { sheet = .organizationRooms }
        )
        // Groups keep their own cadence: the app holds no event stream (see GroupStore).
        .task(id: "\(account.me?.address ?? "")|\(account.features.groups)") {
            await groups.attach()
            await groups.live()
        }
        .onChange(of: groups.pendingJoin, initial: true) { _, link in
            guard let link else { return }
            groups.pendingJoin = nil
            sheet = .join(link)
        }
        // Mail for another conversation, said inside the window rather than by Notification Centre.
        // A desktop notification is right when the app is behind something and wrong when it is in
        // front — see `Announcer`, and `Inbox.tell(about:)` for which of the two happens when.
        .announcements($inbox.announcement) { arrived in
            peer = arrived
            subthread = nil
        }
        .task {
            if let staged = Fixtures.openPeer, peer == nil { peer = staged }
            switch Fixtures.sheet {
            case "settings", "privacy", "blocked", "profile": sheet = .settings
            case "filtered": sheet = .filtered
            case "new": showingNew = true
            case "new-group": sheet = .newGroup
            case "calendar": sheet = .calendar
            case "org-rooms" where account.features.orgs && account.features.groups: sheet = .organizationRooms
            case "device-approval" where account.features.orgs: sheet = .deviceApproval(DeviceApprovalRequest(code: nil))
            default: break
            }
            guard !Fixtures.enabled else { return }
            push.attach(to: account)
            push.attach(to: inbox)
            account.push = push
            push.onGroupPush = { [weak groups] group in groups?.wake(group) }
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
            inbox.onArrival = { arrivals in
                let mailbox = account.me?.key ?? ""
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
        }
        // A visit to another mailbox is a new load and a new poll. Keeping the previous task
        // alive leaves the window empty until that mailbox's long poll finally answers.
        .task(id: account.me?.address) {
            guard !Fixtures.enabled, let identity = account.me?.address else { return }
            peer = nil
            subthread = nil
            await inbox.loadAll()
            guard !Task.isCancelled, account.me?.address == identity else { return }
            await callLog?.refresh()
            await filtered?.refresh()
            await inbox.live()
        }
        // `initial: true` because the count is already right when this appears — the first listing
        // lands before anybody changes anything, and waiting for a change would leave the Dock
        // blank through the one moment somebody is most likely to look at it.
        .onChange(of: inbox.unreadCount + groups.unreadCount, initial: true) { _, count in
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
        .calendarNotices(canPresent: sheet == nil || sheet?.id == "calendar", open: {
            sheet = .calendar
            NSApplication.shared.activate(ignoringOtherApps: true)
        }, visit: visit)
        .calendarMeetingLinks(canPresent: sheet == nil) { request in
            sheet = .meetingLink(request)
            NSApplication.shared.activate(ignoringOtherApps: true)
        }
        .deviceApprovalLinks(canPresent: sheet == nil && !showingNew) { request in
            sheet = .deviceApproval(request)
            NSApplication.shared.activate(ignoringOtherApps: true)
        }
        .onReceive(NotificationCenter.default.publisher(for: .refreshInbox)) { _ in
            Task {
                await inbox.loadAll()
                await callLog?.refresh()
                await filtered?.refresh()
            }
        }
        // A call that has just ended is a new row in the history.
        .onChange(of: calls.mode) { _, mode in
            guard mode == .hidden, !Fixtures.enabled else { return }
            Task { await callLog?.refresh() }
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
        // One sheet at a time. Two `.sheet(isPresented:)` on one view are not reliably both
        // honoured — the phone lost Settings entirely to that shape once.
        .sheet(item: $sheet, onDismiss: finishMailboxVisit) { which in
            switch which {
            case .settings:
                // The phone's, now shared. A Mac sheet takes its size from its content, and a Form
                // with no width asks for the narrowest one that fits its longest label.
                SettingsSheet()
                    .frame(width: 520, height: 620)
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
            case .newGroup:
                NewGroupSheet { id in peer = id; subthread = nil }
                    .frame(width: 480, height: 600)
            case .organizationRooms:
                if account.features.orgs && account.features.groups {
                    OrganizationRoomsScreen(account: account, visit: visit) { id in peer = id; subthread = nil }
                }
            case .joinLink:
                JoinLinkSheet { link in sheet = .join(link) }
                    .frame(width: 460, height: 240)
            case .join(let link):
                JoinGroupSheet(link: link) { id in peer = id; subthread = nil }
                    .frame(width: 460, height: 360)
            case .filtered:
                if let filtered {
                    FilteredSheet(filtered: filtered)
                        .frame(width: 520, height: 620)
                }
            case .peer(let conversation):
                // Keep the presented content alive independently of inbox selection. Removing
                // its conversation while the sheet is open must not leave an empty modal panel.
                PeerInfoSheet(conversation: conversation) { mailbox in
                    pendingMailbox = mailbox
                    sheet = nil
                }
                .frame(width: 520, height: 620)
            }
        }
    }

    private func finishMailboxVisit() {
        if deviceApprovals?.finishScanner() == true { pendingMailbox = nil; return }
        if let meeting = pendingMeeting {
            pendingMeeting = nil
            guard account.me?.address == meeting.request.mailbox, calendar?.scopeId == meeting.scope,
                  account.features.calendar else { return }
            guard calls.isAttached else { inbox.toast = "Calling isn’t ready. Reopen your inbox and try again."; return }
            Task { await calls.joinScheduled(meeting.request) }
            return
        }
        guard let mailbox = pendingMailbox else { return }
        pendingMailbox = nil
        // Wait for AppKit to remove the modal sheet before replacing its underlying inbox.
        guard account.mailboxes.contains(where: { $0.address == mailbox.address }) else { return }
        visit(mailbox)
    }

    private func visit(_ mailbox: Mailbox) {
        guard mailbox.address != account.me?.address else { return }
        peer = nil
        subthread = nil
        inbox.reset()
        account.act(as: mailbox)
    }
}
