import SwiftUI

@main
struct PigeonpostApp: App {
    @UIApplicationDelegateAdaptor(PushDelegate.self) private var pushDelegate
    @State private var session: Session
    @State private var account: Account
    @State private var inbox: Inbox
    @State private var push = PushDelegate.service

    init() {
        let session = Session()
        let account = Account(session: session)
        let inbox = Inbox(account: account)
        _session = State(initialValue: session)
        _account = State(initialValue: account)
        _inbox = State(initialValue: inbox)
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
