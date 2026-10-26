# Calendar creation receipt encryption

Calendar creation retries retain the original HTTP result for seven days. Event results include
custodial calendar details, so `calendar_idempotency.response` must receive the same at-rest
protection as the event itself. This does not make custodial calendar content end-to-end encrypted.

## Format and startup transition

For `create_calendar` and `create_event`, new responses are Vault-encrypted SQLite BLOBs under the
calendar seal label. The authenticated context is `calendar-creation-receipt-v1:` followed by the
JSON array `[scope, action, request_key, body_hash, status, created_at]`. Changing any of these
fields or copying ciphertext between mailboxes makes the result unavailable. An unreadable result
returns HTTP 503 with `calendar_receipt_unavailable`; the creation callback does not run again.
Conflicting request bodies retain the existing HTTP 409 behavior and seven-day expiry contract.

Legacy TEXT responses are accepted and resealed when replayed. Before HTTP request handling or
delivery workers start, the server also converts all legacy TEXT rows for those two actions in
transactions of at most 64 rows. This includes expired rows and malformed bytes: preserving the
receipt prevents an unreadable result from causing another creation. Metadata and retention do
not change. The operation is repeatable; a failure aborts startup with a content-free error.
If legacy rows exist, startup requires the exact Vault key to have come from a readable, nonempty
`POSTBOX_KMS=sealed-file:...` file. An ephemeral development fallback cannot convert persistent rows.

`import_confirm` receipts already use a separate encrypted BLOB format. `cancel_note` records use
base64-encoded sealed TEXT. Neither is converted by this transition.

## Deployment checks

1. Qualify the committed source with focused receipt tests, the complete postbox suite, strict
   Clippy, formatting and CI. Build and inspect the image while the old containers remain running.
2. Keep a protected SQLite backup, configuration copies and image references on the server. Use
   SQLite's backup API; copying an open database file does not capture a consistent WAL state.
   Preserve the existing master key and all other production configuration.
3. Start the candidate against a server-local copy of the backup with the existing key, no external
   network and no published ports. Verify startup, database integrity, conversion of generic TEXT
   rows, preservation of other formats and repeatable restart. Save logs privately on the server.
4. Qualify the emergency rollback on that disposable copy before replacing production. Then
   deploy the inspected image, retaining the live database. Check postbox and reaper readiness,
   expected images, public health, authentication boundaries and unchanged configuration hashes.
   Inspect only receipt action/type/count metadata, never saved result bodies.

## Rollback limitation

Older binaries read the generic response as TEXT and cannot replay encrypted BLOBs. They reject
those requests before running the creation callback, but can write new plaintext receipts. A
rollback must therefore retain privacy-capable code, or suspend calendar access before starting
the old binary. For an emergency rollback, stop postbox/reaper, snapshot the existing `calendar`
feature-flag rows privately, and set every existing calendar flag to disabled in one transaction.
With no enabled account override and the default-off behavior for absent flags, calendar requests
remain unavailable while other inbox services recover. Keep calendar disabled until a qualified
privacy-capable image is running; restore the saved flags deliberately after that rollout.

Never restore a pre-deployment database over accepted messages or calendar writes. Preserve the
current database and encrypted receipts throughout rollback and recovery. A database backup is
for disaster recovery, not an automatic deployment rollback.

Conversion protects active receipt values; it does not promise physical erasure from old SQLite
WAL pages, freelists, filesystem snapshots or backups. Keep pre-fix artifacts protected under the
existing backup-retention policy. Do not remove owner backups as part of this migration.
