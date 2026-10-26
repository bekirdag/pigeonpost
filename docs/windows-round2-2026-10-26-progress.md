# Windows native round-two progress

Plan: [implementation plan](windows-round2-2026-10-26-plan.md).

- Created the isolated Windows worktree from the available `origin/main` (`44bd813`), preserving the original checkout and Android room source. Fetch/merge the qualified parent changes before the Windows PR.
- Existing Windows native core has mailbox-operation generations but its shared transport acquires/renews the current token without an immutable accepted-grant identity. Inspect and qualify that boundary before adding new asynchronous feature flows. The initial source read is a finding, not a completed audit.
- The system .NET SDK is 8.0.101 while this repository targets .NET 10. Installed the free official SDK 10.0.403 into the private continuation tools directory without changing system SDKs or PATH. Baseline compilation/testing is next.
- Docdex's connected MCP session refuses another repository binding. Initialized only this exact Git worktree and used the scoped CLI memory fallback (no prior repository memories). Use scoped CLI/HTTP for code intelligence here; never index an umbrella folder.
- Android room local qualification has completed independently; its hosted approval parent CI is still running. All native stores remain held. No real messages, provider operations or production change.


## Accepted-session ownership foundation

- Added revocable parent/child local-work grants and an immutable async operation context. A grant identifies an accepted login lifecycle; server membership and message autonomy remain separate decisions. Retirement blocks new local writes and waits for an already-admitted short synchronous write before a replacement can publish.
- Session acceptance publishes a grant after the OS vault save succeeds. Ordinary renewal keeps that grant; successful explicit sign-in retires it. Device starts/completions are tied to the exact current request, and sign-out or a newer intent prevents stale renewal/start/invalid-grant responses from accepting or erasing credentials. Failed vault deletion preserves the current session for retry.
- Shared HTTP freezes the caller's actor before token acquisition and rechecks after acquisition, renewal, response headers and JSON/binary reads. Mailbox enrichment and combined inbox loads retain one actor across their requests. The account stream cannot refresh a replacement account from old events or silently reconnect with its token. Context-free preview/test providers retain their existing contract.
- **All 124 core tests pass**, including the real MLS library and fourteen added authorization/session cases (`windows-round2-authorization-session.log`). Controlled cases cover noncooperative token/body completions, a 401 renewal during replacement, a context spanning requests, replacement between mailbox-enrichment calls, old stream callbacks, nested async contexts, admitted local writes, sign-out versus renewal, late device starts, exact request identity and vault failure. The first 118-case run exposed normal stream callback cancellation becoming an exception; preserved its existing graceful return while retaining the old-account refusal. All original assertions remain.
- Scoped HTTP Docdex symbols and AST work for C#; graph calls return no dependency edges. Exact interface/partial-client/session callers and compiler/runtime checks supplement them. Local registration drafting timed out and the one-line registration was written directly. No new dependency or production operation.
- This foundation is not a completed native-account audit: group/call/avatar lifetimes and UI activation/reset bindings still need explicit review. Strong MFA/proof validation, all message-action/calendar/enterprise screens, actual Windows UI and release packaging remain. No store submission.
