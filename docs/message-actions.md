# Message actions and receipts: edit, reply, ticks

Client contract for SDS §14C (Phase 1e). The web inbox is the reference implementation; native
clients follow this document. Everything here is gated on `GET /v1/capabilities` `features`:

| Flag | What it switches on | Checked against |
|---|---|---|
| `message_edit` | `edit_of` on `POST /v1/send` | the sending account |
| `message_reply` | `reply_to` / `quote` on `POST /v1/send` | the sending account |
| `receipts` | delivered/seen receipts recorded and shown, `message.receipt` events | the account that sent the message (the one shown the ticks) |
| `agent_status` | `POST /v1/receipts {kind:"agent_status"}`, the gear | the account that sent the message |

With a flag off the server behaves exactly as before: `edit_of`/`reply_to` are refused with
`403 feature_disabled`, and no `receipt`/`agent_status` fields appear.

## 1. Message references (`msg_ref`)

Direct messages are stored as one sealed row per copy (the sender's copy and the recipient's copy
have different `message_id`s). A **`msg_ref`** names the message itself and is shared by both copies.

- Format: `mref_` followed by 26 characters of lowercase base32 (`a-z`, `2-7`): 128 random bits plus
  two padding bits. Clients generate it (so a retry is idempotent); the server generates one when a
  client sends none.
