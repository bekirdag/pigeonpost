# Account handle consistency progress

## Initial audit

- Started from clean, current main `b0e1355` in isolated worktree `codex/account-handles-20260913`.
- Read Pigeonpost and planning/progress instructions; loaded Docdex profile, repository memory, wake-up context and operator checklist.
- The account adapter already reads the canonical postbox `/v1/me/handles` alongside web billing. However, it converts postbox failures to an empty handle list. Further tracing and reproduction are in progress.
- Plan saved before implementation. Production is unchanged for this task.

## Findings and implementation

- All purchase routes and all native/web sign-ins resolve the verified OIDC subject through `accounts.oidc_sub`; provider receipts are bound to that internal account. No ownership transfer or payment extension is needed. Production aggregate inspection found two active Apple namespaces and two expired Google namespaces.
- Added explicit `include_inactive=true` to `/v1/me/handles`, retaining the active-only default. Returned display state does not grant namespace access. Queries include only the current owner, even after a name transfer.
- The adapter propagates handle/auth failures instead of reporting an empty account. The website shows Apple, Google and Pigeonpost management correctly, retains expired names, prevents an old web subscription overriding canonical ownership, refreshes on demand/focus, and renews expired member tokens for inbox calls.
- Added independent canonical account-handle views to iOS/macOS, Android, Linux and web inbox. Apple/Google purchase counts and payment flows remain provider-specific. Account data is cleared on sign-out and delayed responses cannot repopulate another account.
- Android 0.2.2 (5) and Linux 1.0.1 are prepared. Apple native targets compile; signed uploads and public download changes await release verification. Windows currently uses the web download fallback; its unpublished preview is outside the shipping source tree.

## Validation so far

- Postbox: 233 tests and Clippy pass. Mixed Apple/Google/web ownership, stable subject mapping, expiry, another account and transferred-name isolation are covered.
- Adapter: 17 tests pass, including real HTTP overview tests for mixed providers, expired names, billing outages, rejected member tokens and malformed responses.
- Website/inbox: 63 tests pass, including provider-specific controls, duplicate billing records, lookup error/retry, and web inbox account settings.
- Swift: authentication/model checks and 66 purchase-controller checks pass, including cross-store ownership when Apple is unavailable and late account responses. iOS simulator and macOS targets build successfully.
- Android: JVM tests, debug build and debug lint pass, including canonical API scoping, all providers, expiry, timeouts and account reset. All 15 native instrumentation tests passed locally. CI exposed an offscreen Material text-field label in the existing preview registration test; the test now finds the editable field inside the Settings dialog before scrolling. The corrected registration flow also passes locally.
- Linux: 25 domain/auth tests pass. Native GTK and installed Debian/Flatpak checks passed on x86_64 and aarch64. Linux 1.0.1 is published with checksums and provenance attestations.
- Docdex AST/impact and DAG were used to order shared store/API changes before clients. Docdex's Cargo target wrapper rejected the package directory, so the standard package test runner was used. A local model draft failed validation and was replaced with reviewed code.

## Completion

Implementation, production deployment, desktop publication and mobile submissions are complete. Apple and Google public approvals, and Apple external beta approval, remain store-controlled review steps.

## Production rollout

- Deployed postbox and reaper image `pigeonpost-postbox:account-handles-86c5142` after 233 release-mode tests, a copied-database canary and an integrity-checked database/key backup. Existing environment, mounts and ports were verified unchanged. Rollback containers and backups are retained.
- Deployed the adapter, account page and web inbox with guarded prior-content hashes and file backups. The adapter is active; served asset hashes match the source. Anonymous canonical ownership and account overview requests return 401.
- iOS build 34 is processed, assigned to the existing internal and external groups and submitted for external beta review. Public review `7a692f9b-1456-42c6-bc8a-f356260fd048` is WAITING_FOR_REVIEW with build 34 and all 21 original items preserved (app plus ten subscriptions and ten group localizations). Release is automatic after approval. External beta is WAITING_FOR_BETA_REVIEW; internal testing is active.
- Universal macOS build 34 is Developer ID signed, notarized and stapled; local Gatekeeper accepts it. The immutable release is published. Android 0.2.2 (5) is signed with the existing upload key and available to existing internal testers. The production release passed Google automatic checks and is in review for full rollout to all configured countries; existing listing and reviewer access changes remain included.
- Updated download links only after macOS and Linux release assets became available and their checksums were verified.

## Final evidence

- PR #14 merged as `2defcd2` after all checks on `87b8c03` passed, including Windows lifecycle checks, Android 15 native tests, Linux installed-package checks, Rust/Clippy/audit and web tests. The final desktop-link/account subset also passed all 47 checks.
- Signed distribution source is `86c5142`; subsequent changes only update downloads, a UI test selector and this release ledger. No npm package changed or needed publication.
- Inspected the real iOS and Android Settings screens with mixed-provider fixtures. Tested the deployed account page with controlled Apple/Google/web records; it shows the right provider, expiry and management links without browser errors. A separate live sign-in using the existing dedicated review account showed the same registered handle on both the production account page and web inbox.
- All 61 deployed website/inbox/developer/adapter files match the repository manifest. Postbox and reaper are healthy on the new image; ownership aggregates are unchanged. The unused canary database copy was removed while backups and rollback containers were retained.
- Root main and all four known server Git checkouts were fast-forwarded cleanly to the merged source with zero ahead/behind divergence. A final documentation-only update records this evidence and is synchronized in the same way.
- QA browser credentials were kept private and the temporary copy removed. Signed out and closed the task-created account tab, removed the temporary Android debug app, shut down only the task-booted iOS QA simulator, and retained screenshots/build logs outside the repository.
- Private validation/rollout evidence: `/private/tmp/pigeonpost-account-handles-audit-20260913`. Public desktop releases: `macos-1.0-34` and `linux-desktop-1.0.1`; download page suggests the matching OS package.
