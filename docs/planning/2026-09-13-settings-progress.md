# Settings progress — 2026-09-13

## Final outcome

Settings now has five friendly destinations and focused subpages across iOS, macOS, Android, Linux and the web inbox. macOS 1.0 (36), Linux 1.0.2 and web changes are live. Android 0.2.3 (6) and iOS 1.0 (36) are available internally and submitted for public store review; external TestFlight review is queued. Public store approval remains with Apple and Google. All PR checks passed and PR #15 merged at `f9406e701fd40b68b4fc879c46f9ae2c31fb6363`.

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
- iOS build 36 uploaded in run `34780301959`, processed VALID and assigned to internal/external groups. The initial beta submission was blocked by build 34; this is resolved below.
- Android 0.2.3 (6) is available to internal testers. Public production promotion was prepared and then submitted; no supported devices were lost. Play's sole advisory is absent debug symbols in a pre-stripped third-party native dependency.
- Download URL/test impact graphs have no edges. Updated only release pointers after verification; retained automatic OS suggestions, all platform icons and the single homepage Download link.

## Final store and deployment evidence

- Public iOS version 1.0 now selects build 36 and is WAITING_FOR_REVIEW (submission `397e48c6-069c-4f1b-b0db-3a8c8820b6cc`). Recreated the review through Apple API 4.4.1 and asserted the same app, ten subscription versions and ten subscription group versions (21 items). Updated reviewer navigation paths without modifying demo credentials. Two existing listing screenshots remain COMPLETE and show inbox/conversation pages unaffected by this refactor.
- Expired the superseded TestFlight build 34 after internal build 36 became available; the queued beta review was released. Build 36 is now IN_BETA_TESTING internally and WAITING_FOR_BETA_REVIEW externally. The earlier browser sign-in blocker is resolved; no user sign-in is needed.
- Android 0.2.3 (6) is available internally and submitted for public production review at 100% rollout in all configured countries. Reviewer access instructions now point to Handles > Get a handle, Account > Delete account and Help and about > Support; this metadata update was also submitted for review.
- Deployed four static files with hash guards, atomic replacements and backups at `/var/backups/pigeonpost/settings-20260913`. All 61 production runtime files match the release manifest, the adapter service is active, and public content hashes match. Live web Settings root/Handles/Escape navigation passed. All six desktop archive/checksum URLs respond HTTP 200.
- Android final PR Actions run `34780843269` passed unit tests, lint, release packaging and all native device workflow tests. Final Linux run `34780843255` passed both architectures. The full runtime-source CI run `34780274083` passed all checks; all checks in final PR run `34780843273` also passed, including Windows.
- Shut down the task iOS simulator, restored Android font scale, removed the task Android debug app, stopped the local QA web server, and closed only task-created Mac app processes. The user's installed apps and unrelated browser tabs remain intact.
- Production HTML, CSS and JavaScript responses use `Cache-Control: no-cache`, so existing browsers revalidate the changed Settings assets. No cache-policy change is needed.
- PR #15 is merged at `f9406e701fd40b68b4fc879c46f9ae2c31fb6363`. Local main and all four server Git checkouts were fast-forwarded to that commit, with no tracked/untracked changes and zero ahead/behind. The final documentation commit is also synchronized by the same guarded script; exact final results are recorded in the private `server-sync.json`.
- No backend API, billing-controller, server-container or npm package changes were needed for this presentation update.
