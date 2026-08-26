//  Signed out, opening, or the inbox — the same three states the phone has, said the Mac's way.

import SwiftUI

struct MacRootView: View {
    @Environment(Session.self) private var session
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox

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
        .background(Theme.wash)
        .task(id: session.status) {
            guard session.status == .signedIn else { return }
            await account.loadIdentities()
        }
    }

    @ViewBuilder
    private var opening: some View {
        switch account.load {
        case .failed(let why):
            VStack(spacing: 12) {
                Text(why)
                    .font(.system(size: 13))
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
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Theme.ink)
                Text("A mailbox is where your agents reach you.")
                    .font(.system(size: 13))
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
                .font(.system(size: 30, weight: .bold))
                .foregroundStyle(Theme.ink)
            Text("Your agents' mail, in one place. Sign in with the account that owns the mailboxes.")
                .font(.system(size: 13.5))
                .foregroundStyle(Theme.body)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 340)
                .padding(.bottom, 8)
            Button(working ? "Signing in…" : "Sign in") { go(otherAccount: false) }
                .keyboardShortcut(.defaultAction)
                .disabled(working)
            Button("Use a different account") { go(otherAccount: true) }
                .buttonStyle(.plain)
                .font(.system(size: 12.5))
                .foregroundStyle(Theme.muted)
                .disabled(working)
            if let error = session.lastError {
                Text(error)
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.muted)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 340)
                    .padding(.top, 6)
            }
        }
        .padding(40)
    }

    private func go(otherAccount: Bool) {
        Task {
            working = true
            await session.signIn(otherAccount: otherAccount)
            working = false
        }
    }
}
