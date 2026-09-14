# Address copy and icons progress — 2026-09-14

- Baseline main: `945709cdcf9d002aa5a80a3911426d47dc8d8f5d`, clean and matching origin/main.
- Worktree: `/private/tmp/pigeonpost-address-icon-20260914`; branch `codex/address-copy-icon-20260914`.
- Private evidence: `/private/tmp/pigeonpost-address-icon-audit-20260914`.
- Loaded both Docdex memory lobes, wakeup and operator routine. Direct user publishing authorization satisfies inferred release gates. Read Pigeonpost and planning instructions; saved the user's copy-control and icon consistency preference.
- Confirmed the mismatch visually: iOS icon is the multicolour bird on white, Android app image is a white bird on a rounded navy background. Reuse iOS source artwork.
- Current Swift Settings account address is selectable text without a copy icon. Auditing other display sites before implementation.

## Implementation and validation

- Added canonical address copy controls to iOS/macOS inboxes, mailbox selectors and Account pages; Android inbox, mailbox dialog and Account; Linux inbox and Account; web inbox, selector and Account.
- Copy controls have native touch targets and visible confirmation. Mailbox selection remains separate. Full handles/key addresses are copied rather than shortened display names or labels.
- Android adaptive vector paths already matched the iOS source exactly. Replaced the outdated onboarding/sign-in PNG and exported the same iOS source as the 512px sRGB Play listing asset. The corrected Play listing icon is saved for review.
- Dependency/AST audit completed before edits. Swift/Kotlin impact graphs have no resolved edges, and HTML/CSS have no AST support; checked callsites manually. Linux imports remain UI-to-existing-model/API only. Retrieved DAG trace; no backend/API changes needed.
- Local delegation drafted the shared Swift button; refined its platform sizing, borderless style and cancellable feedback lifecycle.
- Passed: 66 web tests, shared Swift auth/model tests and 66 handle controller checks, Android core tests/lint/debug builds, all 18 native Android workflow tests, 25 Linux core tests, iOS simulator build, universal Developer ID macOS archive.
- Native tests caught test-harness issues: dialog copy buttons need explicit dialog scoping; iOS pasteboard reads from the test runner cause a system paste prompt, so validation uses user-initiated Paste in the app.
- Baseline: `945709cdcf9d002aa5a80a3911426d47dc8d8f5d`. Linux GTK/Flatpak tests subsequently passed on both CI architectures.

- iOS native UI validation completed: 11 tests passed in the full run; the Account copy test needed modal scoping and passed on its focused rerun. Both named and unnamed addresses were pasted into the native compose address field and compared exactly. Shared runtime unchanged by test-harness corrections.

- Browser interaction found and fixed a real edge case: replacing the clicked copy SVG detached the outside-click target and closed the mailbox picker. Copy clicks now stop propagation; the regression clicks the SVG itself. All 66 web tests pass again.
- Both iOS address copy tests passed at the largest accessibility text size; normal size restored. Linux GTK/Flatpak CI passed on x86_64 and ARM64. macOS notarization accepted.
## Published artifacts and deployment

- iOS 1.0 build 37 (`16c5ed77-6e76-4e2c-9c77-33be6aa19015`) is VALID and available to internal and external TestFlight testers. Public review `58735d33-f010-4641-a787-d4c1a35cbcab` is WAITING_FOR_REVIEW with build 37; all 21 review items (app and ten subscription/group versions) were preserved. Public release mode remains AFTER_APPROVAL.
- macOS `macos-1.0-37` is public: universal binary signed for Wodo, notarized and stapled; Gatekeeper accepted. Published ZIP SHA-256 `4ec2318c8c079b5c1db9a43fe851ab4373cfcc06e7eff29c50e948c7b6db6520`. Visually checked the signed app's address row; macOS accessibility automation is unavailable, so a native automated click was not claimed.
- Linux `linux-desktop-1.0.3` is public with x86_64/ARM64 Flatpaks and the Debian package. Downloaded artifacts passed checksum and GitHub attestation verification. Native clipboard tests passed in installed Flatpaks on both architectures.
- Android 0.2.4/code 7 is signed and available on internal testing. The production release at 100% of all targeted countries and the corrected en-US Play icon were submitted together and are under review. Managed publishing is disabled, so approved changes publish automatically.
- Deployed web inbox copy controls and macOS/Linux download pointers with original-file hash guards and a rollback backup at `/var/backups/pigeonpost/address-icon-20260914`. All 61 deployed runtime files matched the feature worktree; service active and the four updated public files passed hash checks. No backend or npm packages changed.
- Real web browser checks verified exact named and long unnamed addresses on the native browser clipboard, preserved current mailbox and kept the picker open after copying. Viewed mobile inbox/picker and desktop Account layouts.
- iOS copy tests passed at the largest accessibility text size; Android copy tests passed at 200% font size. Device text sizes were restored after each run.

## Final integration checks

- PR #16 contains the changes. Full CI at `81993e3` passed Rust on macOS/Linux/Windows, lint/audit, delivery/privacy, web inbox, npm launchers and Linux packaging. Android CI exposed immediate clipboard reads and a hardware-Back transition race; local tests had passed.
- Android tests now wait for the real app window to receive focus, wait for the exact clipboard value after a single native tap, and await the settings destination after hardware Back. No clipboard permissions are bypassed and the clipboard is never written by test code. Local delegation drafted a helper, then its thread handling and type were corrected. All 18 Android workflow tests pass locally after these test-only changes; the final CI run subsequently passed as recorded below.
- The native Windows application remains an unpublished preview outside this release branch. The production web inbox used on Windows includes the new copy controls.

## Final result

- PR #16 merged as `ad0dbe715b58781be6f9fb4855792066b9fdf275` after all 16 checks succeeded or were correctly skipped. Android run `34820447235` passed all 18 native workflows; CI `34820447243` passed the final Windows tests and lifecycle checks; Linux run `34820447284` passed both architectures.
- Local main and all four production Git checkouts were clean and synchronized to the merge. Wodomini verification found 61/61 runtime files matching merged main and the adapter active; the four changed public files returned HTTP 200 with matching hashes. This documentation-only follow-up is included in the final push and checkout synchronization.
- Public releases: macOS 1.0 (37), signed/notarized, and Linux 1.0.3. Both are linked at https://pigeonpost.dev/download. The web inbox update is live.
- iOS 1.0 (37) is in internal/external TestFlight and WAITING_FOR_REVIEW for public App Store release. Ten purchase products and their review items remain attached.
- Android 0.2.4 (7) is available internally and **submitted for public review**. After the Playwright transport disconnected, a fresh MCP server attached through the existing browser's supported bound endpoint, preserving its signed-in session and other tabs. Submitted both pending changes: `0.2.4 (7) - Copy addresses` with a full production rollout, and the corrected default en-US Play icon. Reloading Publishing overview at 2026-09-14 08:45 UTC confirmed both under review, no unsent changes, and managed publishing disabled. Existing countries, screenshots and purchase products are preserved.
- Android review evidence: `/private/tmp/pigeonpost-address-icon-audit-20260914/play-review-final-verified.json` and `play-review-final.png`. Review status is pending Google's decision; public availability is not yet confirmed.
- Temporary HTTP servers are stopped, the task iOS simulator is shut down, and Android font scale is restored. The user's Android production app/emulator and original macOS app remain available.
- Final machine-readable evidence: `/private/tmp/pigeonpost-address-icon-audit-20260914/release-summary.json`, `final-ci-checks.json`, `merge-result.json`, `server-sync.json`, `public-files-final.json` and `wodomini-final-verification.json`.
