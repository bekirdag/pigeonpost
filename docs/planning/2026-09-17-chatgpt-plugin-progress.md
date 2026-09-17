# Pigeonpost ChatGPT plugin — progress

## Current state

Backend implemented and deployed from `a0f9cba63b825b090534e7ce9d12964d8d2b5248`. PR23 merged to main as `cbbdf7b9e8db8ca7e762bf16d9e0324000bc6861`. Real OAuth and messaging tests passed. The OAuth client is disabled with no callbacks pending OpenAI portal setup. No OpenAI plugin record, submission, approval or public listing exists yet: the portal requires the user to add a default payment method and finish identity verification before creation.

## Verified findings

- Public `https://mcp.pigeonpost.dev/mcp` initializes successfully, reports MCP 2025-06-18 and postbox 0.7.18, and exposes existing inbox/messaging tools.
- Protected-resource metadata currently returns 404. Tool descriptions lack the OAuth and review metadata required by the new integration.
- `crates/pigeonpost-postbox/src/oidc.rs` currently disables audience validation for generic clients. The ChatGPT endpoint must validate its own resource audience without changing native-client behavior.
- `do_create_identity` returns a capability token. The new endpoint must discard it before producing tool output.
- The existing Keycloak issuer supports PKCE S256. Its OIDC discovery returned 200 with browser and ChatGPT user agents; a default Python urllib request returned 403, so production-client compatibility needs checking.
- Production postbox runs behind Apache on the shared host, with a loopback Docker container. The Compose/Caddy examples describe an alternate dedicated-host deployment and must not be applied to this host.
- The authenticated OpenAI account lists Piyote and Personal. User wants a separate Wodo organization, then explicitly authorized creating the plugin under Personal temporarily. Neither existing organization was renamed. A support request was drafted but not sent.
- OpenAI blocks even draft plugin creation until developer identity verification. Starting Individual verification then requires a valid default payment method. The user has the payment page and was asked to complete payment-method setup and verification. No payment was made or method added by the agent.
- Live Keycloak runs on wodomini in the `sealunit-keycloak-1` container (24.0.5 custom image), with administration verified through local TLS under authenticated SSH. Existing identity documentation's Keycloak 26 reference does not describe this live container.

## Implemented

- Dedicated `/chatgpt` route and protected-resource metadata; eight inbox/messaging tools with explicit OAuth scopes and review annotations.
- Strict signature, issuer, audience, expiry/not-before, authorized client and access-token-type validation. Generic APIs reject limited ChatGPT tokens, preventing a scope bypass.
- Explicit input limits, ownership enforcement, credential-free create responses, bounded message count/body output, read-only reads and explicit acknowledgements.
- Scoped OAuth provisioning with exact callback allowlists, PKCE, consent, private backups/secrets and an initially disabled client.
- Listing/review scenarios and hosted privacy disclosure for data shared with ChatGPT.
- Rustls 0.23.43 → 0.23.45 and webpki 0.103.13 → 0.103.15 resolve RUSTSEC-2026-0285 before the backend rollout.

## Validation evidence

- `cargo test -p pigeonpost-postbox --locked`: **244 passed**, including creation/send/read/ack, account isolation, untrusted-content handling, bounded output and strict OAuth claim tests.
- `python3 deploy/identity/test-chatgpt.py`: **3 passed**, testing callback restrictions and exclusive private backup creation.
- `cargo clippy -p pigeonpost-postbox --all-targets --locked -- -D warnings`: passed.
- `cargo fmt --all --check`: passed after formatting the new routes.
- `cargo audit --json`: **0 vulnerabilities** after the dependency update; pre-existing yanked-package warnings remain.
- `docdexd hook pre-commit --repo ...`: passed. Vocabulary guard passed; final staged whitespace validation is being completed.
- Dedicated Keycloak client is provisioned **disabled**, with no callbacks, PKCE S256 and explicit consent. Backup/secret storage is private on wodomini under `/var/backups/pigeonpost-chatgpt/20260917-draft`. Initial provisioning exposed Keycloak 24's automatic `rootUrl/*` callback for an empty list; the client remained disabled, the script now clears root/base URLs, and a second verification confirmed an empty callback list.
- Docdex symbols and backend impact graphs confirmed main → mcp/oidc dependencies before edits. DAG export was truncated and not useful for full dependency coverage. Afterward the daemon's MCP/HTTP impact and health endpoints stopped responding; the static disclosure and CI edits used direct local caller/config inspection. CLI `run-tests --target crates/pigeonpost-postbox` does not support workspace crate targets, so Cargo ran the exact suite directly.
- Two local-model refactor attempts invented a nonexistent `Principal.token`; both were rejected. The small refactor was performed locally and covered by the postbox suite.

## Production and acceptance

- PR: https://github.com/bekirdag/pigeonpost/pull/23
- GitHub run `35216287594`: **all 12 checks passed**, including Linux/macOS/Windows suites, Windows lifecycle, delivery/proxy privacy, custody boundaries, lint and audit.
- Image: `pigeonpost-postbox:chatgpt-a0f9cba63b82`; postbox and reaper both use the tested image with original environment, mounts, loopback binding and 10m × 3 log rotation.
- Backend backup: `web:/opt/pigeonpost-postbox/backups/chatgpt-20260917T114912Z` contains private container settings, credentials, message/key data and attachment snapshot. Previous containers remain stopped under timestamped rollback names.
- Hosted privacy disclosure deployed atomically on wodomini. Backup: `/var/backups/pigeonpost-chatgpt/20260917-site`; public SHA256 `962fb3d7a4552361b6d5c4906b37ef87a4f545a50efaa70c862e9b502ac55bae` matches the tested source.
- Public health, protected-resource discovery, eight-tool manifest and generic MCP initialization: **passed**. Missing/invalid credentials return 401 with OAuth challenges; a foreign browser origin returns 403.
- Real Keycloak authorization-code login with PKCE S256: **passed** using two dedicated fixture accounts and a temporary exact localhost callback. Create/list/send/reply/read/ack passed against production. Cross-account inbox access was denied, a read-only token could not create an inbox (403), and a limited connector token could not enter the general REST API (401).
- Review fixture addresses and their conversation are recorded privately on wodomini in `/var/backups/pigeonpost-chatgpt/live-oauth-test-20260917.json`. They contain only synthetic review messages, including an explicitly untrusted instruction fixture. Credentials remain private on the identity host.
- Temporary callback removed after testing; client verified disabled/no callbacks again. No real-user conversations were used by the acceptance test.

## Remaining portal steps

1. User adds the required default payment method and completes OpenAI Individual identity verification for the temporarily authorized Personal organization. Neither Personal nor Piyote was renamed; the separate Wodo organization request is still only a draft and was not sent.
2. Create the Pigeonpost plugin record, obtain the exact production OAuth callback and domain challenge, configure the client, then scan tools and complete listing metadata from `deploy/chatgpt/listing.md`.
3. Test linking and prompts inside ChatGPT itself. The live PKCE acceptance test validates Pigeonpost's side, not the still-unavailable OpenAI plugin connection.
4. Submit the completed review package, track the decision, and publish after approval. Wodo business publisher verification remains a separate company setup step; do not claim it is completed.

Private operational evidence is stored outside the repository. Do not add passwords, access tokens, client secrets, company documents, or reviewer credentials to this file.
