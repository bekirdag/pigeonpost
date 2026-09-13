# Linux desktop progress

## 2026-09-13 — planning and repository inspection

- Created isolated branch `codex/linux-desktop-20260913` from clean main `e70d8bb`.
- Saved `BUILD-PLAN.md` before implementation. Read the native macOS inbox, thread, account, contacts and API contracts.
- Selected GTK 4/libadwaita with Python/PyGObject and Secret Service. Existing Mac, Windows, mobile and Rust CLI builds remain independent.
- Docdex profile, repo memory, wake-up, source symbols and graph/DAG inspection completed. The download HTML has no indexed inbound/outbound code dependencies; its behavior is coupled through DOM selectors in `site/download.js` and covered by `site-inbox/test/download.test.mjs`.
- Linux testing host is Ubuntu 26.04 x86_64. Prefer an isolated Ubuntu 24.04 container; local macOS Docker daemon is unavailable. No new Linux package or website download is published yet.

## Implementation and validation

- Implemented native three-column GTK inbox, mailbox/conversation/subject selection, search, composer and per-conversation drafts, file upload/save, contacts and permission vocabulary, quota, archive/restore, handles and account pages. Reused the actual Mac app icon. No fixture mode is shipped.
- Added public `pigeonpost-linux` OAuth client with device authorization and explicit consent. Provisioning uses the existing issuer's native account configuration; no password grant or embedded secret is enabled. Browser consent was exercised with an isolated QA account.
- 25 authentication/domain regressions pass locally. Eight GTK integration tests pass, including draft isolation, send subject, late-response discard, account clearing and page construction. Screenshots checked in light/dark themes and at 850-pixel window width.
- GitHub Actions run [34772496009](https://github.com/bekirdag/pigeonpost/actions/runs/34772496009) built and installed Debian and Flatpak packages on Ubuntu 24.04 x86_64 and ARM64. Both installed sandboxes passed Secret Service save/read/clear and document-portal mount checks. Subsequent file-forwarding coverage and mailbox selector repair are being revalidated.
- Live production OAuth, keyring persistence, refresh, inbox creation, send/receive, subjects, attachment upload/download, acknowledgement, contact policy and archive/restore were exercised between two QA mailboxes. No purchase or message to another person was made.
- Live GTK account loading exposed a server bug: `GET /v1/quota` ignored the identity selector and returned 400 for accounts with multiple mailboxes. Added an HTTP regression that failed before the repair, then a narrow query/header selector fix using the existing authenticated ownership resolver. This also benefits Mac/Android callers. Server tests, rollout and final live UI validation are in progress.
- Website changes are prepared for the versioned Linux packages, with ARM64 detection, all alternatives and installation guidance. They are not deployed before package publication.
- Docdex's test runner assumes Cargo targets in this workspace and rejects Python/Node targets. Those tests use their native runners; Docdex impact analysis and staged-change hooks remain in use. A local Docdex model drafted desktop metadata; its incomplete Rust draft was rejected during validation.

## Remaining release gates

- Deploy the tested quota repair, finish live installed-app verification and remove temporary QA credentials.
- Verify final x86_64/ARM64 artifacts, provenance and checksums; publish the desktop release.
- Deploy downloads with a fresh backup and verify browser OS cases, links, server health and clean synchronized repositories.
