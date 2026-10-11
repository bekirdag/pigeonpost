# Windows conversation UI refresh and Store release

User authorization: implement the proposed first UI pass and publish the update to Microsoft Store (2026-10-11). Work starts from `origin/main` b118dbef in an isolated worktree. Existing staged/pasted/dropped attachments and a visible copy button on every message remain requirements.

## Implementation

1. Replace the permanent three-column layout with a two-pane default. Offer a subject switcher and optional pinned subject pane on wide windows; use conversation/back navigation below the two-pane breakpoint. Preserve selection and drafts when changing layout or switching subjects.
2. Simplify navigation: compact archive filter, account menu, on-demand conversation search with Ctrl+F, and contextual subject actions. Keep the current mailbox identity visible and account actions accessible even for an account with no mailbox.
3. Improve conversation rows with initials, last-activity labels, concise previews and explicit unread/needs-review indicators. Use consistent light/dark/high-contrast resources, quieter dividers and readable typography.
4. Put staged attachments, text input and attachment/send actions in one composer. Preserve multi-file removal, clipboard and drag/drop flows, per-subject drafts, retry semantics and scroll position. Rich Markdown and expanded attachment viewing are a later pass.
5. Update Windows UI automation for the new navigation and verify compact, medium and pinned layouts, search, subject selection, mailbox switching, staged files, copying, keyboard access and theme behavior. Add focused core behavior coverage only for meaningful new presentation logic.

## Dependencies and validation

- Core presentation properties precede XAML bindings; layout behavior then binds the named controls. Update native tests after controls and accessibility names stabilize.
- Docdex C# symbols resolve. Its impact graphs contain no edges for these Windows files and XAML/PowerShell symbols are unavailable, so inspect partial-class, binding and test references explicitly; an empty graph is not proof of isolation.
- Run existing core tests locally through Docdex where supported. Compile/test/package on Windows Actions for x64 and ARM64; inspect resulting native screenshots. Require native launch, separate fixture UI tests and installed MSIX activation to pass before submission.
- Confirm the currently published Store version, increment matching assembly/MSIX versions, and produce release packages from the reviewed commit. Store packages must exclude UI_TESTS fixtures.

## Publication

1. Commit/push the scoped change and create a PR, validate required checks, then merge the tested change.
2. Download and hash verified x64/ARM64 MSIX artifacts. Update Store release notes and screenshots to match the shipped UI. Preserve account, pricing, age ratings and certification credentials.
3. Submit the update with automatic publication after Microsoft certification. Record the submission/version and exact portal status; do not call a pending certification publicly live.
4. Keep the previous source/version evidence as the rollback reference. If a release regression appears, ship a corrected higher-version package through the same checks; do not overwrite existing release artifacts.

No changes to the backend, other platform apps, billing, reminders or unrelated shared browser tabs are part of this update.

## Outcome

Implementation and all validation gates are complete. PR #37 is merged and version 1.0.5.0
for x64/ARM64 is submitted as Microsoft Store Submission 6, in certification with automatic
publication after approval. Exact hashes, Actions runs and portal status are in the progress log.
