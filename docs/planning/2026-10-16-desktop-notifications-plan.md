# Desktop unread notifications — plan

User authorizes fixes and publication for Windows and any other affected desktop apps.

1. Trace unread state, thread acknowledgement, refresh/stream behavior, native alert/badge integration and release workflows on Windows, macOS and Linux. Use exact released source and isolated worktree; preserve other branches and credentials.
2. Fix per-thread unread/read behavior: only the visible thread in an active window may be acknowledged. Keep counts through background/minimized activity and expose clear thread badges.
3. Add native Windows notifications with safe activation to the correct mailbox/conversation/thread and numeric taskbar/tray indicators. Keep background delivery alive when the window is closed to the tray, provide explicit Quit, prevent duplicate/stale-account alerts and keep notification contents private.
4. Repair demonstrated equivalent desktop defects. Preserve existing account grants and message data; no real-user messages as tests.
5. Validate focused state regressions, native Windows x64/ARM64 compilation, UI/notification integration and packaging. Run affected Mac/Linux tests and build/install checks. Record any OS notification permission limitations accurately.
6. Version, commit and publish affected builds through established channels. Submit signed-by-Store Windows packages with automatic publication, and verify submission status; retain prior release artifacts for rollback. Do not claim certification complete while pending.

Acceptance: unread thread indicators identify where messages arrived; reading thread A does not clear B; background arrivals retain badges and can surface OS alerts; badge totals clear after reading/sign-out; clicking an alert opens its original thread; duplicate refreshes do not repeat alerts. No broad product or server upgrade is intended.

Status: steps1–6 completed. Windows publication is queued with Microsoft; certification remains external and pending. Mac/Linux releases and public download links verified.
