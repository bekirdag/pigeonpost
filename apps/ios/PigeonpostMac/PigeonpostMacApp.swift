//  Pigeonpost on the Mac.
//
//  The same client as the phone — `Shared/` is compiled by both targets, not copied — in a window
//  that behaves like a Mac app. What differs here is only what genuinely differs on a desktop:
//  a resizable window with a real sidebar, a menu bar, and keyboard commands.

import AppKit
import SwiftUI

@main
struct PigeonpostMacApp: App {
    @NSApplicationDelegateAdaptor(MacPushDelegate.self) private var pushDelegate
    @State private var session = Session()
    @State private var account: Account
    @State private var inbox: Inbox
    @State private var calls = CallHost.shared
    @State private var groups: GroupStore
    @State private var callLog: CallLog
    @State private var filtered: FilteredMail
    @State private var avatars: AvatarStore
    @State private var calendar: CalendarStore

    init() {
        let session = Session()
        let account = Account(session: session)
        _session = State(initialValue: session)
        _account = State(initialValue: account)
        let inbox = Inbox(account: account)
        _inbox = State(initialValue: inbox)
        let groups = GroupStore(account: account)
        groups.inbox = inbox
        account.groups = groups
        _groups = State(initialValue: groups)
        let callLog = CallLog(account: account)
        let filtered = FilteredMail(account: account)
        _callLog = State(initialValue: callLog)
        _filtered = State(initialValue: filtered)
        _avatars = State(initialValue: AvatarStore(account: account))
        #if DEBUG
        _calendar = State(initialValue: CalendarStore(client: CalendarFixtures.client(account: account), now: CalendarFixtures.now))
        #else
        _calendar = State(initialValue: CalendarStore(client: account.client))
        #endif
        #if DEBUG
        // The phone's demo mailbox, for looking at the window without an account. Never touches the
        // Keychain or the network: see `Session.init` and the `Fixtures.enabled` guards on every
        // load and poll.
        if Fixtures.enabled {
            Fixtures.apply(session: session, account: account, inbox: inbox)
            LongThread.install(into: inbox)
            callLog.installFixture()
            filtered.installFixture()
        }
        #endif
    }

    var body: some Scene {
        WindowGroup {
            MacRootView()
                .calendarLifecycle()
                .environment(session)
                .environment(account)
                .environment(inbox)
                .environment(MacPushDelegate.service)
                .environment(calls)
                .environment(groups)
                .environment(callLog)
                .environment(filtered)
                .environment(avatars)
                .environment(calendar)
                .environment(\.avatarLookup, avatars.lookup)
                .appliesAppearanceChoice()
                .onOpenURL { url in
                    if let link = GroupStore.JoinLink(url: url) { groups.pendingJoin = link }
                }
                // Wide enough for a sidebar and a thread without either being useless. A window
                // that opens too narrow to show both is a split view nobody benefits from.
                .frame(minWidth: 900, minHeight: 560)
                #if DEBUG
                .task { StoreShotWindow.apply() }
                #endif
        }
        .defaultSize(width: 1100, height: 720)
        .commands {
            // The menu bar is not decoration on this platform: it is where a Mac user looks for
            // what an app can do, and what makes the keyboard usable without a mouse.
            CommandGroup(replacing: .newItem) {
                Button("New Conversation") { NotificationCenter.default.post(name: .newConversation, object: nil) }
                    .keyboardShortcut("n", modifiers: .command)
            }
            CommandGroup(after: .toolbar) {
                Button("Refresh") { NotificationCenter.default.post(name: .refreshInbox, object: nil) }
                    .keyboardShortcut("r", modifiers: .command)
            }
        }
    }
}

extension Notification.Name {
    static let newConversation = Notification.Name("pigeonpost.newConversation")
    static let refreshInbox = Notification.Name("pigeonpost.refreshInbox")
}

#if DEBUG
/// `-window=1440x900` (with `-fixtures`): the window at exactly that size in points, on a Retina
/// screen when there is one, so a window capture is a store-sized screenshot (2880×1800).
enum StoreShotWindow {
    @MainActor static func apply() {
        guard Fixtures.enabled,
              let spec = CommandLine.arguments.first(where: { $0.hasPrefix("-window=") })?.dropFirst("-window=".count),
              case let parts = spec.split(separator: "x").compactMap({ Double($0) }), parts.count == 2 else { return }
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard let window = NSApplication.shared.windows.first(where: { $0.isVisible && $0.canBecomeMain }) else { return }
            let screen = NSScreen.screens.first { $0.backingScaleFactor >= 2 } ?? window.screen ?? NSScreen.main
            guard let area = screen?.visibleFrame else { return }
            let size = NSSize(width: parts[0], height: parts[1])
            window.setFrame(NSRect(x: area.midX - size.width / 2, y: area.midY - size.height / 2,
                                   width: size.width, height: size.height), display: true)
        }
    }
}
#endif
