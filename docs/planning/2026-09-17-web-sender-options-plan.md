# Web sender options parity

Reference: `apps/ios/Shared/Views/PeerInfoSheet.swift` and `Shared/Model/Inbox.swift` at 05df1cf. Scope: static web inbox only.

1. Make the conversation name a keyboard-accessible button that opens a sender dialog, also reachable through the existing information button. Match desktop labels and conditional controls, with Done, Escape/backdrop dismissal, focus restoration and a scrollable narrow-screen layout.
2. Implement Known sender (exact row), Full permissions (effective contact plus server grantable vocabulary), Choose which requests run (exact-row editor), archive/restore, confirmed block/unblock, own-mailbox navigation, addresses and postbox provenance. Preserve namespace rules, existing aliases and policies when marking known. Revocation returns to review with no granted verbs; blocking also clears grants.
3. Scope asynchronous mutations to their original mailbox, serialize sender actions, show pending/error states, and keep failed changes retryable. Existing mailbox switching and conversation reading behavior must remain intact.
4. Add behavioral regressions for controls and request payloads, wildcard/own-mailbox cases, failures, stale responses, nested dialogs and focus. Run the complete web suite, then use the browser against synthetic fixtures for desktop/phone interaction and layout.
5. Commit/push and merge the reviewed scoped change, verify web CI, back up production static files, deploy tested assets, compare public hashes, and record clean Git and deployment evidence.

Dependency evidence: Docdex symbols confirm native policy methods and web render/editor hooks. Impact graphs for JS, HTML, CSS and tests have no indexed edges (classic-script/test coverage limitation), so inspect DOM IDs, script loading and callers directly. Apply markup/style, dialog behavior/policy, then fixtures/tests in dependency order. No backend or native package release is needed.
