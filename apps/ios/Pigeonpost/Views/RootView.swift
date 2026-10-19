import SwiftUI

struct RootView: View {
    @Environment(Session.self) private var session
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(CallHost.self) private var calls

    var body: some View {
        Group {
            switch session.status {
            case .signedOut:
                SignInView()
            case .signedIn:
                if account.me != nil {
                    ConversationsView()
                } else {
                    opening
                }
            }
        }
        .tint(Theme.navy)
        // A minimised call is a strip at the top; the app moves down for it rather than under it.
        // Padding rather than a safe-area inset: the split view's UIKit bars take their safe area
        // from the window and ignore SwiftUI's.
        .padding(.top, calls.mode == .mini ? 64 : 0)
        // Calls sit over everything, the sign-in included, so ending one is never hidden behind a
        // screen change.
        .overlay { CallOverlay() }
        .task(id: callIdentity) {
            if Fixtures.enabled && !CallHost.debugEnabled { return }
            if session.status == .signedIn, account.me != nil {
                calls.attach(session: session, account: account, inbox: inbox)
            } else {
                calls.detach()
            }
        }
        .task(id: session.status) {
            guard !Fixtures.enabled else { return }
            guard session.status == .signedIn, account.me == nil else { return }
            await account.loadIdentities()
        }
    }

    /// Who the call client should be: a change of mailbox, or signing out, re-tells it.
    private var callIdentity: String {
        session.status == .signedIn ? (account.me?.address ?? "") : ""
    }

    /// The seconds between a token and a mailbox. It is two calls — the account's mailboxes, then
    /// each one's handle — so it is worth saying something rather than showing an empty inbox that
    /// is not yet known to be empty.
    ///
    /// A brand-new account passes through here and does not come out: it authenticates fine and
    /// owns nothing, so the load succeeds with an empty list and there is no mailbox to open. That
    /// was an unending spinner. It is not a wait — the load has finished — and it is not an error,
    /// so it gets its own screen and the one call that ends it.
    @ViewBuilder
    private var opening: some View {
        switch account.load {
        case .failed(let message):
            VStack(spacing: 14) {
                Text(message)
                    .font(.geist(14))
                    .foregroundStyle(Theme.body)
                    .multilineTextAlignment(.center)
                Button("Try again") { Task { await account.loadIdentities() } }
                    .font(.geist(15, .semibold))
                Button("Sign out") { Task { await account.signOut() } }
                    .font(.geist(14))
                    .foregroundStyle(Theme.muted)
            }
            .padding(30)
        // Reached only while `me` is nil, so a finished load here means there is nothing to open.
        case .ready:
            firstMailbox
        default:
            ProgressView()
        }
    }

    /// Signed in, with nothing to open yet. The web app's words, so both clients say the same thing.
    @ViewBuilder
    private var firstMailbox: some View {
        VStack(spacing: 12) {
            Text("Almost there")
                .font(.geist(22, .bold))
                .foregroundStyle(Theme.ink)
            Text("You are signed in, but have no inbox yet.")
                .font(.geist(15))
                .foregroundStyle(Theme.body)
                .multilineTextAlignment(.center)
                .padding(.bottom, 6)

            Button {
                Task { await account.createFirstMailbox() }
            } label: {
                Text(account.creating ? "Creating your inbox…" : "Create my inbox")
                    .font(.geist(16, .semibold))
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 13)
            }
            .buttonStyle(.plain)
            .background(Theme.accent, in: RoundedRectangle(cornerRadius: 10))
            .foregroundStyle(Theme.onAccent)
            .disabled(account.creating)

            Button("Sign out") { Task { await account.signOut() } }
                .font(.geist(14))
                .foregroundStyle(Theme.muted)
                .disabled(account.creating)
                .padding(.top, 2)
        }
        .frame(maxWidth: 320)
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.app)
    }
}
