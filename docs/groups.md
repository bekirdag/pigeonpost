# Groups and disappearing messages — client contract (Phase 3a)

Status: implemented in the postbox and the web inbox (Phase 3a, 2026-10-20). Native clients follow
in Phase 3b and must implement exactly this contract. Cryptography, wire formats and the threat
model are in `docs/group-e2ee.md` (P-2); product rules are SDS §4 and §17.

Everything below is `/v1` on the postbox (`https://postbox.pigeonpost.dev`), JSON unless stated.

## 0. Conventions

| Item | Rule |
|---|---|
| Auth | `Authorization: Bearer <member JWT | pk_live_… | cap_…>` as for every other route. |
| Acting mailbox | `x-pigeonpost-identity: /k/…` (or `?identity=`). Required when the credential owns several mailboxes. A group member is a **mailbox**, never an account. |
| Device | `x-pigeonpost-device: <device_id>` on every device-scoped route (marked **D**). The device must be registered for the acting mailbox and not revoked. |
| Bytes | Standard base64 with padding. |
| Idempotency | Mutating routes marked **K** take `request_key` (1–64 of `[A-Za-z0-9_-]`). Same key + same body → the original response; same key + different body → `409 idempotency_conflict`. Keys are scoped to (mailbox, group, action) and kept 7 days. |
| Revisions | `groups.revision` and `group_members.revision` are monotonic integers. Routes that take `expected_revision` refuse a stale one with `409 revision_conflict {current_revision}`; reload and retry. |
| Flags | `groups` gates every route in §1–§4 (off → `404 feature_disabled`). `disappearing` gates timers (§5). Ask `GET /v1/capabilities` (`features[]`) before showing either. A client that cannot load the group crypto library must hide groups entirely; there is no fallback. |
| Errors | `{error, message}` plus the fields named per error. Codes: `not_member`, `not_admin`, `owner_only`, `posting_restricted`, `banned`, `group_full`, `invalid_link`, `invite_required`, `already_member`, `human_required`, `group_deleted`, `revision_conflict`, `epoch_conflict {current_epoch}`, `key_activation_pending`, `encryption_unsupported`, `device_exists`, `device_revoked`, `unknown_device`, `idempotency_conflict`, `key_packages_exhausted`, `resync_required` (410), `rate_limited` (429, `Retry-After`), `quota_exceeded`, `feature_disabled`. |

Human-only actions: settings, roles, removal/ban/unban, invitations, link reset, ownership transfer,
deletion, accepting a private invitation and registering a second device all require a person's
interactive session (member JWT). API keys, mailbox `cap_` tokens and connectors get
`403 human_required` (SDS §4.4 "Agents in groups"). Agents may read, post, leave, self-update their
keys and join public groups.

## 1. Devices and key packages (P-2 §7.1)

| Method | Path | Body / query | Result |
|---|---|---|---|
| POST | `/v1/mls/devices` | `{device_id, signature_key, attestation?}` | `201 {device_id, fingerprint, fingerprint_digits, attestation, mailbox_public_key, created_at}`. The postbox holds hosted mailboxes' identity keys, so it signs the attestation payload (`attestation_payload(mailbox, device_id, signature_key)`) and the client calls `installAttestation(mailbox_public_key, attestation)`. A supplied `attestation` is verified against the mailbox key instead. The first device of a mailbox may be registered by any credential for it; later ones need a human session (and step-up when the deployment configures it). `409 device_exists`. Emits `group.device {change:"added"}` to the mailbox's own account and to every co-member. |
| GET | `/v1/mls/devices` | `?mailbox=` | `{mailbox, devices:[{device_id, fingerprint, fingerprint_digits, signature_key, created_at}]}` — own mailbox, or one that shares a group with the acting mailbox. |
| DELETE | `/v1/mls/devices/{device_id}` | | Revokes. The acting device may revoke itself; revoking another device of the mailbox needs a human session. Unclaimed key packages are deleted, posts from it are refused at once, and every group it is in gets a pending removal. |
| POST | `/v1/mls/key-packages` **D K** | `{request_key, key_packages:[b64], last_resort?: b64}` | `{stored, unclaimed, last_resort}`. Each package must inspect clean, name the acting mailbox and device and carry the registered signature key, with ≥ 1 day of lifetime left. ≤ 100 unclaimed per device; a new last-resort package replaces the old one. |
| GET | `/v1/mls/key-packages/count` **D** | | `{unclaimed, last_resort}`. Top up below 10 (`mls.key_packages_low` also says so). |
| GET | `/v1/mls/welcomes` **D** | | `{welcomes:[{welcome_id, group_id, epoch, seq, welcome}]}`. After `joinFromWelcome`, read the group from `after_seq = seq`. |
| POST | `/v1/mls/welcomes/{welcome_id}/ack` **D K** | `{request_key, joined, error_code?}` | `joined:false` marks this device's leaf stale: it is removed by the next commit and then re-added with a fresh key package. |

