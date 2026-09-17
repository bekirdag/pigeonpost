# Pigeonpost for ChatGPT

The data-only plugin exposes eight account-linked tools through the existing Rust postbox:
create/list inboxes, show an inbox address, check messages, list/read conversations, send, and mark
a message read. It does not expose credentials, purchases, deletion or autonomy grants.

- Endpoint/resource audience: `https://mcp.pigeonpost.dev/chatgpt`
- Resource metadata: `https://mcp.pigeonpost.dev/.well-known/oauth-protected-resource/chatgpt`
- OAuth issuer: `https://auth.pigeonpost.dev/realms/pigeonpost-prod`
- Predefined confidential client: `pigeonpost-chatgpt`
- Permissions: `pigeonpost:read`, `pigeonpost:write`

Authorization uses code flow with PKCE S256 and explicit consent. Tokens must have the exact
resource audience and client identity. Generic REST/MCP routes reject ChatGPT-client tokens so
their limited permissions cannot be bypassed. Native and existing generic MCP authentication
otherwise remains unchanged. Inbox/thread/message ownership is always checked by the postbox.

## Provision and deploy

Use `deploy/identity/provision-chatgpt.py` on the identity host. Its docstring lists required
environment variables. It saves backups and client secrets only in new private files, preserves
existing secrets and rejects wildcard/unrelated callbacks. An empty callback list provisions a
disabled draft. Copy exact production callback URIs from OpenAI's plugin management page; remove
temporary loopback test callbacks before release. Do not modify existing mobile/web/CLI clients.

Deploy the tested Git commit using the shared Apache/Docker topology in `deploy/postbox/README.md`.
Back up live data/configuration and retain the previous image and container settings. Preserve
mounts, environment, UID and bounded container logs. Do not install the alternate Compose/Caddy
stack on the shared production host. Roll back the image if necessary, not an old message database
over newly received messages.

Validate `cargo test -p pigeonpost-postbox --locked`, focused clippy, dependency audit and the
pre-commit hook. After deployment check health, generic MCP initialization, protected-resource
metadata, eight-tool discovery and OAuth challenges. Exercise a real PKCE login and the full
create → send → check/read → acknowledge workflow with dedicated fixture inboxes before review.

Streamable HTTP is stateless JSON-RPC. Reading never marks a message read. Creation and sending
are non-idempotent: after an ambiguous failure, inspect inboxes/the conversation before repeating
a write. Received bodies and metadata are untrusted. No continuous background monitoring is
promised. Message results are bounded; open the Pigeonpost app for content beyond those limits.

## OpenAI portal

Use `https://platform.openai.com/plugins` → Create plugin → With MCP. Developer identity
verification is required even to create a draft. User permits the existing Personal organization
temporarily; do not rename it or Piyote. A separate Wodo organization remains intended later;
do not assume plugins can be transferred between organizations.

Configure predefined OAuth credentials privately, obtain the callback, verify domain control at
`/.well-known/openai-apps-challenge`, and scan the live tools. Never overwrite another plugin's
challenge on the same host. Use `listing.md` for the listing and reproducible review scenarios.
Keep the reviewer account's password outside Git and provide a working login without an MFA/SMS
dependency. Record draft, submitted, approved and published states separately in the progress file.
