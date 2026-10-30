# Message storage encoding (postbox)

How a hosted message is kept on the postbox's disk, where the bytes go, what was cut on
2026-10-28, and what is left because it is protocol rather than storage.

## Layers of one stored copy

Every send writes two rows to `messages` (the recipient's copy, sealed to the recipient, and the
sender's own copy, sealed to the sender) and, for each, a row in `message_meta`.

A copy's `wrap_blob` holds one `envelope::Wrap` (protocol v3, `pigeonpost-core/src/envelope.rs`):

1. **Rumor**: `{from, to, created_at, body}` as JSON, keys as hex. Padded to a multiple of 256
   bytes, then encrypted (+16 byte tag).
2. **Seal**: `{sender_pubkey, nonce, ciphertext, attribution_sk, signature}` as JSON with the rumor
   ciphertext in **hex** (2x). Padded to a multiple of 256, encrypted again (+16).
3. **Wrap**: the outer fields (`version`, `ephemeral_pubkey`, `recipient`, `nonce`, `created_at`,
   `pow_nonce`, `signature`, optional `attribution`) plus the seal ciphertext.

Until now the server stored the wrap exactly as the wire carries it: `serde_json::to_vec(&wrap)`.
That puts the ciphertext in hex again (2x) and the fixed-size fields as JSON decimal arrays
(about 3.6 bytes per byte), plus about 130 bytes of keys and punctuation.

`message_meta.meta_blob` was a **second full wrap per copy** (about 3.8 KB) sealing
`{v, msg_ref, reply_to, edit_of, quote}`. `msg_ref`, `reply_to` and `edit_of` are already plain
columns; the only reader, `msgactions::sealed_quote`, uses just `quote`.

## What changed

- **Compact at-rest record.** `Wrap::to_storage_bytes()` / `Wrap::from_storage_bytes()` in
  `pigeonpost-core`. Layout, little-endian:
  `0xB1 | version | ephemeral_pubkey[32] | recipient[32] | nonce[24] | created_at u64 |
  pow_nonce u64 | signature[64] | flags u8 | (attribution: u32 len + its JSON) | u32 len |
  ciphertext`. 175 bytes of framing, ciphertext raw. `0xB1` is a UTF-8 continuation byte and can
  never begin JSON, so the reader tells the formats apart by the first byte and still accepts
  legacy JSON. Truncated input, trailing bytes, unknown flags and oversized lengths are rejected.
  The attribution block, when present, keeps its JSON form to preserve its own versioned shape.
- **One path in and out.** The postbox writes through `store::encode_wrap` and reads through
  `store::decode_wrap` (`crates/pigeonpost-postbox/src/store/wraps.rs`). Stored wrap bytes never
  leave the server: listings, the event stream and exports return opened text or metadata only,
  so no API changed shape.
- **No metadata envelope without a quote.** `meta_blob` is NULL unless the send carries a quote.
  Older rows that have one are still read. Old images already treat NULL as "no quote".
- Ids, signatures and proof of work are computed over the binary fields, not over the JSON, so
  re-encoding does not change any of them (covered by the codec tests).
- Quotas (`SUM(LENGTH(wrap_blob))`) follow automatically: a mailbox holds about twice as much.

## Measured bytes

From `cargo test -p pigeonpost-postbox stored_bytes_per_message -- --ignored --nocapture`
(`wrap_blob` + `meta_blob`, SQLite row and index overhead excluded):

| body | per copy, JSON | per copy, compact | per send, before (2 copies + 2 meta) | per send, after | per send, after, with a quote |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 20 B | 2,758 | 1,215 | 13,028 | 2,430 | 5,884 |
| 300 B | 3,772 | 1,727 | 15,102 | 3,454 | 6,908 |
| 2,000 B | 10,954 | 5,311 | 29,439 | 10,622 | 14,076 |

Capacity for 1 GB of message payload, one send = both copies:

| body | sends before | sends after |
| ---: | ---: | ---: |
| 20 B | ~77,000 | ~412,000 |
| 300 B | ~66,000 | ~290,000 |
| 2,000 B | ~34,000 | ~94,000 |

## What is inherent

- **Inner double hex and JSON.** The seal carries the rumor ciphertext as hex inside JSON, and the
  rumor's keys are hex too. That is inside the encryption, so the server cannot re-encode it. A
  20-byte body still costs a 1,040-byte ciphertext. Removing it needs an **envelope v4** with a
  binary seal and rumor, coordinated with the loft wire format, the client, the MCP server and
  compliance tooling. Out of scope here.
- **256-byte padding** at both layers is kept on purpose: it makes stored and transmitted size leak
  a coarse bucket rather than the exact length.
- **Two copies per send.** The sender's own copy is sealed to the sender so the server cannot read
  it either; that is the design.
- `meta_blob` is not counted by the quota (it never was).

## Rollout and rollback

- Writer: `POSTBOX_WRAP_STORAGE=compact` (default) or `json`. The server refuses to start on any
  other value. Readers always accept both.
- Convert existing rows: `pigeonpost-postbox --rewrite-wraps compact|json [--apply]` against
  `POSTBOX_DB_PATH`. Dry run by default (prints counts and bytes before/after). 500 rows per
  `BEGIN IMMEDIATE` transaction, each converted row decoded and compared with the original before
  it is written; rows already in the target format and rows that do not decode are left alone, so
  it is idempotent and safe to rerun or resume.

**Rolling back to an image older than this change** (one that reads only JSON):

1. Set `POSTBOX_WRAP_STORAGE=json` on the current image and restart it, so no new compact rows are
   written.
2. `POSTBOX_DB_PATH=... pigeonpost-postbox --rewrite-wraps json` (dry run), then the same with
   `--apply`. Rerun the dry run and require `converted 0` and `undecodable 0` for both
   columns. A zero conversion count alone is insufficient: corrupt compact records are skipped,
   not converted. With the writer still in JSON mode, also require both counts below to be zero:

   ```sql
   SELECT COUNT(*) FROM messages WHERE substr(wrap_blob, 1, 1) = X'B1';
   SELECT COUNT(*) FROM message_meta WHERE substr(meta_blob, 1, 1) = X'B1';
   ```

   Stop the downgrade and investigate any undecodable or remaining compact rows; do not delete
   messages to make this check pass.
3. Deploy the older image.

Rows rewritten to JSON decode to the identical wrap; their JSON is in the shape the image before
this change wrote and reads. Rolling back further than that image is outside this procedure. NULL
`meta_blob` rows need no conversion. Rows written as compact by this image are unreadable by older
images until step 2 has run, so do not skip it.
