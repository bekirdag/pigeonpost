# Android native round two

Continue the big-features handoff and SDS §§14B, 14C and 26. Keep this work isolated from the
qualified Apple candidate and server receipt migration. Store releases remain batched; no Play
submission is part of an intermediate checkpoint.

## Checkpoints and dependency order

1. Direct-message edit, reply and receipts. Follow `docs/message-actions.md` and the deployed
   postbox contract: capability negotiation, folded inbox reads, acknowledgments with edits,
   stable request references, message versions, privacy control and the specified action row.
2. Encrypted group actions and receipts. Authenticate references inside MLS plaintext, fold only
   verified same-sender chains, preserve deletion/expiry semantics and show missing history.
   Keep real cryptography tests mandatory for qualification.
3. Calendar and meetings, following `docs/calendar.md` and the current server contract, with
   strict account/mailbox isolation, frozen uncertain requests, timezone/recurrence handling,
   invitations, imports/exports and meeting navigation.
4. Organization rooms and device approval. Use canonical authorized mailboxes and explicit
   delegation/role facts, strong authentication and safe App Link routing. Do not infer authority
   from handle prefixes or enable enterprise flags as part of client implementation.
5. Build and qualify the integrated Android candidate, then prepare the batched release artifact
   and physical-device checklist. Preserve existing upload signing credentials privately.

## First checkpoint files and validation

- Extend `core/Models.kt` and add `core/MessageActions.kt` for typed references, reply/receipt/info
  projections and edit-window rules. Preserve legacy defaults and separate presentation from
  request autonomy. Extend `Safety.kt`'s capability/policy models as required by the wire.
- Extend `core/PostboxApi.kt` without forcing legacy fake transports to claim support. Send
  explicit actions/fold only after negotiation. Expose typed send/ack/info behavior and keep
  uncertain-send request identities stable.
- Update `core/InboxStore.kt` and `Conversations.kt` to carry folded metadata, composer modes,
  cancellation and scope-safe results. Do not allow an edit to become a new ordinary send after
  the selected mailbox or conversation changes. Preserve ordinary drafts and attachments.
- Update `ui/Thread.kt`, `ui/MessageEditor.kt`, `ui/Privacy.kt` and related view-model glue to show
  compact accessible action rows, reply context, editing state, ticks/agent state, message info and
  the read-receipt preference using the established Compose theme.
- Add meaningful core wire/state tests and emulator interaction cases to the existing suites.
  Cover disabled features, version folding and acknowledgments, response loss/idempotence,
  stale scope results, edit refusal/expiry, privacy masking and reply navigation. Check existing
  regression tests and the R8 build before qualifying the full platform candidate.

Kotlin symbol/graph results are currently incomplete in Docdex. Use explicit call-site inspection,
the compiler and focused state/wire tests for dependency checks, and record that limitation. No
new dependency is planned. Consult the real server and existing reference clients before filling
in any contract detail.
