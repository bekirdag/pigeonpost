# Android encrypted group actions — progress

Direct-message wire/state/UI checks are qualified and pushed at `7d4a684`. Inspection found the legacy Android group client accepts server `edit_of` as a fallback to decrypted references and uses server replacement hints to hide messages. It also prepares a new encrypted send after ambiguous loss and ignores deletion response seq sets. These boundaries are being corrected before exposing group action controls.

AST/impact inspection: `GroupModels.kt`, `GroupClient.kt`, `GroupStore.kt`; no usable Kotlin dependency edges were returned. Exact call sites and the established Apple `GroupActions`/pending-send implementation supply the contract; the runtime and existing real MLS library remain the qualification source.

## Foundation qualification

The initial complete JVM run passed with 131 cases (123 baseline + eight group action projection/compatibility checks). The expanded focused group run then passed all 23 cases, including 15 with real MLS: authenticated edit/reply and retained file keys; forged server edit/replacement metadata; sender-device mismatch; committed-send loss with imported persisted state and byte-identical explicit retry; the same for edits; and whole-chain deletion including dependent unconfirmed edits and attachment keys. The first expanded compile required an exhaustive Kotlin `when` fallback; the failing log is retained. Evidence: `android-group-actions-foundation-first.log` and `android-group-actions-real-mls-{first,qualified}.log`. This is not yet a group UI or full integrated release qualification.

The first local test draft omitted the required assertions and assumed unverified edits could fold; it was rejected. A constrained retry produced the canonical-sequence assertion loop correctly; primary-reviewed security/MLS cases use the actual projection contract.

## Real postbox and platform compilation

The opt-in real Rust postbox check passed with real Kotlin MLS, hosted-device attestations and member Welcome, authenticated reply/edit and retained sealed file, delivered/seen member cursors and both receipt opt-outs, committed-send and committed-edit loss with state import and identical ciphertext/client ID on explicit retry, and whole-chain deletion reconciled after a missed event with `metadata_only=true`. Log: `android-group-actions-real-postbox-first.log`. The client never decrypts old ciphertext a second time when refreshing tombstones. A superseded row with no retained server ciphertext is handled separately from an explicit deletion, preserving any version already decrypted here.

The integrated foundation passed all 137 JVM cases with no skips/failures, including 15 real MLS scenarios, and `:app:compileDebugKotlin`. Log: `android-group-actions-foundation-platform.log`. Group UI/state integration is the next checkpoint; this foundation alone is not a release candidate.
