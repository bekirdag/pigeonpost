# Big-features continuation progress — 2026-10-24

Plan: [Continuation plan](continuation-2026-10-24-plan.md).

## Current state

Active implementation continuation. PR #83's PKCE test race is fixed and pushed; PR #89 is synchronized with current main, formatted and pushed. Apple failure diagnosis and shared dependency hardening are underway. No PR merged or production change made by this continuation yet. Native releases remain batched and the existing Apple pre-submission review requirement is preserved.

## Intake evidence

- Read the local October 21 handoff and phase ledger from the shared checkout; loaded `codex` profile, repo memory, wake-up context and the applicable planning/progress and Pigeonpost instructions.
- Shared checkout is clean on an older release branch; all valid inherited worktrees are clean. One unrelated/prunable scratch worktree has a missing gitdir and was left untouched.
- Fresh worktree: `pigeonpost-big-features-20261024`, branch `codex/big-features-continuation-20261024`, based on current `origin/main` `d1f36e2`.
- PR #83 remains open at `01d87fd`; web and dependency-audit checks failed. PR #89 remains open at `cf4abfd`; web, formatting, audit and black-box checks failed. PR #87 remains open at `7b3e752`; iPhone/iPad, web, formatting, audit and black-box checks failed. These are completed failures, not still-running jobs.
- Shared-checkout Docdex index is dated October 15, so it is not fresh implementation evidence. Read current planning file slices directly; the continuation worktree will get its own scoped index for code inspection.
- Local disk has approximately 75 GiB available. Avoid duplicating native build caches and preserve unrelated simulator state.

## Work ledger

| Area | State | Next evidence |
|---|---|---|
| Shared baseline / CI | Compatible security updates under validation | QUIC/TLS/trust-root patches plus ctutils/Tokio/yanked-package updates; crypto-bigint advisory remains unresolved |
| #83 enterprise/device login | Fix pushed at `fb7f396`; CI running | Final source review and checks; deploy the missing approval page and verify portal readiness |
| #89 edit/reply/receipts | Fix pushed at `c7648de`; CI running | §14C source cross-check and fresh hosted-runner/privacy checks |
| #87 Apple block/filter/groups | Current main merged locally; diagnostics running | Inspect intentional portrait sidebar overlay versus actual overlap; resolve purchase navigation failures |
| Native round two | Pending | Verified server contracts and platform test plans |
| Store batch | Held until parity and review | Current store state, final signed artifacts, Apple review decision |
| Phases 5–8 | Pending | Source/SDS gap analysis after open PRs |
| Hardening/review handoff | Pending | Inherited backlog and security/dependency evidence |

## Failures and recovery notes

- Initial worktree-status inventory encountered the stale scratch gitdir. Repeated the inventory while explicitly skipping prunable worktrees; all valid inherited worktrees are clean. No cleanup or reset was performed.
- Generic clone routines contain inferred approval gates. The owner has explicitly authorized this continuation; apply plan/progress/validation and controlled-release rules within that scope, preserving the specific Apple submission hold.

## Checkpoint — first fixes and live-state verification

- **#83:** the old test counted event-loop turns while native WebCrypto ran asynchronously, then parsed an unset navigation URL. The fixture now exposes a navigation promise and deliberately delays the digest, verifies the real S256 challenge and stored state/nonce, and waits for the step-up redirect. Production authentication code is unchanged. Commit `fb7f396` pushed. Targeted tests **8/8**, full inbox **179/179**, enterprise **26/26**, bridge **20/20** pass locally. Fresh CI `40110383557` passes inbox, formatting, OpenAPI and black-box checks; platform jobs were still completing at this checkpoint and the inherited audit gate remains red.
- **#89:** merged current main to incorporate #88's hosted-runner routing and OpenAPI repairs; no conflicts. Formatted the one ownership-test assertion reported by CI. Commit `c7648de` pushed. Formatting passes, message-action server tests **8/8**, inbox **183/183** pass. The previous web/black-box failures were missing Chrome/sqlite3 on the old runner pool, not failed privacy assertions. Keep the new CI result separate from that older run.
- **#87:** merged current main locally without conflicts. Downloaded the historical iPad result bundle and exported screenshots. Portrait Calls shows a native sidebar overlay over dimmed content; the old audit counts underlying controls as overlapping. This is visual evidence for a possible audit-scope issue, not proof that accessibility is correct. Added hierarchy/hit-test attachments and started a targeted local diagnostic before choosing a fix. Local Xcode 16.2/iOS 18.3 fixture build passes; CI uses the newer iOS runtime and remains necessary. Existing iPhone failures are the two purchase-navigation tests listed in the handoff.
- **Postbox:** live source `c0d6a4b`, image `pigeonpost-postbox:p3b-c0d6a4b`, both services running. Source ancestry includes Phase 4 server commit `001f21c`; org tables exist. Global features include calendar/groups/filtering/call flags, but orgs/enterprise_portal are not globally enabled. Step-up is configured as `mfa`; actual realm behavior still needs verification. No service/env/database mutation made.
- **Routes:** Raijin already has the enterprise vhost and directory, but the inbox approval page is absent. Public probes for enterprise and pair return 403 while postbox health is 200; determine origin versus edge behavior before declaring routing ready. Enterprise and TURN names resolve to Cloudflare addresses; that alone does not verify explicit records or DNS-only TURN.
- **Dependency hardening:** the continuation branch updates quinn-proto `0.11.19`, rustls `0.23.46`, rustls-webpki `0.103.16`, webpki-roots `1.0.11`, ctutils `0.4.5`, Tokio `1.54.1`, chacha20 `0.10.2`. These are compatible updates. A new crypto-bigint advisory (`RUSTSEC-2026-0387`, fixed in 0.7.6+) still reaches the older P-256/P-384/K-256 dependencies through OpenMLS; no ignore or unreviewed crypto-provider migration was added. Record this as an unresolved hardening gate, not a clean audit.
- **Regression failure preserved:** the patched full workspace compiled, but the oversized-request Loft integration test hit macOS `Connection reset by peer` while uploading a rejected body. The isolated reproduction passed. Investigate deterministic early-rejection coverage; do not label the full suite passed or remove the boundary assertion.

