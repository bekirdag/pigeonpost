# Android message actions interoperability progress

- Isolated from Android UI checkpoint `c2752e6`; the other worktree is running the full native UI,
  lint and unsigned release/R8 checks against that unchanged source. All eight new UI cases and
  123 JVM cases already pass before the final visual refinement.
- Read the current real Android and Apple postbox fixtures and the app transport. The existing
  Apple loss proxy already demonstrates the safe pattern: record body hashes/identifiers, never
  bearer headers or message contents, and distinguish a dropped committed response from a
  rejected operation. Its already-qualified source will be preserved.
- Implementation and actual direct-message postbox qualification are pending. No production
  credentials, accounts or messages will be used.

## Real HTTP qualification

The first opt-in Gradle run passed against the current Rust postbox binary on throwaway loopback services: capability discovery, distinct copy IDs/shared references, delivered/seen/agent metadata, folded edits and fresh unread revisions, versions, reply context, foreign references/ownership refusal, sender and recipient receipt preferences, Filtered receipt privacy, and committed-loss send/edit recovery. Both exact retries retained byte-identical request hashes and yielded one accepted copy/version. The proxy saw one initial attempt (no automatic POST replay), then an explicit duplicate recovery. Default JVM tests are unchanged. The private log is `continuation-20261024/validation/android-message-actions-real-postbox.log`. Fixture DB/key/issuer credentials were temporary and removed by its own runner; production was untouched.

Local Qwen scaffold output used nonexistent/obsolete Gradle APIs (`useFinalizedEnvironment`, `mainClassName`) and omitted required task dependencies/validation after automatic fallback attempts. It was rejected; the explicit `JavaExec` task uses supported Gradle 8.13 APIs and passed actual compilation/execution. Kotlin graph/symbol extraction has no usable edges; explicit source checks and the real compiler/runtime qualified these boundaries.
