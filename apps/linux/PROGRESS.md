# Linux desktop progress

## 2026-09-13 — planning and repository inspection

- Created isolated branch `codex/linux-desktop-20260913` from clean main `e70d8bb`.
- Saved `BUILD-PLAN.md` before implementation. Read the native macOS inbox, thread, account, contacts and API contracts.
- Selected GTK 4/libadwaita with Python/PyGObject and Secret Service. Existing Mac, Windows, mobile and Rust CLI builds remain independent.
- Docdex profile, repo memory, wake-up, source symbols and graph/DAG inspection completed. The download HTML has no indexed inbound/outbound code dependencies; its behavior is coupled through DOM selectors in `site/download.js` and covered by `site-inbox/test/download.test.mjs`.
- Linux testing host is Ubuntu 26.04 x86_64. Prefer an isolated Ubuntu 24.04 container; local macOS Docker daemon is unavailable. No new Linux package or website download is published yet.

## Pending

- Implement app and native packaging.
- Provision and validate Linux account authorization.
- Run unit, GTK, installed-package and live messaging checks.
- Publish verified packages, update downloads and deploy/synchronize repositories.