Device ids: 1–64 of `[A-Za-z0-9._-]`, unique per mailbox, never reused for a different key unless
the old one is revoked. The web inbox uses `web-<12 random base32>`.

## 2. Groups

`group` objects:

```json
{
  "id": "grp_…", "name": "…", "description": null, "avatar_id": null,
  "join_policy": "private|public", "post_policy": "everyone|admins", "visibility": "unlisted|listed",
  "members_can_invite": false, "members_can_change_timer": false, "ephemeral_secs": null,
  "revision": 3, "epoch": 2, "activation_state": "provisioning|active", "org_id": null,
  "member_count": 3, "max_members": 1000, "created_by": "/k/…", "created_at": 0, "last_at": 0,
  "last_seq": 12, "deleted_at": null, "has_link": false,
  "my": {"role": "owner|admin|member|restricted", "state": "active|invited|keying|left|removed|banned",
         "read_cursor": 10, "unread": 1, "notify": "all|mentions|none", "muted_until": null,
         "invited_by": "/k/…", "joined_at": 0, "filtered": false, "revision": 1}
}
```

An invited mailbox sees only `{id, name, description, member_count, join_policy, my}`; it gets no
history, keys or calls. `unread` counts application messages after `read_cursor` from others.

| Method | Path | Body / query | Result and rules |
|---|---|---|---|
| POST | `/v1/groups` **K** | `{request_key, name (1–80), description? (≤500), join_policy?, post_policy?, visibility?, members_can_invite?, members_can_change_timer?, ephemeral_secs?, invite?: [address\|handle]}` | `201 {group, invitations:[{address, status}]}`. Starts `provisioning`, creator `owner`/`keying`. Invitations are recorded but stay invisible until activation. 20 groups/day per account. |
| GET | `/v1/groups` | `?state=active,invited,keying` (default all three) | `{groups:[group]}` for the acting mailbox, most recent first; deleted groups the mailbox was in appear once with `deleted_at`. |
| GET | `/v1/groups/{id}` | | `group`. `404 not_found` when the mailbox has no relationship to it (a known id grants nothing). |
| PATCH | `/v1/groups/{id}` | `{expected_revision, name?, description?, join_policy?, post_policy?, visibility?, members_can_invite?, members_can_change_timer?, ephemeral_secs?}` | Owner/admin; `ephemeral_secs` also by members when `members_can_change_timer`. Human only. `{group}`; notice + `group.settings`. |
| DELETE | `/v1/groups/{id}` | | Owner. Soft delete; members see `group.deleted`; content purged within 30 days. |
| GET | `/v1/groups/{id}/members` | `?state=` | `{members:[{address, handle, role, state, invited_by, joined_at, revision}]}`. Active members see active/keying members; admins also see invitations and bans. |
| POST | `/v1/groups/{id}/members` **K** | `{request_key, invite:[address\|handle]}` | Admins, or members when `members_can_invite`. ≤ 200 per request, ≤ 1,000 per day per group. `{results:[{address, status: invited\|already_member\|banned\|not_admitted\|unresolved\|group_full}]}`. A recipient who blocked the inviter gets no record (`not_admitted`). |
| PATCH | `/v1/groups/{id}/members/{address}` | `{expected_revision, role?: admin\|member\|restricted, state?: removed\|banned}` | `{address}` URL-encoded. Promote: owner/admins. Demote an admin: owner only, or the admin themself. Restrict/remove/ban: admins, not against the owner or admins (the owner may remove an admin). Revoking an invitation is `state: removed` on an invited row. `expected_revision` is the member row's revision. |
| DELETE | `/v1/groups/{id}/bans/{address}` | | Admins. Lifts a ban. |
| POST | `/v1/groups/{id}/invitation` | `{accept}` | The invited mailbox. Accept → `keying` (authority rechecked); decline removes the invitation. |
| POST | `/v1/groups/{id}/join` | `{link_token?}` | Public groups: listed, or with the current link token. → `keying`. Bans and size apply. |
| POST | `/v1/groups/{id}/leave` | | The owner must transfer first unless they are the last member (then the group is deleted). |
| POST | `/v1/groups/{id}/transfer` | `{to, expected_revision}` | Owner → an active admin. |
| POST | `/v1/groups/{id}/link` | `{enabled}` | Admins. `{link_token, url}` once (`https://inbox.pigeonpost.dev/g/<id>#<token>`), or `{link_token:null}`. A new token invalidates the old one. Only for `join_policy=public`. |
| POST | `/v1/groups/resolve` | `{group_id, link_token?}` | Preview `{id, name, description, member_count, join_policy, visibility, my_state, can_join, needs}` for a listed public group, a valid link, an invitation or existing membership; otherwise `404 not_found`. The token travels in the body, never a URL. |
| GET | `/v1/rooms` | `?q=` | Listed public groups (`org_id` rooms join this in Phase 4). |
| POST | `/v1/groups/{id}/read` | `{seq}` | Forward only. `group.read` to the account's other devices. |
| GET | `/v1/groups/{id}/receipts` | | Feature `receipts`. Members' `delivered_seq` (advanced when their client lists messages) and `seen_seq` (read cursor, `null` when either side turned read receipts off), for ticks. See `docs/message-actions.md` §5. |
| PUT | `/v1/groups/{id}/notifications` | `{notify, muted_until}` | `muted_until`: unix seconds, `null` = not muted. |

