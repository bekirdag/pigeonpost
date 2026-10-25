# Privacy fixture port allocation progress

- Preserved both Linux failure logs. The second failure proves a public server-startup address
  collided with an earlier captured client address; an actual disclosure has not been inferred.
- A bound-socket handoff experiment failed on macOS, so that approach was rejected without
  changing production listeners. A local-model helper draft used invalid socket APIs and was
  also rejected. The final change separates the fixture address ranges using kernel settings.
- Added real-socket startup-gap and later-address-reuse checks. The complete local privacy gate
  passes **6 tests**; its one listed ignored test is the crash probe executed by the parent.
  Two concurrent whole-process runs pass, and a deliberately injected disclosure is rejected.
  Targeted Clippy with warnings denied and workspace formatting pass.
  The Docdex test runner has no feature-flag argument, so qualification uses the
  explicit Cargo `test-utilities` invocation. Shared build artifacts are reused; no source in
  other worktrees is changed.
- No privacy assertion is skipped, no source log redaction is weakened, and no production
  deployment is part of this change. Hosted validation follows local qualification.
