import SwiftUI

@main
struct PigeonpostApp: App {
    @UIApplicationDelegateAdaptor(PushDelegate.self) private var pushDelegate
    @State private var session: Session
    @State private var account: Account
    @State private var inbox: Inbox
    @State private var push = PushDelegate.service
    @State private var calls = CallHost()
    // Phase 1b: call history, filtered mail and profile pictures, per acting mailbox.
    @State private var callLog: CallLog
    @State private var filtered: FilteredMail
    @State private var avatars: AvatarStore

    init() {
        let session = Session()
        let account = Account(session: session)
        let inbox = Inbox(account: account)
        _session = State(initialValue: session)
        _account = State(initialValue: account)
        _inbox = State(initialValue: inbox)
        let callLog = CallLog(account: account)
        let filtered = FilteredMail(account: account)
        _callLog = State(initialValue: callLog)
        _filtered = State(initialValue: filtered)
        _avatars = State(initialValue: AvatarStore(account: account))
        // The bars UIKit draws for SwiftUI's navigation titles, in the app's own type.
        let bar = UINavigationBar.appearance()
        bar.titleTextAttributes = [.font: Typeface.platform(16, .semibold)]
        bar.largeTitleTextAttributes = [.font: Typeface.platform(30, .semibold)]
        #if DEBUG
        if Fixtures.enabled {
            Fixtures.apply(session: session, account: account, inbox: inbox)
            // `-long` on top of them. Six short messages fit on one screen, so the fixture mailbox
            // cannot show what a long conversation does when it opens. See `LongThread`.
            LongThread.install(into: inbox)
            callLog.installFixture()
            filtered.installFixture()
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
                .environment(callLog)
                .environment(filtered)
                .environment(avatars)
                .environment(\.avatarLookup, avatars.lookup)
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
