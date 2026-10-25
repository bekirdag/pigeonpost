# Native authorization lifetime

Dependency of native machine approval and strong sign-in. Parent: device-approval foundation #104.
Progress: [evidence](native-session-scope-2026-10-25-progress.md).

1. Exercise the actual Session implementation with an isolated URLSession and browser callback
   adapter, synthetic tokens and an in-memory Keychain. Reproduce stale authorization completing
   after sign-out; no real realm request or production credential may enter a test.
2. Give each accepted authorization a distinct identity and each pending browser flow a ticket.
   Reject stale successes and failures. Refresh tasks must remain shared but cannot adopt into a
   replaced session, clear a newer task or forget a newer account after a late refusal.
3. Retire local authorization before awaiting realm logout. Bind native Apple and browser sign-in
   results to the same lifetime rules. Preserve existing ordinary token refresh behavior.
4. Test competing authorizations, sign-out during exchange, refresh sharing and delayed logout;
   run the shared regressions and both Apple builds. Fresh OIDC nonce/freshness and approval UI
   are subsequent steps and must not be represented as enabled by this change.

Keep Apple review and the release batch held. Nests/Products remain planning only.
