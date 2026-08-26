//  Pigeonpost on the Mac.
//
//  The same client as the phone — `Shared/` is compiled by both targets, not copied — in a window
//  that behaves like a Mac app. What differs here is only what genuinely differs on a desktop:
//  a resizable window with a real sidebar, a menu bar, and keyboard commands.

import SwiftUI

@main
struct PigeonpostMacApp: App {
    @NSApplicationDelegateAdaptor(MacPushDelegate.self) private var pushDelegate
    @State private var session = Session()
    @State private var account: Account
    @State private var inbox: Inbox

    init() {
        let session = Session()
        let account = Account(session: session)
        _session = State(initialValue: session)
        _account = State(initialValue: account)
        _inbox = State(initialValue: Inbox(account: account))
    }

    var body: some Scene {
        WindowGroup {
            MacRootView()
                .environment(session)
                .environment(account)
                .environment(inbox)
                .environment(MacPushDelegate.service)
                // Wide enough for a sidebar and a thread without either being useless. A window
                // that opens too narrow to show both is a split view nobody benefits from.
                .frame(minWidth: 900, minHeight: 560)
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
