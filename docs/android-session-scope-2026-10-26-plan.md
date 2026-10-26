# Android authorization scope before calendar and enterprise

Isolated child of Android message/group checkpoint `5a7ec71`; do not mutate the parent while its full regression/build runs. Keep store submissions batched and Apple held for owner review.

1. Add a tested parent/child authorization lease and scoped token provider in core. Bind token renewal/invalidation to the accepted grant; a retired actor cannot obtain another account's credentials. Guard synchronous sensitive operations in parent-first lock order; allow retirement to wait for outstanding local writes without holding a coroutine lock across suspension.
2. Install a fresh root lease/authorization ID only when Session restores or accepts an authenticated grant. Refresh preserves it. Sign-out, session expiry and accepted reauthentication retire the previous grant. Publish authorization identity into the view-model reset flow so a same-subject new grant is still a new context. Preserve provider login/PKCE/terms behavior and closed-app push cleanup.
3. Create canonical-mailbox scoped postbox/group clients. Retire a child when the actor changes. Bind async token acquisition, renewal, invalidation, network results, group metadata and MLS/storage operations to that lease. GroupStorage must check the scope inside its actual serialized read/write/remove operation; no stale Keystore write can race a new actor load.
4. Qualify parent/child retirement, suspended token refresh/invalidation, same-mailbox new grants, old group callbacks and writes, legitimate other-mailbox operation, fixture compatibility, JVM and native regressions. Keep cancellation separate from a live session-expiry signal.
5. Merge into the Android candidate only after meaningful checks pass; calendar stores and enterprise step-up/device approval screens then use the established context rather than introducing a second mechanism.

Use no new dependencies. Docdex Kotlin graph/symbol coverage may be incomplete; capture that limitation and use exact call sites/compiler/runtime evidence. No production session/token is used in tests; no store upload or new feature flag is part of this boundary.
