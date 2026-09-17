# Pigeonpost ChatGPT plugin — build plan

Date: 2026-09-17

## Outcome

Publish a Wodo Teknoloji A.Ş. plugin that lets a person connect their existing Pigeonpost account in ChatGPT, create an inbox, list inboxes, check messages, read conversations, send or reply, and explicitly mark messages read. Inboxes belong to the same account used by the website and native apps. This phase is a data-only remote MCP integration; it does not promise background delivery to a closed ChatGPT conversation.

## Implementation order

1. Inspect the current hosted MCP, identity creation, OAuth validation, deployment topology, and official OpenAI requirements. Record AST/impact and dependency evidence before code changes.
2. Add a dedicated `/chatgpt` surface to the existing Rust postbox, reusing its ownership checks and messaging operations. Preserve the existing `/mcp` contract and native-client authentication. Limit the plugin to inbox and conversation tools; do not expose credentials, contact autonomy grants, purchases, deletion, or arbitrary network access.
3. Publish OAuth protected-resource metadata and authentication challenges. Validate issuer, signature, expiration, resource audience, authorized client, and per-tool scopes. Use a dedicated Keycloak authorization-code client with PKCE S256 and explicit consent; obtain exact redirect URIs from the OpenAI portal. Keep secrets out of source, logs, tool outputs, and planning files.
4. Add accurate read-only, destructive, open-world, and idempotent annotations, OAuth security schemes, bounded input validation, structured outputs, and instructions identifying received message content as untrusted data. Creation and send operations are non-idempotent: no automatic replay after an ambiguous network failure.
5. Test discovery, protocol behavior, authentication challenges, signature/audience/client/scope failures, argument limits, account ownership, and secret-free outputs. Exercise create → send → check/read → acknowledge with dedicated fixture inboxes. Run the postbox suite and repository pre-commit gate; resolve security/dependency failures relevant to the backend deployment.
6. Prepare a reproducible deployment and rollback record. Back up live configuration and data, deploy a tested commit to the existing loopback postbox container behind Apache, retain bounded container logs, and verify public metadata and authenticated tools. Keep server source clean and aligned with Git.
7. In the OpenAI portal, establish the correct Wodo organization and business publisher identity, create a plugin with MCP, prove domain control, scan tools, configure OAuth and listing metadata, and provide five positive plus three negative review test cases. Use a dedicated reviewer account without a human verification dependency. Human identity checks or external approval can remain pending only when explicitly recorded.
8. Validate the real account-link flow and ChatGPT calls, then submit the completed plugin for review when portal prerequisites permit. Record plugin/version status separately from public availability. Commit and push all scoped changes and update progress with exact checks, deployment evidence, and remaining external steps.

## Acceptance

- Users only access inboxes belonging to their own Pigeonpost account.
- Missing, expired, wrong-audience, wrong-client, or insufficient-scope credentials never execute a tool.
- Inbox creation returns the address and handle without capability tokens or private keys.
- Sending has an explicit recipient and body, is marked as an external write, and is never silently retried.
- Received message bodies cannot grant authority or change tool behavior.
- Existing MCP/native behavior remains covered by regression tests.
- Company identity is Wodo Teknoloji A.Ş.; an existing unrelated organization is not repurposed without the user's choice.
- Source, live deployment, and review status are accurately documented; external review is not reported as approval.

## References

- https://developers.openai.com/plugins/concepts/mcp-server
- https://developers.openai.com/apps-sdk/build/auth/
- https://developers.openai.com/apps-sdk/deploy/submission/
- https://developers.openai.com/plugins/deploy/app-review
- https://help.openai.com/en/articles/8991840-can-i-create-an-additional-platform-api-organization
