# Web desktop parity progress — 2026-09-17

Status: implemented, validated, merged and deployed.

- Audit in progress on isolated branch codex/web-desktop-parity-20260917 at /private/tmp/pigeonpost-web-parity-20260917.
- Confirmed unguarded mailbox response adoption and unconditional bottom scroll on every web render.
- Confirmed current web Settings/account handles and attachment workflows are already present.
- Plan saved before implementation. Production unchanged so far.

- Implemented scoped request cancellation/adoption and loading/retry UI, clearer default-inbox names and mailbox ordering, removal of the duplicate address bar, slash normalization, latest-ten history window with stable nodes and reading anchors, full-history find, confirmed thread deletion, draft ownership, attachment metadata and keyboard/pointer column resizing.
- Added 11 behavior regressions; all passed through `docdexd test run-node`. `docdexd run-tests --target site-inbox` rejects the Cargo-only target mapping, so full web suite uses the established npm script after the targeted Docdex runs.
- Local browser fixture serves only synthetic mail on 127.0.0.1:35807; Google Play signed-in tab preserved. Browser rendering validation underway.

- All 80 web tests passed after adding stale-snapshot, acknowledgement and offline-retry regressions. A final regression also covers sent requests not showing an invented held verdict. Billing checkout contract tests passed.
- Browser validation: 1440×1000 desktop, 820×850 narrow desktop and 390×844 phone; no horizontal overflow. Initial/latest bottom gap was 0 to 0.5px. Refresh preserved the exact reading anchor; prepending ten older messages changed its offset by 0.03125px. Delayed 420px content expansion retained bottom within 0.5px. Mouse resizing changed the column from 320px to 400px and persisted the width. Empty composer overflow corrected to zero. Light/dark find layouts reviewed in screenshots.
- Production inbox HTML/JS/CSS hashes exactly match the clean main baseline; no uncommitted server-only changes would be overwritten. Production CORS preflight returns 204 and allows DELETE from the inbox origin.

- Final validation: 82/82 web tests and 13/13 billing checkout contract tests passed. Docdex staged hook passed. GitHub PR21 inbox-web job 105102310272 succeeded (run 35190669590); formatting/Clippy, native custody lint, conformance, launcher and privacy/custody checks also succeeded at the deployment checkpoint.
- Code commit: `3d5b25c1db1259cd3dc872332874a82852610337`. PR: https://github.com/bekirdag/pigeonpost/pull/21. Merge: `ef0821f2757de34387eaee19b5b1950c561df81d`.
- Deployed to `wodomini:/var/www/pigeonpost-inbox` after backing up the complete root at `/var/www/pigeonpost-inbox.bak-web-20260917T063930Z`. Only `app.js`, `app.css` and `index.html` changed; the other eleven files match the backup. Temporary staging was removed. This web root is static files, not a Git checkout.
- Public HTTPS verification: all three assets returned 200 and their SHA-256 values exactly matched the tested source. Live browser sign-in view loaded the new controls and versioned script without script errors or horizontal overflow.
- Deployed SHA-256: app.js `3cac2082fcc769492e767921184fdb2ae12ed7edb014cb353a5ecd78bd5adca9`; app.css `9b5cd42d73bb59422964012c943e24898277a0dfc1a1ec85bc7939631b5ede82`; index.html `cde405c76069365823b7dcb738d5c39c28d4bdb031d99d0d422299ab50f3e282`.
- Evidence is retained in `/private/tmp/pigeonpost-web-parity-audit-20260917/` (test logs, browser measurements/screenshots, deployment manifest/result and public verification). The synthetic test server was stopped and browser artifacts moved out of the repository.
- Existing broader-repository issue: cargo-audit job 105102310456 reports RUSTSEC-2026-0285 for rustls 0.23.43 (fixed by >=0.23.45). Cargo.lock is unchanged by this browser release. This backend dependency release is not covered by the passing web checks.
