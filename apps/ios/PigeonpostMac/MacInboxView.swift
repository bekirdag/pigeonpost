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
    @Environment(CallLog.self) private var callLog: CallLog?
    @Environment(FilteredMail.self) private var filtered: FilteredMail?
    @Environment(CallHost.self) private var calls

    /// The conversation being read. The sidebar's selection.
    @State private var peer: String?
    /// The subject inside it. The middle column's selection; `nil` means the whole conversation.
    @State private var subthread: String?
    @State private var showingNew = false
    @State private var sheet: Sheet?
    @State private var pendingMailbox: Mailbox?

    private enum Sheet: Identifiable {
        case settings
        case filtered
        case peer(Conversation)

        var id: String {
            switch self {
            case .settings: return "settings"
            case .filtered: return "filtered"
            case .peer(let conversation): return "peer:\(conversation.peer)"
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
            openFiltered: { sheet = .filtered }
        )
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
            default: break
            }
            guard !Fixtures.enabled else { return }
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
