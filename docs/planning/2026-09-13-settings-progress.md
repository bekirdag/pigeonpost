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
- Prepared Android 0.2.3 (6) and Linux 1.0.2; rollout evidence is recorded below.

## Validation

- iOS Simulator and macOS Debug builds pass.
- `DOCDEX_RUN_TESTS_CMD=sh DOCDEX_RUN_TESTS_ARGS=apps/ios/Tests/run.sh docdexd run-tests`: auth/model checks and all 66 handle-controller checks pass.
- Android core tests, lint and debug assembly pass. All 16 native instrumentation checks pass after fixing recreation. Initial test compile used an unavailable Espresso helper; replaced it with existing Android instrumentation input.
- Web `npm test`: 64 pass, including page visibility, Escape/back, focus and text-size persistence. Desktop 1280px and phone 390px browser screenshots show all five rows without horizontal overflow.
- Initial iOS UI run: 7/10 pass. Two new tests incorrectly selected the underlying Inbox navigation bar; scoped them to the actual Settings page. Inbox dismissal assertion now waits for the transition instead of returning immediately when it begins. Final rerun passed all 10 checks (see below).
- Generic Docdex test runner initially lacked configuration; explicit runner environment succeeds. CSS/HTML AST is unsupported; DOM/browser validation covers those files.
- Linux native runtime is unavailable on macOS and on the Wodomini host. Use isolated native Linux test packaging/CI, without installing GTK into the production host.


## Native validation completed

- Final iOS UI run: all 10 purchase/navigation tests pass (`ios-ui-tests-pass.log`, `ios-settings-pass.xcresult`). Found and fixed a real startup race: the Handles row now observes and waits for its stable store before allowing navigation, preventing a blank purchase page. Restored Done on every navigation page. Fixture launch staging now runs only once, so selecting an inbox does not reopen Settings during QA.
- Linux Actions run `34779476791`: both x86_64 and aarch64 Debian/Flatpak builds, GTK navigation tests and installed package integration pass. Downloaded and inspected native Settings/Account/Handles screenshots.
- Android signed 0.2.3 (6) bundle and APK built; upload certificate SHA-256 matches the existing Play upload key. No billing controller or price changes.
- macOS visual QA uses a development-signed build. The existing local installation uses an Apple Development certificate; a Developer ID debug copy prompted for its keychain item. Declined that request and closed only the task-created process; no keychain password or access rule changed.
- App Store Connect's browser login expired. Asked the user to sign in again while continuing builds. API credentials remain usable for uploads and TestFlight distribution.

## Release validation and publication

- Final native source is `b205fd6d8a8a26041ca71971537a16f2457859e7`. Decorative Swift icons stay fixed-size at accessibility text sizes; rebuilt and visually rechecked the iPhone layout. Mac Settings and Handles pages, Android normal and 1.5x font, web desktop/mobile, and Linux native screenshots were visually inspected.
- Linux `linux-desktop-1.0.2` release run `34780084681` passed for x86_64 and ARM64. All three published Debian/Flatpak assets match SHA256SUMS and pass GitHub attestation verification.
- macOS `macos-1.0-36` is published. Universal Developer ID archive passed notarization run `34780581740`; downloaded the final asset and verified its stapled ticket and Gatekeeper acceptance. SHA-256: `f91fd341270b1a5f9052e6b7b965804d904de703e659143fc151d777db27a777`.
- iOS build 36 uploaded in run `34780301959`, processed VALID and assigned to internal/external groups. Another build is still in beta review; replacement remains pending.
- Android 0.2.3 (6) is available to internal testers. Public production promotion is being prepared; no supported devices were lost. Play's sole advisory is absent debug symbols in a pre-stripped third-party native dependency.
- Download URL/test impact graphs have no edges. Updated only release pointers after verification; retained automatic OS suggestions, all platform icons and the single homepage Download link.
