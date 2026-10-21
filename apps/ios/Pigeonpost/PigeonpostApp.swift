import SwiftUI

@main
struct PigeonpostApp: App {
    @UIApplicationDelegateAdaptor(PushDelegate.self) private var pushDelegate
    @State private var session: Session
    @State private var account: Account
    @State private var inbox: Inbox
    @State private var push = PushDelegate.service
    @State private var calls = CallHost.shared
    @State private var groups: GroupStore

    init() {
        let session = Session()
        let account = Account(session: session)
        let inbox = Inbox(account: account)
        _session = State(initialValue: session)
        _account = State(initialValue: account)
        _inbox = State(initialValue: inbox)
        let groups = GroupStore(account: account)
        groups.inbox = inbox
        account.groups = groups
        _groups = State(initialValue: groups)
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

    private func openJoinLink(_ url: URL) {
        if let link = GroupStore.JoinLink(url: url) { groups.pendingJoin = link }
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(session)
                .environment(account)
                .environment(inbox)
                .environment(push)
                .environment(calls)
                .environment(groups)
                .appliesAppearanceChoice()
                // Group join links (https://inbox.pigeonpost.dev/g/<id>#<token>), when universal
                // links reach the app, and the app's own scheme.
                .onOpenURL { url in openJoinLink(url) }
                .onContinueUserActivity(NSUserActivityTypeBrowsingWeb) { activity in
                    if let url = activity.webpageURL { openJoinLink(url) }
                }
                .task {
                    push.attach(to: account)
                    // And the inbox, which is what the notification delegate consults before it
                    // decides whether the system needs to say anything at all.
                    push.attach(to: inbox)
                    account.push = push
                    push.onGroupPush = { [weak groups] group in groups?.wake(group) }
                }
        }
    }
}
