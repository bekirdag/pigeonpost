import SwiftUI

@main
struct PigeonpostApp: App {
    @UIApplicationDelegateAdaptor(PushDelegate.self) private var pushDelegate
    @State private var session: Session
    @State private var account: Account
    @State private var inbox: Inbox
    @State private var push = PushDelegate.service
    @State private var calls = CallHost.shared

    init() {
        let session = Session()
        let account = Account(session: session)
        let inbox = Inbox(account: account)
        _session = State(initialValue: session)
        _account = State(initialValue: account)
        _inbox = State(initialValue: inbox)
        // The bars UIKit draws for SwiftUI's navigation titles, in the app's own type.
        let bar = UINavigationBar.appearance()
        bar.titleTextAttributes = [.font: Typeface.platform(16, .semibold)]
        bar.largeTitleTextAttributes = [.font: Typeface.platform(30, .semibold)]
        // Before any window: a call answered from the lock screen of a phone woken by a VoIP push
        // has to sign the call page in with these, and there may never be a window for it.
        CallAlerts.shared.adopt(session: session, account: account, inbox: inbox)
        #if DEBUG
        if Fixtures.enabled {
            Fixtures.apply(session: session, account: account, inbox: inbox)
            // `-long` on top of them. Six short messages fit on one screen, so the fixture mailbox
            // cannot show what a long conversation does when it opens. See `LongThread`.
            LongThread.install(into: inbox)
        }
        #endif
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(session)
                .environment(account)
                .environment(inbox)
                .environment(push)
                .environment(calls)
                .appliesAppearanceChoice()
                .task {
                    push.attach(to: account)
                    // And the inbox, which is what the notification delegate consults before it
                    // decides whether the system needs to say anything at all.
                    push.attach(to: inbox)
                    account.push = push
                }
        }
    }
}
