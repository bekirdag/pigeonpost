# Allowlist confinement progress

- Baseline 78a8118e. Reproduced locally with an isolated temporary repository and a harmless outside-workspace marker: fixed git status invoked a configured fsmonitor hook. Temporary files removed.
- Dedicated branch/worktree created; implementation and qualification pending. No real agent or server configuration changed.

- Exact-root Docdex symbols/impact returned index-writer backoff; direct call-site review covered config → launch → agent process teardown and the SSH harness. Both modes now share isolation/cleanup; approved argv is the container entrypoint. Added real SSH checks for mapped paths, exact execution and an approved script attempting an outside-host read/write. Positive runtime-dependent execution is qualified in the mandatory container harness.

- Shared container launch now supplies the exact approved executable as the entrypoint; validated workspace paths map to `/workspace`. Container settings apply to both modes. Runtime/image absence fails closed.
- Configuration/unit fixtures and the real SSH harness cover the new boundary. The harness executes approved workspace code attempting to read/write outside the mount. The in-memory protocol test checks refusal; positive command execution is covered with the actual runtime.
- `docdexd run-tests --target crates/pigeonpost-cli/src/shell` rejected that Cargo target (`invalid_argument`); using the exact `cargo test --locked -p pigeonpost-cli shell::` equivalent.

## Qualification

- Local `cargo test --locked -p pigeonpost-cli shell::`: 21/21 pass, including missing-runtime refusal with no host fallback.
- Real compiled postbox/tunnel/agentd/client loopback: 60/60 checks pass with Docker exercised (no skips). Approved cat/echo and workspace hook execute inside the container; host reads/writes, traversal, symlink escape, shell syntax and forwarding are refused. Teardown, ciphertext-only broker capture, step-up, key rotation and audit checks pass.
- `cargo fmt --all --check` and Python syntax checks pass. Exact-root Docdex recovered: agent.rs inbound shell/mod.rs, outbound config.rs/confine.rs; no wider launch callers.
- Updated the optional no-container harness mode to report accepted-command execution as skipped, rather than falsely claim a missing runtime when the operator explicitly disabled that lane. CI still requires real container execution.
- This is an agent-side release change. No production agent opt-in or installed binary was modified.