### Membership lifecycle

```
invite ──► invited ──accept──► keying ──commit adds a device──► active
public join ─────────────────► keying
active ──leave──► left        active ──admin──► removed | banned
```

A `keying` member waits for any online member device to add one of its devices (§3). Server access
for `left/removed/banned` stops at once; their leaves become **pending removals** and application
posts are refused with `409 key_activation_pending` until an authorized device commits the removal.

## 3. Messages and handshakes

| Method | Path | Body / query | Result and rules |
|---|---|---|---|
| GET | `/v1/groups/{id}/messages` **D** | `?after_seq=&before_seq=&limit≤100` | `{items, last_seq, epoch, more}`. Active members only, within the current membership interval. `410 resync_required` when `after_seq` is older than the retained handshakes. |
| POST | `/v1/groups/{id}/messages` **D K** | `{request_key, client_message_id, epoch, enc_scheme, ciphertext, envelope_signature, attachments?:[att ids], reply_to?: seq, edit_of?: seq, mentions?:[address]}` | `201 {seq, id, expires_at}`. Checks: active member, posting policy (`posting_restricted`), not restricted, device registered, envelope signature, envelope header (application, this group, this mailbox/device, `client_message_id`), `epoch == group epoch` (`409 epoch_conflict {current_epoch}`), no pending removal, `enc_scheme = mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1`, ciphertext ≤ 1.2 MB, group quota (100 MB, 1 GB when the owner is paid), 120 posts/min per mailbox. `edit_of` (own message, ≤ 24 h) replaces the old item with a tombstone; the new item carries the full new text. `(sender, client_message_id)` is unique: a retry returns the original. |
| DELETE | `/v1/groups/{id}/messages/{seq}` | | Sender (≤ 24 h) or admin. Tombstone (`deleted_at`), attachments released. |
| POST | `/v1/groups/{id}/attachments` **D** | raw sealed bytes (`application/octet-stream`) | `201 {id, bytes, sha256}`. Counts against the group quota. Claimed by the next post that names it; unclaimed uploads expire after 24 h. |
| GET | `/v1/groups/{id}/attachments/{att_id}` | | Sealed bytes, only while the referencing message is visible to the caller and unexpired. Decrypt with `openAttachment(sealed, reference)` from the message body. |
| POST | `/v1/groups/{id}/key-packages/claim` **D K** | `{request_key, mailboxes:[address]}` | `{claims:[{mailbox, device_id, key_package, key_package_ref}], exhausted:[{mailbox, device_id}]}` — one package per pending-add device of each mailbox. Any active member device may claim for pending adds. Claims expire after 10 minutes. |
| POST | `/v1/groups/{id}/commits` **D K** | `{request_key, epoch, commit_id, commit, envelope_signature, welcome?, adds:[{mailbox, device_id, key_package_ref}], removes:[{mailbox, device_id}], expected_revision}` | `201 {seq, epoch, revision}` (epoch is the new one). P-2 §7.3: first valid commit for an epoch wins; anything else is `409 revision_conflict {current_epoch, revision}`. `adds` must be pending adds claimed by this device; `removes` must be pending removals or the committer's own mailbox's other devices; `welcome_recipients(welcome)` must equal the adds' refs. Self-updates (no adds/removes) are always allowed. The creator's first commit activates the group. |
| POST | `/v1/groups/{id}/epoch-ack` **D** | `{epoch}` | Monotonic per device. |
| GET | `/v1/groups/{id}/crypto` **D** | | `{epoch, revision, activation_state, in_group, pending:[{kind: add\|remove, mailbox, device_id, since}], lagging_devices:[{mailbox, device_id, acked_epoch}]}` |
| POST | `/v1/groups/{id}/resync` **D K** | `{request_key}` | This device lost its state: its leaf becomes a pending removal, then a pending add. |