- The server stores it on both rows and also seals it, with the reply quote, into a per-copy metadata
  envelope next to the message (same envelope v3, sealed to each copy's owner).
- `reply_to` and `edit_of` are `msg_ref`s. A client may instead pass the `message_id` of its **own
  copy** of the referenced message; the server translates.
- References resolve only within the same pair of mailboxes. Anything else is `404 reference_not_found`.
- A retry with a `msg_ref` this sender already used for the same recipient returns `200` with the
  original ids and `"duplicate": true` instead of sending twice.

## 2. Sending

`POST /v1/send` takes, in addition to `{to, body, from?, thread_id?, thread?, attachments?}`:

| Field | Meaning |
|---|---|
| `msg_ref` | Optional client-generated reference (above). |
| `reply_to` | `msg_ref` (or own-copy `message_id`) of the message being answered. |
| `quote` | ≤ 200 characters the client shows as the quoted parent; kept sealed, shown if the parent is gone. |
| `edit_of` | `msg_ref` (or own sent-copy `message_id`) of the message being edited. |

`201 {message_id, sent_copy_id, recipient, msg_ref}`.

### Edit rules

- Only the original sender, only their own sent message, within **24 hours** of the original, and
  not once the original has expired (disappearing timer). Errors: `404 reference_not_found`,
  `403 edit_window_closed`, `400 edit_has_attachments` (edits are text only).
- An edit of an edit is an edit of the root: `edit_of` always names the original.
- The edit is delivered as its own row in the same thread as the original, carrying `edit_of`.
  `thread_id`/`thread` in the request are ignored for an edit.
- An edit is a new request for every check: admission, blocking, filtering, stranger throttle, quota
  (its bytes count) and verb evaluation. If the original was filed in Filtered, so is the edit.
- **An edit can never raise a held request to `auto`.** If the recipient's view of the original is
  `review`, the edit is `review` too (`held_because` is the original's reason). If the original was
  `auto`, the edit is evaluated as a new request on its own body.

## 3. Reading

`GET /v1/inbox` gains `actions=1` and `fold=1`. Every listed message also carries these fields
(absent or `null` when not applicable):

```json
{
  "message_id": "…", "msg_ref": "mref_…",
  "reply_to": {"msg_ref": "mref_…", "message_id": "…|null", "from": "/k/…|null",
               "direction": "in|out|null", "excerpt": "first line|null", "quote": "client quote|null",
               "verb": "make_change|null", "args": {…}|null},
  "edit_of": "mref_…|null", "edit_of_message_id": "…|null",
  "edited_at": 1700000000, "edit_count": 2,
  "receipt": {"state": "sent|delivered|seen", "delivered_at": 0, "seen_at": null},
  "agent_status": {"state": "working|done|failed", "at": 0}
}
```

- `receipt` and `agent_status` appear on **sent copies** only (`direction: "out"`).
- `edited_at`/`edit_count` appear on an original that has edits in this mailbox.
- `excerpt` is computed from this mailbox's own copy of the parent; `quote` is what the replying
  client supplied, for when the parent is gone.

**Views:**

| Query | Edit rows | Bodies |
|---|---|---|
| neither (legacy) | listed as separate messages | an edit's body is prefixed `Edited: ` (inside `note`/`args.task` for a request envelope) and it is always held (`held_because: "edit"`); a reply to prose gets a `> quote` first line |
| `actions=1` | listed as separate messages with `edit_of` | raw |
| `actions=1&fold=1` | **omitted**; the original shows the latest text | raw; the original's `read` is false while any of its edits is unread |

Agents (agentd, MCP) use `actions=1`, since each edit is an event they must act on. People's
clients use `actions=1&fold=1`. A client using `fold=1` acks with
`POST /v1/ack {message_id, with_edits: true}` so the original's versions are acknowledged together.

`GET /v1/messages/{message_id}/info` (own copy, root or edit id) returns:

```json
{"message_id": "…", "msg_ref": "mref_…", "direction": "in|out", "sent_at": 0,
 "versions": [{"message_id": "…", "body": "…", "at": 0}],
 "reply_to": {…}|null, "receipt": {…}|null, "agent_status": {…}|null}
```

`versions` is oldest first: the original, then each edit. Deleting the original
(`POST /v1/messages/delete`) deletes every version in that mailbox.

## 4. Receipts

```
sent ──► delivered ──► seen
            └──► working ──► done | failed     (agent recipients; shown over delivered/seen)
```

| State | Recorded when |
|---|---|
| sent | `POST /v1/send` returned 201. |
| delivered | The recipient lists it (`GET /v1/inbox`, any view, including `?filtered=1`), or posts `kind: "delivered"`. agentd posts it when it spools the event. |
| seen | The recipient acks it (`POST /v1/ack`), or posts `kind: "seen"` (conversation opened). agentd posts it when a run picks it up. |
| working / done / failed | agentd posts `kind: "agent_status"`. |

`POST /v1/receipts` (recipient side, any credential that may read the mailbox):

```json
{"identity": "/k/…?", "message_ids": ["…"], "msg_refs": ["mref_…"],
 "kind": "delivered|seen|agent_status", "status": "working|done|failed"}
```

At most 100 references per call, 60 calls per minute per mailbox (`429 rate_limited`). Only the
caller's own received copies count; anything else is silently skipped. `200 {updated: n}`.
States only move forward (`delivered` never undoes `seen`; `done`/`failed` are final for a run, but a
new `working` after `done` is accepted, for a follow-up run on an edit).

**Storage.** The sender's copy stores `delivered_at`, `seen_at`, `agent_status`, `agent_status_at`.
Every change emits `message.receipt` to the sender's account:
`{msg_ref, message_id (the sender's copy), mailbox, state, at}`.
An edit emits `message.edited` to the recipient's account:
`{msg_ref, edit_of, message_id, original_message_id, mailbox}`.

**Privacy:**

- **Send read receipts** (`read_receipts`, per mailbox, default on) in `GET/PUT /v1/policy`. Off: the
  mailbox records no seen receipts for what it reads, and its own sent copies never show `seen`
  (masked to `delivered`), as in common messengers. Delivered receipts are always on.
- Agent status is the agentd machine's choice (`report_status = true` in `agentd.toml`, default on).
- A sender the recipient has **blocked** gets no receipts at all (it stays at sent).
- A message in **Filtered** never reports more than delivered (no seen, no agent status) until the
  recipient accepts the sender. Delivered happens on any listing, so a sender can't tell Filtered
  from a normal inbox.

## 5. Groups

Groups keep their E2EE model (`docs/groups.md`): edits and replies are `edit_of`/`reply_to` seqs on
`POST /v1/groups/{id}/messages`. For ticks:

- `GET /v1/groups/{id}/receipts` → `{members: [{address, handle, delivered_seq, seen_seq}],
  read_receipts}` for active members other than the caller. `delivered_seq` advances when that
  member's client lists messages; `seen_seq` is their read cursor, `null` when they turned read
  receipts off (and every member's `seen_seq` is `null` to a caller who turned them off).
- A message with seq *n* is delivered when every listed member has `delivered_seq ≥ n`, and seen
  (green) when every member with a visible `seen_seq` has `seen_seq ≥ n` and at least one is visible.
  Message info lists the names in each state.

## 6. UI (all clients)

- **Action row below and outside the bubble**, aligned to its side: received → copy, reply; sent →
  edit (only while editable: own, ≤ 24 h, not expired, `message_edit` on), copy. It is always
  visible, as in the reference screenshot: quiet (70% opacity) on desktop until the message is hovered
  or focused, and compact on touch screens. Icons are 18 px line icons in 30 px buttons (28 px on touch). The long-press/right-click menu keeps the full
  list: reply, copy, edit, info, delete, report, block.
- **Ticks inside the bubble**, right of the time, on sent messages:
  - sending: clock; sent: one gray ✓; delivered: two gray ✓✓; seen: two green ✓✓;
  - `agent_status: working`: a small green gear rotating slowly (static plus "Working…" under
    reduced motion); `done` reverts to the receipt ticks; `failed`: a small red ⚠.
  - Accessible labels: "Sent", "Delivered", "Seen 10:05", "Agent working on this", "Agent run failed".
- **"edited"** label beside the time on an edited message; message info shows versions.
- **Reply**: the composer shows the quoted parent (sender, first line) with ✕/Esc to cancel; the
  bubble shows the quote above the text; tapping it scrolls to the parent and highlights it.
- **Edit**: inline in the bubble, or the composer in edit mode with an "Editing message" bar;
  Enter/Send saves, Esc/✕ cancels.
- **Settings → Privacy**: "Send read receipts" toggle.
