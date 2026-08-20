//  Configuration — the counterpart of site-inbox/config.js.
//
//  No secret lives here. The OIDC client is public and PKCE-gated; the postbox validates the member
//  token it receives against the realm, so nothing here grants anything on its own.

import Foundation

enum Config {
    /// The hosted postbox. A member token is accepted directly as the bearer credential: the
    /// postbox validates it against the realm and resolves it to the account that owns these
    /// mailboxes.
    static let postbox = URL(string: "https://postbox.pigeonpost.dev")!

    /// Which namespace is "mine". When several mailboxes are on the account, the one under this
    /// namespace opens by default — the operator's own inbox rather than whichever address the
    /// server happened to list first.
    static let primaryNamespace = "/bekir"

    enum OIDC {
        static let issuer = URL(string: "https://auth.pigeonpost.dev/realms/pigeonpost-prod")!

        /// The app's own public client — deliberately not the browser's `pigeonpost-web`. A native
        /// app redirects to a custom scheme rather than to an origin, and sharing one client would
        /// mean one set of redirect rules for two things with different lifetimes.
        static let clientId = "pigeonpost-mobile"

        /// Must match a redirect URI on the client exactly.
        static let redirectURI = "dev.pigeonpost.inbox://oauth2redirect"

        /// The scheme half of that, which is what the web authentication session watches for.
        static let redirectScheme = "dev.pigeonpost.inbox"

        /// `offline_access` keeps this signed in the way a messenger is expected to be. The
        /// contact-address scope is deliberately not requested: this app shows who you are signed
        /// in as from `preferred_username`, and asking for a claim we never read is a claim we
        /// would then be holding for no reason.
        static let scope = "openid profile offline_access"

        static func endpoint(_ path: String) -> URL {
            issuer.appendingPathComponent("protocol/openid-connect").appendingPathComponent(path)
        }
    }

    /// How long to hold the inbox request open waiting for mail. The postbox clamps this to its own
    /// ceiling; 25s sits below common proxy idle timeouts.
    static let waitSeconds = 25
}
