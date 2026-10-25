# Native scheduled meetings — progress

Plan: [meeting integration plan](native-meetings-2026-10-25-plan.md).

- Isolated worktree starts at PR96 harness correction072c581. PR96 remains in draft; all store
  submissions remain held. PR97 independently corrects second precision in the backend.
- Existing calls.js `joinScheduled` already enforces call-service admission and returns joined,
  waiting, choose-occurrence or error states. Calendar details currently show only the code.
- CallHost configures after awaiting a token but only rechecks page readiness; it can publish a
  captured old mailbox after a switch or detach. Renewal has the same missing generation guard.
  The root views own the single call overlay; calendar sheets need explicit dismissal first.
- Added an actual WKWebView/CallHost loopback harness with controllable synthetic token delivery.
  Before correction all5 checks fail: switch, detach, identity release, page reload and renewal.
  All5 now pass after binding/page/configuration generation guards and confirmed page configuration.
  The native message handler also accepts only its trusted main frame. No real tokens, media or
  accounts are used (`native-call-host-before.log`, `native-call-host-scoped.log`).
- Shared bridge now has an explicit member-only scheduled join, strips authority fields, settles
  abandoned renewals/joins, and bounds the pending join. Calendar UI integration is next.

## Scheduled join implementation and checks

- Calendar details now offer voice/video joins for a specific occurrence. The root captures the
  mailbox/calendar generation and waits for sheet dismissal before starting the call overlay.
  Declined/cancelled occurrences cannot use these controls; series details ask for an occurrence.
- Actual WK harness passes17 checks, including pending direct calls and cold-launch ring answers,
  precise occurrence forwarding, lobby waiting, window refusal, old bridge failure and cancellation.
  Both full Apple app builds pass. Both new44pt voice/video UI dismissal checks pass on iPhone and
  iPad (`native-meetings-{iphone,ipad}-joins.xcresult`); fixture calls intentionally remain detached.
- Shared calls client now owns pending scheduled joins by room object and stream generation.
  Late media is released; a late refusal cannot end a replacement call; an accepted abandoned join
  leaves with its original token/session; a stale401 cannot renew as a new account. All4 regressions
  fail with the previous calls.js and pass with the correction. All14 focused bridge/alerting/race
  checks pass and the complete207-case inbox suite passes. Stream shutdown also settles readiness
  waiters and rejects frames/completions from replaced streams.
- Docdex run-tests rejects the nested web target mapping, so used its actual npm runner; retained
  `native-meetings-web-runner.log` and `native-meetings-web-suite.log`. Debug fixture paths remain
  excluded by Docdex policy; scoped filesystem reads and app builds validate that fixture.

## Independent backend rollout

- PR97 merged at a5389f8 after all CI passed; production source and postbox/reaper image now match
  `pigeonpost-postbox:calendar-seconds-7053774` (image be603e400151...). Server-local SQLite backup
  `/var/backups/pigeonpost/postbox-20261025T062402Z` passed quick_check and candidate/rollback startup.
  Both containers are running with0 restarts, environment files match the backup byte-for-byte,
  public health returns200 and anonymous capabilities still return401. No native store submission.

## Final cancellation review

- Direct/group placement and ring answering now stop when capture reports cancellation. Captured
  tracks waiting for noise setup are registered immediately, so hang-up releases the microphone
  without waiting for a module/network operation. Two additional real-client regressions fail at
  d32d92e and pass with the correction; no real media device is used.
- All16 focused bridge/alerting/cancellation checks and the complete209-case inbox suite pass
  (`scheduled-races-final.log`, `pending-capture-before.log`, `native-meetings-web-final.log`).
  Updated each call page's script version so cached clients receive the correction at web rollout.
- Merged PR96's5f89bbb accessibility correction into this branch, preserving the new join cases.
  Full10-case local device runs and the final universal Mac build pass. All native stores remain
  held. Non-scheduled HTTP completion ownership and device-switch processing remain on the phase9
  review scope; these checks are specific to the verified paths, not a complete call security audit.

- Hosted Mac14/26 (including the actual WK harness), real MLS/calendar, inbox and Rust checks pass.
  Windows x64/ARM64/core pass after one retry: the first call-harness failure occurred in its
  browser-peer startup, before the native app was launched. Hosted iPhone passes the full suite.
  One iPad join case queried hittability before the detail control existed; it now waits for that
  exact button first. Both focused join cases pass locally after this correction
  (`native-meetings-wait-joins-ipad.xcresult`). Hosted iPad requalification is required.