Items in `GET …/messages` (one per-group sequence, process strictly in order):

```json
{"seq": 7, "type": "application", "id": "gm_…", "epoch": 2, "sender": "/k/…", "sender_device": "web-…",
 "client_message_id": "…", "ciphertext": "b64", "attachments": [{"id": "gat_…", "bytes": 1234}],
 "reply_to": null, "edit_of": null, "created_at": 0, "expires_at": null, "deleted_at": null}
{"seq": 8, "type": "commit", "epoch": 2, "sender": "/k/…", "sender_device": "…", "commit_id": "…",
 "commit": "b64", "adds": [{"mailbox","device_id"}], "removes": [], "created_at": 0}
{"seq": 9, "type": "notice", "kind": "member_joined", "actor": "/k/…", "metadata": {…}, "created_at": 0}
```

A deleted or edited item keeps its `seq` with `ciphertext: null` and `deleted_at` set (or
`replaced_by: seq`). Clients render notices from the **verified commit outcome** where one exists
(P-2 §9) and treat server notices as a cross-check: a disagreement is shown as a warning.

Notice kinds: `group_created`, `member_invited`, `member_joined`, `member_left`, `member_removed`,
`member_banned`, `role_changed`, `settings_changed`, `ownership_transferred`, `device_added`,
`device_removed`, `timer_changed`, `call_started`, `call_ended`, `link_reset`.

### Client loop (what every native client must do)

1. On start: register the device once (§1), keep ≥ 10 key packages plus one last-resort package
   uploaded, persist the encrypted state blob after **every** mutating call.
2. Fetch `GET /v1/mls/welcomes`; join each; ack; self-update after a last-resort join.
3. For each active group: `GET …/messages?after_seq=<cursor>`; `processIncoming` each commit/
   application item in `seq` order; render notices; `epoch-ack` after commits.
