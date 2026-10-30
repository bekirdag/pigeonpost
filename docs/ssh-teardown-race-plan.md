# Fence command launch after SSH teardown

The documentation PR's real SSH loopback test ended the client but still found the long-running command. Source inspection found that teardown consumes the current process metadata without marking the session closed; a delayed russh handler can subsequently spawn a command into that retired session. The harness also uses global command-line matching, which can mistake the client for the remote command.

Add deterministic protocol tests that finish teardown before an exec request, with and without a terminal. Confirm failure on the original implementation. Serialize launch registration and teardown under the existing process-state mutex, marking the state closed before cleanup. Preserve process-group killing and container removal for commands already launched. Tighten the loopback check to identify the remote fixture process rather than any matching client command line, without relaxing termination assertions.

Docdex symbols and impact recovered after initial writer backoff. The changed private functions are in shell/agent.rs, imported by shell/mod.rs; launch configuration comes from config.rs/confine.rs. Validate the focused shell suite, real Docker/SSH loopback and CI. This is an agent-side source fix; installed CLI/agentd binaries require a separate release. Do not change app submissions or restart production services.
