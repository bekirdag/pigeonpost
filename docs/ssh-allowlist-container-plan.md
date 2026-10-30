# Confine allowlisted agent commands

A synthetic fixed `git status --short` executed a workspace-configured fsmonitor hook outside the workspace with the existing allowlist environment. Exact argv matching is not a filesystem or execution sandbox.

Keep exact command/path matching, but run accepted allowlist commands directly inside the same restricted container boundary as container mode. Map validated workspace paths to /workspace, preserve no terminal/no shell parsing, and apply rootful-Docker checks, resource limits, network defaults and cleanup. Allow image/runtime settings for both confined modes. A container image missing an approved program must fail closed.

Review configuration and launch dependencies, add regressions for container argv and path mapping, adapt the real SSH harness to require its already-supported container runtime, then run Rust unit/SSH integration and loopback tests. Document upgrade behavior and preserve explicit unconfined mode. No production server restart is needed for an agent-side launch change; CLI binaries must be released separately after qualification.