4. `GET …/crypto`; when `pending` is non-empty, wait 0–3 s (jitter), then claim, build one commit
   with all adds and removes, post it. On `409 revision_conflict`: `discardCommit`, catch up, retry
   with backoff (200 ms base, 10 s cap). On `409 epoch_conflict` for an application post: catch up
   and re-encrypt under the same `client_message_id`.
5. Show device changes (`group.device`, commit outcomes): "Jane added a new device" with the
   fingerprint, and the group safety number in group info. The postbox can attest devices for
   hosted mailboxes; visibility is the defence (P-2 §3, §12).

## 4. Events (`GET /v1/events?typed=1`)

Typed events are delivered when the `account_events` flag is on **or** the client asks with
`typed=1`. Ids look like `"<mail>.<n>"`; resume with `Last-Event-ID`. Events are wake-ups: refetch
authoritative state over REST. Membership is rechecked at delivery, so a removed mailbox stops
receiving `group.message`/`group.handshake` at once.

| Type | Data |
|---|---|
| `group.message` | `{group_id, seq, sender}` (also for deletions/edits: `{…, deleted: true}`) |
| `group.handshake` | `{group_id, seq, epoch}` |
| `group.member` | `{group_id, address, role, state, change}` |
| `group.settings` | `{group_id, changed:[…], revision}` |
| `group.deleted` | `{group_id}` |
| `group.read` | `{group_id, seq}` (own account only) |
| `group.crypto_pending` | `{group_id, kinds:[add\|remove]}` |
| `group.device` | `{mailbox, device_id, fingerprint, change: added\|revoked}` |
| `mls.welcome` | `{welcome_id, group_id, device_id}` |
| `mls.key_packages_low` | `{device_id, unclaimed}` |
| `message.expired` | `{thread_id, ids}` or `{group_id, seqs}` |
| `thread.settings` | `{thread_id, ephemeral_secs, changed_by}` |

## 5. Disappearing messages (SDS §17)

Allowed timers: `null` (off), 300, 3600, 28800, 86400, 604800, 2419200 seconds.

* Direct threads: `PATCH /v1/threads/{id} {ephemeral_secs}` — either participant; both sides of the
  thread share the value; `thread.settings` goes to both. `GET /v1/threads` carries
  `ephemeral_secs`; `/v1/inbox` messages carry `expires_at`.
* Groups: `PATCH /v1/groups/{id} {ephemeral_secs, expected_revision}`; admins, or members when
  `members_can_change_timer`. Items carry `expires_at`.
* The clock starts at send (D-25); changing a timer affects only new messages.
* The postbox deletes expired rows every 30 s, releases attachments and emits `message.expired`.
  Reads never return expired content. Clients must also drop expired items locally by their own
  timer, before rendering, even when they missed the event.
* Push for a disappearing conversation never carries content ("New message").
* Stated limits (show in help text): recipients can screenshot or copy; notifications already shown
  may stay; copies made outside Pigeonpost are not affected.

## 6. Group calls (SDS §4.7)

Calls service (`/calls/v1`, same origin as the inbox):

| Method | Path | Body | Result |
|---|---|---|---|
| POST | `/calls/v1/calls` | `{session, kind, from, group_id}` | Rings every active member's sessions (each filtered by their call settings and blocks). `room.group_id` is set. |
| GET | `/calls/v1/groups/{group_id}/call?from=` | | `{active, room?, kind?, participants?}` for "Call in progress — Join". |
| POST | `/calls/v1/groups/{group_id}/join` | `{session, from}` | Any active member joins without knocking. |

Membership is rechecked by the postbox (`POST /internal/v1/group-call-admission`, calls-service
token) at create, ring and join. Calls keep the six-person mesh cap; meeting-link guests stay
separate and never gain group membership. Start and end appear in the group as `call_started` /
`call_ended` notices.

## 7. Push

Group messages notify eligible active members except the sender, muted members, `notify=none`
(and `mentions` unless mentioned), and members who blocked the sender. The body is generic:
title = group name, body = "New message". The server cannot read group content. `mentions` are
sender-supplied metadata used only for this routing.
