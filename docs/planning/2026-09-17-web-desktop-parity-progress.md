# Web desktop parity progress — 2026-09-17

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
