import SwiftUI

struct RootView: View {
    @Environment(Session.self) private var session
    @Environment(Account.self) private var account

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
        .task(id: session.status) {
            guard !Fixtures.enabled else { return }
            guard session.status == .signedIn, account.me == nil else { return }
            await account.loadIdentities()
        }
    }

    /// The seconds between a token and a mailbox. It is two calls — the account's mailboxes, then
    /// each one's handle — so it is worth saying something rather than showing an empty inbox that
    /// is not yet known to be empty.
    @ViewBuilder
    private var opening: some View {
        switch account.load {
        case .failed(let message):
            VStack(spacing: 14) {
                Text(message)
                    .font(.system(size: 14))
                    .foregroundStyle(Theme.body)
                    .multilineTextAlignment(.center)
                Button("Try again") { Task { await account.loadIdentities() } }
                    .font(.system(size: 15, weight: .semibold))
                Button("Sign out") { Task { await account.signOut() } }
                    .font(.system(size: 14))
                    .foregroundStyle(Theme.muted)
            }
            .padding(30)
        default:
            ProgressView()
        }
    }
}
