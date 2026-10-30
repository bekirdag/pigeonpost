# Onboarding and calls continuation — 29 October 2026

Owner request: review Claude's handed-off implementation and finish simpler branded code sign-in, first-message navigation, phone-based contact discovery, free names/avatars, SMS invitations, and reliable calls across Pigeonpost clients.

## Order and dependencies

1. Recover current source, PR checks, deployment versions and configuration. Preserve the main checkout, historical handoffs, concurrent work and current store submissions. Append to the existing main-checkout ledger; never rewrite it.
2. Review the deployed call-answer fix and smoke follow-up #181. Validate the stalled AudioContext regression and synthetic media paths; distinguish automated coverage from a physical iPhone lock-screen check. Fix regressions with scoped changes and evidence before deployment.
3. Review and qualify Apple #180 and Windows #173 for first-send navigation, lost responses, canonical recipient resolution, draft custody and login/mailbox switches. Merge qualified source; retain current store submissions.
4. Review Keycloak code sign-in in sealunit/su_iam #4 and the staged image/theme. Verify Postmark access and dedicated transactional stream with open/link tracking disabled. Set Pigeonpost sender, deploy with backups and a narrowly scoped rollback, then validate sign-in and branded delivery. Support OS code suggestions; do not claim unrestricted automatic access to Apple Mail/Gmail or implement mailbox scraping.
5. Qualify saas_be #29, disable tracking only for Pigeonpost, deploy with backups and verify a single owner-addressed test when authorized. Enable calendar/company invitations only after transport verification.
6. Review Phase 10 #178 against SDS §14D: person-only proof of phone control, opt-in discovery, anti-enumeration limits, encrypted phone storage, contact uploads not retained, explicit SMS actions with durable budgets, revocation/blocking and local alias isolation. Resolve current merge conflicts and defects before exact-head CI qualification.
7. Verify the merged saas_be #30 SMS transport and tenant-scoped token deployment. Deploy compatible postbox/web/invite assets, retain a compatible storage-format rollback, then enable validated feature flags. Do not send unsolicited test SMS or invitations to address-book contacts.
8. Implement the native Phase 10 round: opt-in OS contact permission/import, phone verification/discoverability controls, discovered contacts, per-inbox local aliases, free display names and avatars, explicit SMS invitations with a review step. Follow existing platform session/custody patterns and test denial, empty results, malformed contacts and account changes.
9. Review the earlier #136–#163 risk areas in the review handoff (SSH, storage codec, org authority/webhooks, meeting consent/audio deletion, stream key authority, native login scope). Fix concrete defects with focused regressions and record remaining external/device validation honestly.

## Verification and rollout

- Use Docdex profile/repo memory, symbols and impact/DAG when available; if unavailable, record failure and inspect direct source and dependency references.
- Keep source work isolated; no credentials in source, output or public artifacts. Backups remain server-side.
- Use synthetic local fixtures for behavioral tests. Do not read users' conversations or send test calls to real accounts.
- Qualify current PR head and all applicable checks; investigate failures without waiving assertions.
- Inspect native/browser visuals where behavior or layout changes. Use the existing shared browser only.
- Deploy only reviewed artifacts, verify hashes/health, and preserve the postbox's compact storage format. Do not use older pre-#160 images as a rollback.
- Existing Apple/Google/Microsoft submissions are already pending. Do not cancel or resubmit them; record source intended for the next batch.
- Nests remain planning-only. Unrelated billing currency enablement and infrastructure migrations are outside this continuation.

## Evidence

Maintain `docs/onboarding-continuation-2026-10-29-progress.md` separately with exact revisions, commands, findings, deployment evidence and real blockers.
