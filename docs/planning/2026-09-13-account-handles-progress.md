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
- Android: JVM tests, debug build and debug lint pass, including canonical API scoping, all providers, expiry, timeouts and account reset. Native instrumentation is running.
- Linux: 25 domain/auth tests pass. Updated native widget tests will run on both architectures in release CI.
- Docdex AST/impact and DAG were used to order shared store/API changes before clients. Docdex's Cargo target wrapper rejected the package directory, so the standard package test runner was used. A local model draft failed validation and was replaced with reviewed code.

## Pending release steps

Native screen inspection; Linux installed-package checks; signed iOS/Android/macOS builds and submissions; backed-up postbox/adapter/site deployment; live verification; Git synchronization and QA cleanup.
