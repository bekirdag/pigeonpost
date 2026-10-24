# Big-features continuation plan — 2026-10-24

Resume the owner's explicitly requested implementation from the local `2026-10-21-big-features-HANDOFF.md`, the reviewed big-features SDS and its phase ledger. Hosted Nests and Products/Services remain separate planning work; this continuation does not provision agent hosting.

## Baseline and working rules

- Use an isolated worktree from `origin/main`; intake main is `d1f36e2`. Preserve the shared checkout and unrelated worktrees.
- Read current source and live PR/deployment state before relying on the October 21 handoff. Keep historical passes and failures tied to their original source revisions.
- Load profile/repo memory, use scoped Docdex search/symbols/AST and impact/DAG before code changes, and use local delegation for eligible small tasks with validation.
- Keep secrets out of source, logs and artifacts. Read only necessary private configuration and redact operational evidence.
- Commit and push meaningful validated checkpoints. Document unsuccessful checks and concrete follow-ups in the separate progress record.
- Native releases remain one batch after parity work. Preserve the owner's earlier Apple pre-submission review requirement; prepare reviewable artifacts before requesting that final decision. No paid purchase or unconfirmed seller announcement is implied.

## Work order and acceptance

1. **Recover and validate the open work.** Inspect PRs #83, #89 and #87 and their exact failed jobs. Distinguish branch defects from shared-main regressions, fix underlying issues without weakening checks, and review the authorization/cryptographic contracts. Merge only validated heads. Verify the running postbox includes Phase 4 server changes and intended feature flags. Deploy the enterprise site and device approval page with rollback and public-content verification after merge.
2. **Native round two.** Bring iOS/iPadOS/macOS, Android, Windows and Linux to the reviewed edit/reply/receipt contract, calendar and enterprise client/device-approval flows. Preserve encryption safety notices, permissions, accessibility and feature gating. Run platform-appropriate tests/builds and protect other projects' simulators.
3. **Prepare one native release batch.** Inspect current store states, build final versions and checksums, validate packages and prepare listings. Remove mainland China availability before a CallKit submission. Keep the Apple review hold visible. Source WNS configuration securely where available and record any human-only console action explicitly.
4. **Finish remaining SDS phases in order.** Phase 5 enterprise API, scoped keys/webhooks and developer documentation; Phase 6 streaming on the existing media host; Phase 7 consented meeting transcription/summaries and privacy updates; Phase 8 confined agent SSH and terminal UX. Each phase requires its own source review, impact analysis, tests and controlled deployment evidence.
5. **Hardening and review.** Work the inherited backlog, dependency/security advisories, authorization/revocation and cross-platform regressions, load/recovery tests and group-crypto review. Write a final review handoff with exact commits, PRs, artifacts, outstanding risks and reproducible evidence.

## Dependency order

Shared CI/runtime baseline → enterprise server/client authorization and #83 → message references/revisions/receipts and #89 → Apple group/block/filter integration and #87 → native parity → reviewed release batch. Enterprise API/streaming/meeting/SSH work reuses those verified contracts and keeps separate feature gates. Provider/account-console prerequisites must not stop independent implementation.

## External prerequisites to verify

Sender-domain verification for calendar/company invitations; iyzico USD activation and merchant subscription callbacks; enterprise and TURN DNS; 2027 TRY catalog before December 24; seller/terms confirmation before announcing sales; exposed-secret rotation; physical-device call checks. Verify which are still missing before asking the owner, and do not infer completion from a handoff note.

## Validation strategy

Start with failed CI logs and the smallest meaningful reproductions. Prefer Docdex test runners where feasible; use the repository's exact native/harness commands when the wrapper cannot express the job. Broaden checks after failures, integration changes or unresolved risk. Maintain exact source/version evidence and rollback plans for deployments. A green build alone does not establish live correctness or completion of every SDS requirement.
