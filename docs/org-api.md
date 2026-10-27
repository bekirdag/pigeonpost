# Enterprise API — contract (Phase 5)

SDS: `docs/planning/2026-10-20-big-features-sds.md` §18 (API, keys, conventions, OpenAPI, webhooks,
docs site), §3.2–§3.4 (principals, `authorize`, step-up), §6 (org model). The machine-readable
source of truth is `openapi/pigeonpost-org.v1.yaml`; this file explains the decisions behind it.
The org model itself is `docs/orgs.md`.

Everything here is behind the feature flag **`org_api`** (global, or on the org owner's account).
With the flag off, `/org/v1/*` answers `404 feature_disabled` and the key and webhook routes on the
dashboard API do too.

## 1. Who can do what

An **org API key** (`pk_org_` + 32 characters) is an automation credential for one organization.
It is not a person, and it is not a mailbox.

| A key can | A key can never |
|---|---|
| Read the org, members, roles, agents, delegations, rules, rooms, usage, billing status | Read any message, group ciphertext, attachment or key material (D-04) |
| Invite, rename, suspend, reactivate and remove **ordinary members** | Touch the owner or any member holding an admin role, or grant any role |
| Create and delete agents, disconnect their devices, revoke unredeemed enrollment codes | Issue enrollment codes or device credentials, approve device logins, open SSH |
| Create, update and delete **restrictive** org rules (block, review) | Create or widen a rule that raises trust (allow, known, auto, verbs, locked) |
| Create rooms, change listing/auto-join, invite org mailboxes, delete rooms | Add anyone to a room's encryption group; only members' devices do that |
| Send direct messages **as the one org agent mailbox chosen when the key was made** | Send as any other mailbox, or post into an end-to-end-encrypted room |
| Read the org audit log, manage webhooks | Create, list or revoke other API keys; perform any step-up action |

These limits are enforced in `policy.rs` (`Action::OrgApi`, `Actor::OrgKey`) as an intersection:

1. **Credential kind.** Only `Actor::OrgKey` reaches `/org/v1`. Human-only and step-up actions refuse
   it with `human_required`. Keys do not work on `/v1` (they are not account or mailbox credentials)
   and `/v1` credentials do not work on `/org/v1`.
2. **Mandatory denies.** Revoked, expired, wrong IP, org not live (`org_inactive`; read-only orgs
   refuse writes with `org_read_only`).
3. **Grant.** The key's **scope** for the route *and* the **live authority of the person who made
   it**: the creator must still be an active owner or admin of the org and their current roles must
   permit the action (`org::permits`). Demote or remove the creator and every key they made stops
   working at once; promote them again and it works again only if it was never revoked.
4. **Action limits.** On top of the grant, each write applies the bounds in the table above
   (target rank 0 for member writes, restrictive rules only, same-org mailboxes only).

### Scopes

| Scope | Allows | Creator must hold (§6.4) |
|---|---|---|
| `org:read` | Every `GET` except `/audit`; `POST /rules/evaluate` | owner/admin (all reads) |
| `members:write` | Member invite, rename, suspend, reactivate, remove, resend invitation | `ManagePeople` |
| `agents:write` | Agent create/delete/disconnect, revoke enrollment codes | `ManageAgents` |
| `rules:write` | Restrictive rule create/update/delete | `EditMemberRules`/`EditAgentRules` per rule owners |
| `rooms:write` | Room create/update/invite/delete | `CreateRoom` + `DeleteAnyRoom` |
| `messages:send` | `POST /messages` as the key's `send_as` agent mailbox | `ManageAgents` |
| `audit:read` | `GET /audit` | `Security` |
| `webhooks:write` | Webhook create/delete/rotate | `Security` |

`GET /key` and `DELETE /key` (self-revocation) need no scope.

### Keys (dashboard API, a person signed in)

| Method | Path | Notes |
|---|---|---|
| GET | `/v1/orgs/{id}/api-keys` | Owner/admin. `{keys:[ApiKey]}`; never the secret. |
| POST | `/v1/orgs/{id}/api-keys` | Owner/admin **with step-up** (`401 step_up_required`, or `403 step_up_unavailable` when the server has no strong-auth `acr` configured — never bypassed). Body `{name, scopes:[…], expires_in_days?: 1..730, ip_allowlist?: ["203.0.113.0/24", …≤20], send_as?: "/acme/ci-bot"}`. `send_as` is required with `messages:send` and must be an active org **agent**. `201 {key: ApiKey, secret: "pk_org_…"}`, `Cache-Control: no-store`; the secret is shown once and stored only as SHA-256. At most 25 live keys per org. |
| DELETE | `/v1/orgs/{id}/api-keys/{key_id}` | Owner/admin. Revokes immediately. |

`ApiKey`: `{id: "okey_…", name, scopes, last4, send_as, ip_allowlist, created_at, created_by
(member handle), expires_at, last_used_at, revoked_at}` (Unix seconds on the dashboard API, like the
rest of `/v1/orgs`).

Every key creation and revocation is audited (`api_key.create`, `api_key.revoke`) with the key id as
target. Every authenticated `/org/v1` request is audited as `api_key.use` (`actor_kind = api_key`,
actor = the key id, details `{method, route, status}`), and every change it makes carries the same
actor on its own record.

## 2. Conventions (`/org/v1`)

- Base URL `https://postbox.pigeonpost.dev/org/v1` (D-26 alias `api.pigeonpost.dev` once DNS exists).
  `Authorization: Bearer pk_org_…`.
- JSON, `application/json; charset=utf-8`, snake_case. Timestamps are ISO 8601 UTC
  (`2026-10-27T10:15:00Z`).
- Every success is `{"data": …}`; lists are `{"data": […], "next_cursor": "…" | null}` with
  `?limit=1..200` (default 50) and `?cursor=` (opaque; pass back exactly).
- Errors are `{"error": {"code", "message", "doc_url"}}`. Codes are stable; see §5.
- `Idempotency-Key: <1-255 printable ASCII>` on any `POST`, `PATCH` or `DELETE`. Scoped to the key
  and route; the request hash covers method, path and body. A replay within 24 h returns the
  recorded status and body with `Idempotent-Replayed: true`; the same key with a different request is
  `409 idempotency_conflict`. Only successful responses are recorded.
- Rate limits per key: 600 requests/minute, of which 60 writes/minute. Every response carries
  `RateLimit-Limit`, `RateLimit-Remaining` and `RateLimit-Reset` (seconds) for the bucket that
  applied; `429 rate_limited` adds `Retry-After`.
- Versioning: breaking changes go to `/org/v2`; v1 stays 12 months after a deprecation notice.

## 3. Endpoints

`{member}` is a member id (`mem_…`) or the member's name inside the org (`john` for `/acme/john`).
`{agent}` is the agent's name (`ci-bot`) or its mailbox address. `{room}` is a room id.

| Method | Path | Scope | Notes |
|---|---|---|---|
| GET | `/key` | — | This key: id, name, org, scopes, expiry, allowlist, `send_as`. |
| DELETE | `/key` | — | Revoke this key. |
| GET | `/org` | org:read | Name, namespace, status, seats/agents used and limits, plan, settings. |
| GET | `/members` | org:read | `?status=invited|active|suspended`. |
| POST | `/members` | members:write | Invite one ordinary member `{first_name, last_name, contact, handle}` → `201 {data: {member, invitation: {id, link, expires_at, delivery}}}`, `no-store`. Seats, verified domains and handle rules as in `docs/orgs.md`. No `roles` field. |
| GET | `/members/{member}` | org:read | |
| PATCH | `/members/{member}` | members:write | `{first_name?, last_name?}`. |
| DELETE | `/members/{member}` | members:write | `?mode=reserve` (default: the address is held for a successor) or `delete`. Offboarding as §6.7. |
| POST | `/members/{member}/suspend` | members:write | |
| POST | `/members/{member}/reactivate` | members:write | |
| POST | `/members/{member}/resend-invite` | members:write | New link, old one stops working; `no-store`. |
| GET | `/roles` | org:read | Role definitions and who holds each. Role writes stay on the dashboard (owner step-up). |
| GET | `/agents` | org:read | |
| POST | `/agents` | agents:write | `{handle, label?}` → `201`. `409 agent_limit` at 10 × seats. |
| GET | `/agents/{agent}` | org:read | |
| DELETE | `/agents/{agent}` | agents:write | Deletes; every device credential revoked. |
| POST | `/agents/{agent}/disconnect` | agents:write | Revokes every device credential and unredeemed code; the agent stays. |
| GET | `/agents/{agent}/delegations` | org:read | Delegation writes are human-only. |
| GET | `/agents/{agent}/enrollments` | org:read | Metadata of codes people issued (never the code). |
| DELETE | `/agents/{agent}/enrollments/{enrollment}` | agents:write | Revoke an unredeemed code. |
| GET | `/rules` | org:read | |
| POST | `/rules` | rules:write | Restrictive only: `admission` `block`/null, `autonomy` `review`/null, no `verbs`, `known` false, `locked` false. Anything else is `403 human_required`. |
| PATCH | `/rules/{rule}` | rules:write | Only rules an API key created; the result must stay restrictive. |
| DELETE | `/rules/{rule}` | rules:write | Rules an API key created, or rules that grant trust (removing a grant lowers trust). Deleting a person's restriction is `human_required`. |
| POST | `/rules/evaluate` | org:read | `{from, to, verb?}` dry run, as the dashboard. |
| GET | `/rooms` | org:read | Every org room (metadata only). |
| POST | `/rooms` | rooms:write | `{name, owner, description?, join_policy?, post_policy?, listed?, auto_join?}`. `owner` is an active org member or agent mailbox whose device activates the room's encryption; the room is `provisioning` until then. |
| PATCH | `/rooms/{room}` | rooms:write | `{listed?, auto_join?}`. |
| POST | `/rooms/{room}/members` | rooms:write | `{handle}` of a live org mailbox → a room **invitation** the person accepts in their app; a room member's device then adds them to the encryption group. Agents cannot accept invitations (human-only); they join public rooms. |
| DELETE | `/rooms/{room}` | rooms:write | |
| GET | `/audit` | audit:read | Oldest first, `?since=<ISO>&action=<prefix>&limit&cursor`. For SIEM streaming. |
| GET | `/usage` | org:read | Seats, agents, rooms, storage used, call minutes (30 days). |
| GET | `/billing` | org:read | Plan and status; creator must be owner/admin (§6.4 view). Changes stay on the dashboard. |
| GET | `/webhooks` | org:read | |
| POST | `/webhooks` | webhooks:write | `{url, events:[…], description?}` → `201 {data: {webhook, secret}}`, `no-store`. |
| DELETE | `/webhooks/{webhook}` | webhooks:write | |
| POST | `/webhooks/{webhook}/rotate` | webhooks:write | New secret; the old one keeps signing for 24 h. |
| GET | `/webhooks/{webhook}/deliveries` | org:read | Latest attempts. |
| POST | `/messages` | messages:send | `{to, body, thread_title?}` → `201`. See §4. |

The dashboard API has the same webhook routes for people (owner/admin, no step-up; Unix seconds):

| Method | Path | Result |
|---|---|---|
| GET | `/v1/orgs/{id}/webhooks` | `{webhooks:[Webhook]}` |
| POST | `/v1/orgs/{id}/webhooks` | `{url, events, description?}` → `201 {webhook: Webhook, secret: "whsec_…"}`, `no-store` |
| DELETE | `/v1/orgs/{id}/webhooks/{webhook}` | `{deleted: true}`; pending deliveries are cancelled |
| POST | `/v1/orgs/{id}/webhooks/{webhook}/rotate` | `{webhook, secret, previous_secret_expires_at}`, `no-store` |
| GET | `/v1/orgs/{id}/webhooks/{webhook}/deliveries` | `{deliveries:[Delivery]}` (newest first, `?limit≤100`) |

`Webhook`: `{id: "wh_…", url, events, description, status: "active", created_at, created_by,
rotated_at, previous_secret_expires_at, last_delivery: {at, status, response_status} | null}`.
`Delivery`: `{id: "whd_…", event_id, event_type, attempt, status: "delivered"|"failed"|"retrying"|
"blocked", response_status, error, duration_ms, at}`.

## 4. Sending messages, and why rooms are not included

Org rooms are end-to-end encrypted with MLS (`docs/group-e2ee.md`): the postbox stores ciphertext
and holds no group keys. For an API to post into a room, something on the server would have to be a
member of the room's encryption group — that is, the postbox would hold group keys. That would
make every room's content readable by the server, which is exactly what §3.6 rules out
("server-custodied group keys are not an equivalent substitute"). Running an MLS client for a
service mailbox inside the postbox is the same thing with extra steps.

So `messages:send` is limited to **direct messages** from the key's `send_as` org agent mailbox to
**mailboxes of the same org** (members and agents). Those messages take the normal hosted direct
path (`do_send`: personal blocks, org rules, inbox policy, filters, quotas), which is custodial
exactly as every hosted direct message already is. A target that is a room, a group or a mailbox
outside the org is refused (`409 room_encrypted`, `403 recipient_not_in_org`).

To post into a room from automation, enroll an org agent on the machine with an enrollment code a
person issued (`pigeonpost login --enroll-stdin`), then have it join a public org room
(`pigeonpost group join <grp_id>`; accepting an invitation is a person's decision, so agents join
rather than accept). A room member's device adds it to the encryption group, and the agent's own
device then holds its MLS keys and posts with the normal `/v1/groups` API (`pigeonpost group send`).
The guide "Post build results into a room" walks through it.

## 5. Errors

| HTTP | Code | When |
|---|---|---|
| 400 | `bad_request`, `invalid_handle`, `invalid_contact`, `invalid_rule`, `invalid_cursor`, `invalid_webhook_url` | Malformed input. |
| 401 | `unauthorized` | No key, unknown key, revoked key or expired key (one answer). |
| 403 | `ip_not_allowed` | The caller's address is outside the key's allowlist. |
| 403 | `insufficient_scope` | The key lacks the route's scope. |
| 403 | `forbidden` | The creator's current role no longer allows it, or the target is out of the key's reach (owner/admins). |
| 403 | `human_required` | Only a person can do this (role grants, trust-raising rules, enrollment codes, …). |
| 403 | `recipient_not_in_org`, `not_admitted` | `POST /messages`. |
| 404 | `not_found`, `feature_disabled` | |
| 409 | `seat_limit`, `agent_limit`, `handle_taken`, `already_invited`, `domain_not_verified`, `org_read_only`, `org_inactive`, `idempotency_conflict`, `room_encrypted`, `invite_used`, `webhook_limit`, `too_many_rules` | |
| 413 | `payload_too_large` | Message body over 64 KB. |
| 429 | `rate_limited` | With `Retry-After`. |
| 500 | `store_error` | |

`doc_url` is `https://developers.pigeonpost.dev/org-api-errors#<code>`.

## 6. Webhooks (§18.5)

Events (subscribe with a list, or `["*"]`):

| Event | Data (ids and handles only, never content) |
|---|---|
| `member.invited`, `member.joined`, `member.suspended`, `member.removed` | `member_id`, `handle` |
| `agent.created` | `agent` (address), `handle` |
| `agent.connected`, `agent.disconnected` | `agent`, `device_id` |
| `room.created`, `room.deleted` | `room_id` |
| `billing.status_changed` | `status`, `previous_status` |
| `meeting.brief_ready`, `security.ssh_session` | Reserved for Phases 7 and 8; not emitted yet. |

Events are recorded by database triggers in the same transaction as the change, from every path
(dashboard, API, billing bridge, lifecycle worker), and only for orgs that have a webhook.

Delivery: `POST <url>` with body

```json
{"id":"evt_…","type":"member.joined","created_at":"2026-10-27T10:15:00Z","org_id":"org_…","data":{"member_id":"mem_…","handle":"/acme/john"}}
```

and headers `Content-Type: application/json`, `User-Agent: Pigeonpost-Webhooks/1`,
`Pigeonpost-Event-Id`, `Pigeonpost-Event-Type`, `Pigeonpost-Delivery-Id` (per attempt) and

```
Pigeonpost-Signature: t=1793095200,v1=<hex HMAC-SHA256(secret, "1793095200" + "." + raw body)>
```

During the 24 h after a rotation there are two `v1=` entries (new secret first). Verify with a
constant-time comparison, reject timestamps more than 5 minutes from your clock, and deduplicate on
`Pigeonpost-Event-Id` — delivery is at-least-once.

A 2xx answer within 10 s is success. Anything else (including any 3xx: redirects are never
followed) is retried with exponential backoff — 30 s, 2 min, 10 min, 30 min, 1 h, then every 2 h —
until 24 h after the event, then marked failed. The outbox is in the database, so restarts lose
nothing; an attempt in flight when the server stops is retried after its 60 s lease.

**Egress safety.** A webhook URL must be `https://` on port 443 or 8443, with no credentials or
fragment, at most 2,048 characters, and a host that is not `localhost`/`*.local`/`*.internal`. Its
name is resolved at registration and again at every delivery; if **any** resolved address is
loopback, private (RFC 1918, ULA), link-local (including 169.254.169.254), CGNAT, multicast,
documentation, benchmarking, reserved, unspecified, or an IPv4-mapped/NAT64/6to4 form of one, the
URL is refused or the attempt is marked `blocked`. The connection is pinned to the address that was
checked (so a DNS answer cannot change between the check and the connect), uses certificate
verification for the URL's host name, no proxy, no redirects, a 5 s connect and 10 s total timeout,
and reads at most 4 KB of the response. At most 10 webhooks per org; signing secrets are sealed at
rest with the postbox vault and are never logged.

## 7. Security self-review (SDS §23, for this phase)

| Threat | Mitigation | Residual |
|---|---|---|
| Key theft | SHA-256 at rest, shown once, optional expiry and IP allowlist, revocation, self-revocation, per-use audit with key id | A stolen key works until revoked or expired; the allowlist is only as good as `TRUSTED_PROXY_HOPS`. |
| Privilege escalation | Scope ∧ creator's live role ∧ action bounds in one `authorize`; no role, key, enrollment or device-credential issuance; restrictive-only rules | — |
| Content access | No route reads messages, ciphertext, attachments or keys; rooms cannot be posted to | Hosted direct messages remain custodial (unchanged). |
| SSRF via webhooks | §6 egress rules, re-resolved per delivery, pinned connection, no redirects | Webhook administration reveals org metadata to the configured endpoint by design. |
| Replay / duplication | `Idempotency-Key` with request hash; webhook event ids | Idempotency records live 24 h. |
| Denial of service | Per-key request and write limits, 10 webhooks per org, bounded outbox, bounded response reads | In-memory rate limiter resets on restart. |
