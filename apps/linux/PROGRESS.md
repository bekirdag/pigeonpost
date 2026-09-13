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
- Final release workflow [34773663739](https://github.com/bekirdag/pigeonpost/actions/runs/34773663739) passed on Ubuntu 24.04 x86_64 and ARM64. Both installed sandboxes passed Secret Service save/read/clear, document-portal mount and actual forwarded-file reads. Real GTK windows and installed Flatpak launch screenshots were inspected. Native and Flatpak tests share a standard desktop bus so the portal mount remains valid.
- Live production OAuth, keyring persistence, refresh, inbox creation, send/receive, subjects, attachment upload/download, acknowledgement, contact policy and archive/restore were exercised between two QA mailboxes. No purchase or message to another person was made.
- Live GTK account loading exposed a server bug: `GET /v1/quota` ignored the identity selector and returned 400 for accounts with multiple mailboxes. The HTTP regression failed before the repair and passed afterward; all 232 postbox tests passed on macOS and in the production Linux release build. The query/header selector uses the existing ownership resolver and rejects foreign mailboxes. This also benefits Mac/Android callers.
- Deployed `pigeonpost-postbox:linux-quota-2f7d726` for the service and reaper after a canary check. Preserved original master key, a consistent SQLite backup, configuration and stopped rollback containers under `/opt/pigeonpost-linux-quota-20260913`. Runtime configuration and public health were verified. The installed Debian app then loaded and rendered live messages and attachments successfully.
- Removed both QA mailboxes and their messages, revoked the session, verified an empty native keyring, deleted the isolated QA sign-in account and removed temporary credentials. No unrelated user account or conversation was changed.
- Published [Linux Desktop 1.0.0](https://github.com/bekirdag/pigeonpost/releases/tag/linux-desktop-1.0.0), with x86_64/ARM64 Flatpak bundles, an architecture-independent Debian package, SHA256SUMS and verified GitHub build attestations. All three public downloads returned 200 and matched their checksums. The CLI latest-release pointer remains `v0.7.18`.
- Deployed the Linux downloads at [pigeonpost.dev/download](https://pigeonpost.dev/download) with ARM64 detection, all alternatives and installation guidance. All 61 website tests pass. The homepage retains one Download link. The guarded deployment verifies previous/new hashes and keeps the original files in `/var/backups/pigeonpost/linux-downloads-20260913`.
- Docdex's test runner assumes Cargo targets in this workspace and rejects Python/Node targets. Those tests use their native runners; Docdex impact analysis and staged-change hooks remain in use. A local Docdex model drafted desktop metadata; its incomplete Rust draft was rejected during validation.

## Release notes and limits

- [PR #13](https://github.com/bekirdag/pigeonpost/pull/13) merged the implementation and server repair. Release source is tag `linux-desktop-1.0.0` (`b2edd5c`). Later documentation updates do not change packaged runtime files.
- Linux dependencies and UI were tested on Ubuntu 24.04 under X11/Xvfb on both processor architectures. Wayland support uses GTK/Flatpak's standard Wayland interface; physical Wayland, KDE and other distribution sessions were not exercised. Debian 12 meets the declared dependency floors; it was not separately installed for this release.
- Direct Flatpak bundles require installing a newer release to update. They are not yet listed on Flathub. The GNOME runtime is downloaded on first install.
- Handle checkout and account deletion use the existing website. No offline message database or system tray is included. Notifications arrive while the app runs.
