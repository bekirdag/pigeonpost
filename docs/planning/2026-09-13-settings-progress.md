# Settings progress — 2026-09-13

## Baseline and audit

- Main starts clean at `1183fef2d89317bf08d26d50a33c0fb2a53f78e6`.
- Worktree: `/private/tmp/pigeonpost-settings-20260913`, branch `codex/settings-20260913`.
- Private evidence: `/private/tmp/pigeonpost-settings-audit-20260913`.
- Read Pigeonpost and planning/progress skill instructions. Retrieved both Docdex memory lobes, wake-up context, and operator directive. Saved the user's preference for concise native Settings with subpages.
- Current iOS/macOS and Android Settings put ownership, purchases, account and inbox functions on one long page. Linux uses a flat button list. Web inbox has the same long-screen problem; native Windows is still an unpublished preview outside main.
- Docdex symbols/impact completed for the four presentation implementations and Swift handle section. Linux dependencies point to existing API/auth/model/vault modules. Swift/Kotlin/JS impact graphs expose no cross-file edges, so manually check callers and tests. Retrieval DAG exported for `mcp-480`.
- Local delegation attempted successfully for concise menu subtitles (`qwen2.5:3b`); primary agent will refine wording to retain permissions and archive meaning.

## Implementation

- Implemented five concise destinations across SwiftUI, Compose, GTK and web. Owned handles and registration now occupy separate pages. Added native/back keyboard navigation and web focus restoration/trapping.
- Android activity recreation previously discarded an open Settings subpage. Preserve saved navigation for the same account/inbox, and reset it on account or inbox changes.
- Prepared Android 0.2.3 (6) and Linux 1.0.2. No release or deployment performed yet.

## Validation

- iOS Simulator and macOS Debug builds pass.
- `DOCDEX_RUN_TESTS_CMD=sh DOCDEX_RUN_TESTS_ARGS=apps/ios/Tests/run.sh docdexd run-tests`: auth/model checks and all 66 handle-controller checks pass.
- Android core tests, lint and debug assembly pass. All 16 native instrumentation checks pass after fixing recreation. Initial test compile used an unavailable Espresso helper; replaced it with existing Android instrumentation input.
- Web `npm test`: 64 pass, including page visibility, Escape/back, focus and text-size persistence. Desktop 1280px and phone 390px browser screenshots show all five rows without horizontal overflow.
- Initial iOS UI run: 7/10 pass. Two new tests incorrectly selected the underlying Inbox navigation bar; scoped them to the actual Settings page. Inbox dismissal assertion now waits for the transition instead of returning immediately when it begins. Rerun pending.
- Generic Docdex test runner initially lacked configuration; explicit runner environment succeeds. CSS/HTML AST is unsupported; DOM/browser validation covers those files.
- Linux native runtime is unavailable on macOS and on the Wodomini host. Use isolated native Linux test packaging/CI, without installing GTK into the production host.
