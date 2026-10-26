# Android message actions: real postbox qualification

Qualify the Android core client's actual JSON and retry behavior against the production Rust
implementation on throwaway loopback services. This work is isolated from the UI qualification
worktree so installed-source regression tests remain unchanged.

1. Add a focused Node fixture under `tests/e2e-messages/`, following the existing tested issuer,
   server and loss-proxy patterns. Generate temporary keys/accounts and enable only the needed
   flags on that disposable server. Keep credentials in a protected setup file and never print
   them. Use temporary database/blob/loft paths; inspect and isolate registry/network behavior.
2. Add a Kotlin executable check harness using `PostboxClient` from the app core. Check explicit
   capabilities, sender/receiver references and copy IDs, folded edit/ack/version behavior, reply
   context, recipient receipt privacy and cross-pair refusal. Exercise real committed-response
   loss with the same request identity/body and exactly one accepted message/version.
3. Add an explicit Gradle JavaExec task and a local runner using the existing dependencies. Keep
   default JVM tests unchanged: this integration target runs only when requested. Ensure cleanup
   ends only the runner's own issuer/proxy/postbox processes and temporary files.
4. Run the real checks, record the source and results, then merge the isolated work back into the
   Android candidate. Group action integration, calendar/enterprise screens and store release
   gates remain separate checkpoints; no store submission is authorized by a test run.
