# Calendar import confirmation — progress

Plan: [retry correction plan](calendar-import-retry-2026-10-25-plan.md).

- Isolated branch starts from production main a5389f8. Scoped Docdex binding/memory loaded; initial
  impact calls report writer backoff while the new index initializes. Direct scoped reads work.
- The original `calendars.rs::import_confirm` deleted its draft after creating each selected index.
  Real HTTP regressions reproduced a 404 after a lost response and four copies from two repeated
  indices. A second regression caught omitted versus explicit default-calendar equivalence.
- Confirmation now normalizes indices, rejects more than 200 input entries, and stores a sealed
  receipt in the existing idempotency table under `import_confirm`. Its authenticated context binds
  mailbox, draft and canonical selection/destination. Replay rechecks current calendar ownership,
  returns the original result for seven days and emits no new reminders or account notices.
- Four real HTTP tests cover replay, default equivalence, deduplication, changed selections/targets,
  another mailbox, expiry, deleted destinations and sealed storage. All four pass. The final full
  postbox suite passes 455 tests plus two worker tests; strict Clippy and formatting also pass.
  Private before/after logs are in the continuation release's validation directory.
- API reference and OpenAPI now describe the selection bound and replay contract. Scoped graph
  analysis identifies the calendar module/event code and the OpenAPI route-drift test as consumers.
- No production rollout yet. Qualification, PR and pinned candidate/rollback startup remain next.
  Old binaries ignore the new action and retain their earlier consumed-draft 404 behavior.
- Separate pre-store hardening finding: the existing generic create idempotency helper stores full
  response JSON in plaintext for seven days. This correction does not change that older cache;
  it needs a compatibility-aware fix before the native release batch.
