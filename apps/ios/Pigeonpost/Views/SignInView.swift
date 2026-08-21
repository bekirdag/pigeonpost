//  Signed out. The web app's card, with the same words.

import SwiftUI

struct SignInView: View {
    @Environment(Session.self) private var session
    @State private var working = false

    var body: some View {
        VStack(spacing: 0) {
            Spacer()
            VStack(spacing: 12) {
                Text("Inbox")
                    .font(.system(size: 30, weight: .bold))
                    .foregroundStyle(Theme.ink)
                Text("Your agents' mail, in one place. Sign in with the account that owns the mailboxes.")
                    .font(.system(size: 15))
                    .foregroundStyle(Theme.body)
                    .multilineTextAlignment(.center)
                    .padding(.bottom, 10)

                Button {
                    go(otherAccount: false)
                } label: {
                    Text(working ? "Signing in…" : "Sign in")
                        .font(.system(size: 16, weight: .semibold))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 13)
                }
                .buttonStyle(.plain)
                .background(Theme.navy, in: RoundedRectangle(cornerRadius: 10))
                .foregroundStyle(.white)
                .disabled(working)

                // Quiet, and below the main button, because it is the rarer intent — but present,
                // because without it somebody signed in through a provider has no way back to the
                // chooser. The provider's cookie answers for them, and the screen they need never
                // appears.
                Button("Use a different account") {
                    go(otherAccount: true)
                }
                .font(.system(size: 14))
                .foregroundStyle(Theme.muted)
                .disabled(working)
                .padding(.top, 2)

                if let error = session.lastError {
                    Text(error)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.muted)
                        .multilineTextAlignment(.center)
                        .padding(.top, 6)
                }
            }
            .frame(maxWidth: 320)
            Spacer()
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.ground)
    }

    private func go(otherAccount: Bool) {
        Task {
            working = true
            await session.signIn(otherAccount: otherAccount)
            working = false
        }
    }
}
