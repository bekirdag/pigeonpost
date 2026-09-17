# Pigeonpost ChatGPT plugin — progress

## Current state

Implementation in progress on `codex/chatgpt-plugin-20260917`, starting from `c96f2e903c2c55f79033f4299eda03536654a4a3`. The separate OAuth client is being provisioned disabled pending the portal callback. Backend deployment and plugin submission have not occurred yet.

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

## Validation and next steps

Next: complete staged validation, commit/push, deploy the tested backend and privacy disclosure, and verify public discovery/challenges. Once the user completes OpenAI prerequisites, create the plugin record, get its exact OAuth callback/domain challenge, enable the client and validate real account linking before review submission.

Private operational evidence is stored outside the repository. Do not add passwords, access tokens, client secrets, company documents, or reviewer credentials to this file.
