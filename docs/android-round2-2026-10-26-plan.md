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

## Composer state and retries

Use one composer mode per mailbox/peer/subject draft. Editing saves and restores the ordinary draft
and its staged files; preserve a structured request's verb and unrelated arguments. Normalize the
General draft's subject to null so the server assigning its default thread ID cannot hide an
unconfirmed attempt. Capture the full send before scheduling its coroutine and retain the same
reference/body/uploaded attachment IDs for explicit in-process retries. An uncertain attempt
locks that draft until it is resolved or explicitly dismissed; legacy unkeyed sends keep their
existing behavior. Account reset retires those requests; restarting the app does not yet restore
them from encrypted storage and is a separate recovery/hardening item.

Fold versions only through the negotiated API, and acknowledge them together. Optimistic read
marks include the edited revision so a fresh edit cannot inherit the old acknowledgment. Message
info belongs to the current mailbox/conversation/dialog generation. Metadata reload after handle
registration gets its own generation; it must not retire the active authorization or strand an
in-flight send. A future enterprise step-up flow must carry explicit Session authorization
generation through the Android stores and token provider before that flow is exposed.
