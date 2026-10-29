# Android contacts progress

- Isolated branch `codex/contacts-android-20261029`, base contact review `628da824`. Main checkout and other native worktrees preserved.
- Read the server/client contract, Android settings, authorized transport and organization-store lifecycle. Native contact screens are absent; existing profile-picture support is free.
- Exact-root Docdex initialization/tree worked. Search had no indexed files; symbol/impact requests returned writer backoff. Direct source and call-site inspection is the fallback, scoped to this worktree. Earlier local-model drafts failed validation and were not used.
- Plan recorded before implementation. No application edits or store actions yet.

## Implementation and evidence

- Added typed `core/contacts/PeopleApi.kt` requests over the existing authorized transport and `PeopleStore.kt` with immutable login/inbox ownership, stale-result rejection, feature withdrawal, 200-item initial batches, partial results and a separate single-use invitation confirmation. Discovery uploads only hashes; local contact names/aliases never enter the server contact/permission table.
- `PhoneContacts.kt` and `ui/People.kt` implement disclosure before Contacts permission, platform phone normalization with a region override, verification with the Android SMS-OTP autofill hint, opt-in discoverability, unlink confirmation, bounded lazy results and per-recipient invitation review. No READ_SMS permission, background imports or bulk invites.
- Free display-name/visibility controls and local aliases are wired into Settings, conversation lists, headers, the thread column and call history. Self-set names keep `~`; the actual address remains visible. Profile names refresh while foregrounded, independently of contact discovery.
- Aliases use the existing app-private SharedPreferences mechanism, keyed by subject + inbox hash, backup disabled and cleared at explicit sign-out. No new storage dependency. Existing free avatars and paid handle behavior remain unchanged.
- **426/426 JVM tests passed**, with real MLS required and no skips. Ten new tests cover hash vectors, upload privacy, malformed matches, precedence, quotas/partial results, stale responses/login changes, mailbox round trips, feature withdrawal and explicit invite confirmation.
- Android lint, debug APK and test APK builds passed. Reused qualified group-crypto artifacts from the prior native worktrees after confirming unchanged crate sources. **5/5 UI tests passed** in an owned read-only API 34 emulator: cancel disclosure, actual OS permission denial, separate invite confirm/cancel, free-name validation/save and country normalization. All API calls are synthetic/in-memory; no actual SMS or address-book row was sent.
- Initial checks caught a private Compose helper and a shortened-label test expectation; fixed. Android rejects the reserved UK mobile fixture as a valid phone, so the normalization test uses a fictional London-format example; the published hash-vector tests are unchanged. Repeated permission tests reset only this debug app’s permission flags so they deterministically receive the OS prompt.
- `git diff --check` passed. Private evidence: `Documents/releases/pigeonpost/continuation-20261029/validation/android-contacts-*`.
- The staged Docdex pre-commit hook recovered and passed (exit 0).

## Remaining

- Exact-head CI/review and merge; store version/submissions remain untouched for the next native batch.
- Owner-approved handset test for actual SMS delivery and keyboard autofill. Emulator success does not prove carrier delivery.
- Apple/Windows/Linux native contact surfaces remain separate follow-ups.
