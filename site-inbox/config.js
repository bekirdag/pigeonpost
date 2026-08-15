// Pigeonpost inbox — configuration.
//
// No secret lives here. The OIDC client is public and PKCE-gated; the postbox validates the member
// token it receives against the realm, so nothing here grants anything on its own.
window.PIGEONPOST_INBOX = {
  // The hosted postbox. A member token is accepted directly as the bearer credential: the postbox
  // validates it against the realm and resolves it to the account that owns these mailboxes.
  postbox: "https://postbox.pigeonpost.dev",

  // Which namespace is "mine". When several mailboxes are on the account, the one under this
  // namespace opens by default — the operator's own inbox rather than whichever agent's address
  // the server happened to list first. Sub-agent mailboxes stay one click away in the picker.
  primaryNamespace: "/bekir",

  oidc: {
    issuer: "https://auth.pigeonpost.dev/realms/pigeonpost-prod",
    // Shared with the account surface on pigeonpost.dev. Public client, PKCE S256.
    clientId: "pigeonpost-web",
    redirectPath: "/",
    // Deliberately not requesting the contact-address scope: this app shows who you are signed in
    // as from `preferred_username`, and asking for a claim we never read is a claim we would then
    // be holding for no reason.
    scope: "openid profile",
  },

  // How long to hold the inbox request open waiting for mail. The postbox clamps this to its own
  // ceiling; 25s sits below common proxy idle timeouts.
  waitSeconds: 25,
};
