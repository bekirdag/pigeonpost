# Restore Windows threads column and publish

Authorization: the user explicitly requests restoring the missing Windows threads column and publishing a new version. Base main dbf3463; use isolated branch codex/windows-threads-column-20261013. Preserve the original iOS checkout. Publication and normal git/version steps are already authorized.

## Implementation and acceptance

1. Trace the current Subjects pane, width rules, selection bindings and native tests. Keep the three-column desktop experience (conversations, subjects/threads, messages) visible by default, including ordinary windows below 1100 logical pixels. Reserve usable message width at the narrow end of desktop mode. Retain the compact navigation below 760 logical pixels and optional hide/show control.
2. Preserve subject selection, per-subject drafts, mailbox selection, message reading, attachments, search and resizing. Use existing controls and bindings without changing server/API behavior.
3. Extend real Windows UI automation to assert default visibility, usable three-column geometry at normal/medium widths, thread list selection, draft preservation, optional hide/show and compact-to-desktop restoration. Run existing core tests locally and x64/ARM64 builds, release launch, native UI and MSIX installation checks through the established Windows workflow. Inspect native screenshots.
4. Bump app and package versions to 1.0.6 / 1.0.6.0. Commit/push, validate and merge the scoped change. Download the tested Store packages, verify versions/identity/architectures and hashes, and keep fixture builds out of the submitted packages.
5. Create the next Microsoft Store submission through the existing signed-in Partner Center session. Replace x64/ARM64 packages, refresh affected listing text/screenshots and release notes, preserve existing settings and reviewer credentials, submit with automatic publication after certification. Verify the resulting status and distinguish certification pending from publicly live.

## Evidence and rollback

Record source files, test/build commands and outputs, native screenshots, package digests, commit/CI links and the Store submission identity/status in a separate progress log. Microsoft certification is external; do not claim public availability before it is shown. Keep the previous Store release available until the new submission completes; a regression requires a higher-version correction rather than distributing unsigned upload artifacts.

## Completion

Implementation, native validation, package verification, merge and Store submission completed October 13, 2026. Submission 7 is in certification with automatic publication; public availability remains an external Microsoft step. The progress ledger records the existing unrelated Rust audit finding and all release evidence.
