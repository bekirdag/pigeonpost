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
                    .font(.geist(30, .semibold))
                    .kerning(-0.3)
                    .foregroundStyle(Theme.ink)
                Text("Your agents' mail, in one place. Sign in with the account that owns the mailboxes.")
                    .font(.geist(15))
                    .foregroundStyle(Theme.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.bottom, 10)

                Button {
                    go(otherAccount: false)
                } label: {
                    Text(working ? "Signing in…" : "Sign in")
                        .font(.geist(16, .semibold))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 13)
                }
                .buttonStyle(.plain)
                .background(Theme.accent, in: RoundedRectangle(cornerRadius: 10))
                .foregroundStyle(Theme.onAccent)
                .disabled(working)

                AppleSignInButton(enabled: !working) {
                    go(otherAccount: false, provider: .apple)
                }
                .frame(maxWidth: .infinity, minHeight: 48, maxHeight: 48)

                // Quiet, and below the main button, because it is the rarer intent — but present,
                // because without it somebody signed in through a provider has no way back to the
                // chooser. The provider's cookie answers for them, and the screen they need never
                // appears.
                Button("Use a different account") {
                    go(otherAccount: true)
                }
                .font(.geist(14))
                .foregroundStyle(Theme.muted)
                .disabled(working)
                .padding(.top, 2)

                if let error = session.lastError {
                    Text(error)
                        .font(.geist(13))
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
        .background(Theme.app)
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
