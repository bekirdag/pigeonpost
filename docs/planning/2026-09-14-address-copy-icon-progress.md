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
- Android adaptive vector paths already matched the iOS source exactly. Replaced the outdated onboarding/sign-in PNG and exported the same iOS source as the 512px sRGB Play listing asset. Store listing update is in progress.
- Dependency/AST audit completed before edits. Swift/Kotlin impact graphs have no resolved edges, and HTML/CSS have no AST support; checked callsites manually. Linux imports remain UI-to-existing-model/API only. Retrieved DAG trace; no backend/API changes needed.
- Local delegation drafted the shared Swift button; refined its platform sizing, borderless style and cancellable feedback lifecycle.
- Passed: 66 web tests, shared Swift auth/model tests and 66 handle controller checks, Android core tests/lint/debug builds, all 18 native Android workflow tests, 25 Linux core tests, iOS simulator build, universal Developer ID macOS archive.
- Native tests caught test-harness issues: dialog copy buttons need explicit dialog scoping; iOS pasteboard reads from the test runner cause a system paste prompt, so validation now uses user-initiated Paste in the app. Android rerun passed; iOS rerun in progress.
- Linux GTK/Flatpak tests will run on both CI architectures. Releases/deployment are still pending. Baseline: 945709cdcf9d002aa5a80a3911426d47dc8d8f5d.
