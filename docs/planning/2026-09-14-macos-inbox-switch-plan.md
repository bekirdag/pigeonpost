# Responsive macOS inbox switching

## Acceptance criteria

Switching from the mailbox picker or a peer's "Visit inbox" action immediately opens a fresh load for the selected identity. Previous requests cannot populate the new mailbox, including rapid A → B → A navigation. Show a native spinner with the selected address and useful loading text; show failure/retry or empty states instead of an unexplained blank list. Keep mailbox switching and Settings usable while loading. Preserve account authentication, message data and existing permissions.

## Implementation and validation

1. Load Docdex profile, repo and conversation context; read the scoped Linux test thread. Trace macOS navigation, shared Inbox loads/polling, network cancellation and model rebuilding. Refresh stale symbols, inspect impact and DAG, and check manual Swift callsites where graph edges are unavailable.
2. Reproduce the delayed/stale mailbox response defect using controlled async requests. Add regression coverage for rapid switches, cancellation, old response/error isolation, failure/retry, loading completion and empty inboxes. Check UI responsiveness and spinner states in a native macOS build using isolated fixtures.
3. Key the macOS load/live task to mailbox identity and reset screen state consistently. Capture a mailbox generation around shared async reads, reject stale completions, and make loading/failure state accurate. Keep background metadata from delaying usable messages unnecessarily. Investigate any measured CPU stalls rather than assuming every delay is network-related.
4. Run focused model regressions, shared Swift tests, native macOS build and relevant iOS compatibility build because Inbox is shared. Review changes and update the progress record with real results.
5. Under the existing release authorization, publish a new signed universal macOS build after notarization, verify the downloadable artifact and update the website's download pointer with a rollback path. Commit/push and synchronize clean production checkouts. Preserve unrelated apps, browser sessions and worktrees.

## Scope

Worktree: `/private/tmp/pigeonpost-mac-inbox-switch-20260914`; baseline `880d245104b3a034930155eb77c0e567dba0ea36`. Private evidence: `/private/tmp/pigeonpost-mac-inbox-audit-20260914`. Linux reply inspection does not authorize resending or bypassing the existing held deployment request.
