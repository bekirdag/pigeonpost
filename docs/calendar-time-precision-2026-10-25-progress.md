# Calendar time precision — progress

Plan: [correction plan](calendar-time-precision-2026-10-25-plan.md).

- Isolated worktree/branch starts from production source845a02a. Apple calendar PR96 is frozen at
  992c7ff for CI; its local40 model/36 draft/20 transport/24 state/34 real-server checks,8 UI cases
  on each device, both builds and Mac runtime layout checks pass. Native stores remain held.
- Confirmed `fmt_local` truncates seconds; it is used for persistence as well as response formatting.
  New-worktree index initialization returned writer backoff. Direct scoped reads work; index/graph
  readiness and focused regression are next. No source correction or production change yet.

## Reproduction and correction

- Index readiness recovered. Symbols/AST and impact analysis confirm callers in event persistence/
  projections, import/export and calendar types. Added two real socket tests before changing the
  helper: recurrence save/read/edit/move preserves its instant,90-second duration and original id;
  ICS preview/confirm/read/export preserves17/47-second boundaries.
- Both fail against the original implementation (`calendar-seconds-before.log`): saved recurrence
  starts17 seconds early and import preview loses the seconds. The corrected helper retains
  nonzero seconds and keeps the old minute form for zero-second input. Both regressions pass
  (`calendar-seconds-focused.log`). Full postbox qualification passes:451 package tests plus2
  avatar worker tests, Clippy with warnings denied, and workspace formatting.
- Production read-only aggregate audit:0 timed calendar events,0 stored second-precision values,
  0 cached timestamp disagreements,0 unknown zones. No event content or identities were printed;
  no data repair is needed for this deployment. Historical values that an older version already
  truncated are not guessed or rewritten by this correction.
- Docdex's test runner cannot map the nested Cargo package (documented unsupported-target error),
  so used the package's actual Cargo runner. Two real regression tests ran; the separate worker
  target's zero matching filtered tests are not counted as validation.
