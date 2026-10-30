# SSH teardown review progress

- PR #189 CI run 40981656230, job 123822897517, failed waiting for the revoked session's command to exit. The original log is retained privately. Baseline is main 994ab822.
- Reviewed spawn, teardown, russh channel handling and the loopback harness. Local delegated test draft used incorrect channel variants and did not enforce rejection; rejected it. Deterministic protocol regressions and lifecycle fencing are being qualified before any merge.

- Both deterministic protocol tests failed before the fix: a retired session accepted exec with and without a terminal. After the shared closed-state/launch-registration fence, all 23 shell checks pass.
- `docdexd run-tests` rejects this Cargo source-directory target; used its exact Cargo fallback. Symbols/impact/DAG succeeded before the implementation change.
- Real compiled postbox/tunnel/agentd/client loopback passes 61 checks with Docker exercised, including remote PID-based revocation/idle cleanup and all existing isolation/authentication checks. Formatting and Python compilation pass.
- Include this fix in review PR #189, whose CI exposed the race; preserve the original failure and local before/after logs in the private validation directory. No production binary or app submission changed.
