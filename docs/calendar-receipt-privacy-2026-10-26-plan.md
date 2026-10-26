# Calendar creation receipt privacy

## Confirmed problem and scope

`calendar/mod.rs::idempotent` stores complete creation replies as plaintext JSON for seven days. Event replies duplicate otherwise sealed details. Its two callers are event and calendar creation. Import confirmations already use a separate sealed receipt, and cancellation notes contain base64-encoded sealed bytes; preserve both formats.

## Implementation and validation

1. Extract the generic helper into a focused calendar receipt module. Write encrypted BLOB responses with the existing Vault and calendar seal label. Bind scope, action, request key, body hash, status and creation time through a versioned, unambiguous authenticated context.
2. Read both legacy TEXT and encrypted BLOB receipts. Replay the original status/body without running the creation callback or effects. Reject a changed body as before; malformed or unauthenticatable saved replies fail closed, retaining the key and avoiding another creation.
3. Before public/internal request handling starts, migrate the two generic legacy actions in bounded transactions. Preserve original bytes, metadata, retention and other receipt actions, including malformed legacy data that must remain unavailable. Make migration repeatable and require the existing master key. No schema or dependency addition.
4. Update the two creation callers to supply their existing Vault. Keep public request/response contracts stable. Document the encrypted format, legacy transition and rollback limits: older binaries cannot replay BLOB responses, so a rollback must retain the privacy-capable code or suspend calendar writes while restoring a compatible binary; never restore an old database over accepted new writes.
5. Add real HTTP/database tests for replay and single effects, raw storage, legacy replay/migration/restart, cross-context swaps and corruption, expiry and body conflicts. Run focused tests, the complete postbox suite, format and strict Clippy, plus the relevant native calendar integration.
6. Commit/push and qualify CI. Deploy only the qualified source after server-local backup and isolated candidate startup checks. Keep backups protected on the server; verify postbox/reaper, public health and unchanged credentials/configuration. Record actual evidence separately.

Native calendar authorization work proceeds independently in its own worktree. No store submission is part of this correction.
