# Native device approval

Continuation of native round two. Contract: [native device approval](device-pairing-native.md)
and [organizations](orgs.md). Progress: [work ledger](native-device-approval-2026-10-25-progress.md).

1. Add strict in-memory pairing code/link and preview models, and scoped server transport. Resolve
   and deny must stay bound to the signed-in human and exact resolved request. Expiry, refusal,
   throttling and late responses must be visible and cannot expose another account's request.
2. Handle strong-authentication challenges before an ordinary refresh replay. Use the fixed realm,
   authorization code, PKCE, state and nonce. Bind authorization/refresh results to the original
   session generation and subject; retired requests cannot restore a signed-out/replaced session.
3. Add shared Apple confirmation screens with prominent handle, terminal confirmation number,
   capabilities, expiry and untrusted network hint. Require a deliberate hold or accessible
   confirmation. Step-up returns to a refreshed preview and requires another approval decision.
4. Connect explicit dashboard/Settings entry, scanner and trusted links after existing sheets close.
   Gate against server capabilities and preserve active calls and unrelated presentations.
5. Qualify parser/transport/session races, UI denial/hold/expiry/new-code isolation and both Apple
   builds. Record native MFA realm configuration and actual OTP round-trip evidence separately.

The requesting machine never receives the human token. Codes, tokens and previews are not logged,
persisted or included in analytics. Native store releases stay batched; Apple App Review remains
held for the owner's Claude review. Nests/Products remain planning only.
