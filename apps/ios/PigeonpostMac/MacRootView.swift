//  Signed out, opening, or the inbox — the same three states the phone has, said the Mac's way.

import SwiftUI

struct MacRootView: View {
    @Environment(Session.self) private var session
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(CallHost.self) private var calls
    @Environment(AvatarStore.self) private var avatars: AvatarStore?

    var body: some View {
        Group {
            switch session.status {
            case .signedOut:
                MacSignInView()
            case .signedIn:
                if account.me == nil {
                    opening
                } else {
                    MacInboxView()
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.app)
        .tint(Theme.accent)
        // A minimised call is a strip along the top of the window; the inbox moves down for it
        // rather than sliding under it.
        .padding(.top, calls.mode == .mini ? 64 : 0)
        // Calls sit over everything, the sign-in included, so ending one is never hidden behind a
        // screen change.
        .overlay { CallOverlay() }
        // A call on screen has the whole window: the inbox's toolbar would otherwise sit over it,
        // offering to compose or search mail underneath a call.
        .toolbar(calls.mode == .full ? .hidden : .automatic, for: .windowToolbar)
        .task(id: callIdentity) {
            if Fixtures.enabled && !CallHost.debugEnabled { return }
            if session.status == .signedIn, account.me != nil {
                calls.avatars = avatars
                calls.attach(session: session, account: account, inbox: inbox)
            } else {
                calls.detach()
                // Another account may sign in next; what this one could see is not theirs.
                if session.status == .signedOut { avatars?.reset() }
            }
        }
        .task(id: session.status) {
            guard !Fixtures.enabled, session.status == .signedIn else { return }
            await account.loadIdentities()
        }
    }

    /// Who the call client should be: a change of mailbox, or signing out, re-tells it.
    private var callIdentity: String {
        session.status == .signedIn ? (account.me?.address ?? "") : ""
    }

    @ViewBuilder
    private var opening: some View {
        switch account.load {
        case .failed(let why):
            VStack(spacing: 12) {
                Text(why)
                    .font(.geist(13))
                    .foregroundStyle(Theme.body)
                    .multilineTextAlignment(.center)
                Button("Try again") { Task { await account.loadIdentities() } }
                Button("Sign out") { Task { await account.signOut() } }
                    .buttonStyle(.plain)
                    .foregroundStyle(Theme.muted)
            }
            .padding(30)
        case .ready:
            // Signed in, owning nothing. The phone answers this with a button that mints the first
            // mailbox; the Mac says the same thing, because an account with no mailbox is a real
            // state and an endless spinner is not an answer to it.
            VStack(spacing: 12) {
                Text("You are signed in, but have no inbox yet.")
                    .font(.geist(15, .semibold))
                    .foregroundStyle(Theme.ink)
                Text("A mailbox is where your agents reach you.")
                    .font(.geist(13))
                    .foregroundStyle(Theme.body)
                Button("Create my inbox") { Task { await account.createFirstMailbox() } }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(30)
        default:
            ProgressView()
        }
    }
}

struct MacSignInView: View {
    @Environment(Session.self) private var session
    @State private var working = false

    var body: some View {
        VStack(spacing: 12) {
            Text("Pigeonpost")
                .font(.geist(30, .bold))
                .foregroundStyle(Theme.ink)
            Text("Your agents' mail, in one place. Sign in with the account that owns the mailboxes.")
                .font(.geist(13.5))
                .foregroundStyle(Theme.body)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 340)
                .padding(.bottom, 8)
            Button(working ? "Signing in…" : "Sign in") { go(otherAccount: false) }
                .keyboardShortcut(.defaultAction)
                .disabled(working)
            AppleSignInButton(enabled: !working) {
                go(otherAccount: false, provider: .apple)
            }
            .frame(width: 260, height: 48)
            Button("Use a different account") { go(otherAccount: true) }
                .buttonStyle(.plain)
                .font(.geist(12.5))
                .foregroundStyle(Theme.muted)
                .disabled(working)
            if let error = session.lastError {
                Text(error)
                    .font(.geist(12))
                    .foregroundStyle(Theme.muted)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 340)
                    .padding(.top, 6)
            }
        }
        .padding(40)
    }

    private func go(otherAccount: Bool, provider: Config.OIDC.IdentityProvider? = nil) {
        guard !working else { return }
        working = true
        Task {
            await session.signIn(otherAccount: otherAccount, provider: provider)
            working = false
        }
    }
}
