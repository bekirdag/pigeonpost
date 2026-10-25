# Native machine confirmation and strong sign-in

Parent: Apple integration #105 (`e445231`). Keep that candidate frozen for complete CI.
Contracts: [native approval](device-pairing-native.md), [organizations](orgs.md).

1. Add fresh authorization with the fixed realm, PKCE, state and nonce. Bind issuer, audience,
   current subject, time and requested authentication context before accepting a response. Use
   only ID tokens returned directly by the fixed HTTPS token endpoint, following OpenID Connect
   Core 3.1.3.7; the postbox independently validates the access token and required authentication.
2. Build an authorization-scoped confirmation store. Token reads and ordinary renewals must check
   the same authorization before and after suspension. Late replies cannot replace another code
   or account. Denial must re-resolve its final state. Lost approval replies may be resolved but
   cannot cause automatic approval retries.
3. Show organization, agent inbox/explicit creation, machine label, requested access, untrusted
   network hint, terminal number and expiry. Require a 1.5-second hold or accessible confirmation.
   A strong sign-in returns to a fresh preview and requires another deliberate approval.
4. Connect trusted pairing URLs and scanner/dashboard/Settings entry after existing sheets close,
   respecting server flags, active calls and unrelated presentations. No requesting machine ever
   receives a human token; codes and previews remain in memory.
5. Qualify model, transport, authorization and UI races plus both Apple builds. Inspect the actual
   native client's verified-address claims and MFA flow/mappers before live enablement. A genuine
   OTP round trip is separate evidence from synthetic tests. Keep all store/Claude review gates.

Progress: [work ledger](native-device-ui-2026-10-25-progress.md). Nests/Products remain planning only.
