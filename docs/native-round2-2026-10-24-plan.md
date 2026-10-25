# Native round two — implementation plan

Continuation of the October 21 big-features handoff and SDS §§14B, 14C and 26.
Progress and validation: [work ledger](native-round2-2026-10-24-progress.md).

## Scope and order

1. Finish and integrate the shared baseline and PRs #83, #89 and #87 before deploying or releasing dependent changes.
2. Add edit, reply, message information and receipt/privacy support to the shared Apple app, then Android, Windows and Linux. Preserve legacy server behavior when flags are off; use the contracts in `docs/message-actions.md` and `docs/groups.md`.
   Group controls depend on complete version erasure, periodic metadata reconciliation, retaining sealed files across caption edits, and exact-request recovery after response loss/restart. Keep these contracts qualified with real MLS/server transport tests before native release.
3. Add calendar, scheduled meetings and invitation handling on every client against `docs/calendar.md`, preserving timezone, revision, idempotency and privacy rules.
4. Add organization rooms/directory/operated-agent access and native device-approval screens against `docs/orgs.md` and `docs/device-pairing-native.md`. Configure native MFA with the verified web level-2 flow, PKCE/state/nonce and a fresh approval decision after step-up.
5. Build and qualify one native release batch. Keep the owner's Apple review hold; remove mainland China before submitting the CallKit release. Do not announce web sales before seller/payment prerequisites are settled.

## Validation

- Server-wire fixtures and compatibility tests for missing/unknown fields, message actions, privacy and mailbox switches.
- Real encryption/binding and group-action tests; no plaintext group fallback.
- Platform model and UI tests, iPad layout audit, accessibility and failed-request recovery.
- Calendar wall-time/DST, revision-conflict, cancellation and invite-permission cases.
- Device approval: invalid/new code isolation, denial failures, expiry, OTP/PKCE round trip and another deliberate approval after sign-in.
- Signed artifacts, current store status, production rollout/rollback evidence and owner device checks recorded separately from automated tests.

## Boundaries

Nests and Products remain planning only. Use existing free/open-source infrastructure. No new paid infrastructure, unrelated worktree cleanup, or secrets in source, logs or repository documents. Preserve unrelated simulators and shared browser tabs.