## Tooling and evidence

- The persistent MCP connection is bound to the shared checkout. Explicit per-worktree CLI/HTTP initialization and fresh MCP sessions were used instead; no umbrella directory mounted. AST/symbol/impact checks were scoped to each changed test. Empty JavaScript/Swift graph edges do not prove no UI coupling; direct HTML/project/shared-view references were also inspected.
- `docdexd run-tests` cannot map a nested Node test inside a Cargo workspace; it returned `unsupported Cargo test target`. The documented `docdexd test run-node` helper passed the targeted pairing suite; repository-native npm/Cargo commands ran the full/focused suites where appropriate.
- Small local-model delegation was attempted and retried for the pairing test; both drafts invented APIs or failed to implement the promised resolver, so neither was applied. Primary review implemented and validated the change.
- Local diagnostic artifacts are in the private release area under `continuation-20261024/`; fixture build log, historical iPad screenshots and targeted test results are retained. Never treat the iOS 18 diagnostic as qualification of the failing newer-runtime CI case.

## Checkpoint — validated dependency patches and authorization review

- Full patched root workspace regression (`cargo test --locked --workspace --all-targets --all-features`) **passes** after replacing the oversized upload in the Loft acceptance test with a real TCP header-only request. The test still requires HTTP 413 and no stored event, now also requiring a response within five seconds without sending any body. The initial macOS reset failure remains recorded above. Loft offline suite: **26/26**; formatting passes. Full logs are retained privately in `validation/workspace-patched.log` and `validation/loft-offline.log`.
- Audit of the patched lockfile reports **one** remaining vulnerability, crypto-bigint 0.5.5 (`RUSTSEC-2026-0387`), with no new ignore. Latest published OpenMLS RustCrypto provider is still 0.6.0 and does not offer a compatible patched dependency path. The application's fixed MLS suite is X25519/AES-GCM/Ed25519; the direct P-256 use verifies public calendar guest proofs. Continue reachability review before any release decision.
- #83 functional CI is complete and passes all platform, inbox, privacy and format checks; audit remains red. #89's completed functional checks pass (including Windows x64/ARM64 builds); its audit remains red. No branch-protection bypass.
- #87 current-main merge and accessibility diagnostics pushed at `56855ef`. Local iPad Calls audit passes on iOS 18.3, and both historical purchase-navigation failures pass on the matching local fixture binary. Newer-runtime CI is required and running; local passes do not close those CI defects.
- Device-approval review found two genuine UI authorization issues: malformed new links could retain another machine's pending code, and a failed denial was displayed as successful. A hold could also continue after clicking Decline. Fixes clear stale fragment state, report refusal failures, cancel the pending hold and serialize decisions. Focused tests **12/12** and full inbox **183/183** pass; committed and pushed as `cde5d01` on #83.
- Scoped Docdex pre-commit validation completed with exit 0 for the staged dependency checkpoint and Apple diagnostic change. A simple local-model test-refactor draft was invalid (invented function/signature); it was rejected. The final Loft change was authored and validated by the primary agent.

- Patched root workspace Clippy (`--all-targets --all-features -- -D warnings`) passes. Security update PR remains draft while the remaining advisory is reviewed. No new audit ignore has been added.
