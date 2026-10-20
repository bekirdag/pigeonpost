# Organizations — client contract (Phase 4, enterprise beta)

SDS: `docs/planning/2026-10-20-big-features-sds.md` §4 (org rooms), §5 (portal), §6 (org model),
§7 (invitations), §9 (rules), §10 (device login). This file is what the enterprise portal, the web
inbox, the CLI and the native apps build against. The postbox is the only source of truth.

Every `/v1/orgs…` route is behind the feature flag **`orgs`** (`404 feature_disabled` otherwise).
The portal additionally checks **`enterprise_portal`** in `GET /v1/capabilities` before showing
`/start` and `/app`. Invitation messages are sent only when **`org_invite_email`** is on; until
then the admin copies the link that the create/resend response returns once.

Base URL: `https://postbox.pigeonpost.dev`. Bearer: a member JWT (`pigeonpost-web`, scope
`openid email profile` for invitation acceptance), unless a route says otherwise. CORS admits
`https://enterprise.pigeonpost.dev` and `https://inbox.pigeonpost.dev`.

Errors are `{"error": code, "detail": text, …extra}`. Codes used here: `feature_disabled`,
`not_org_member`, `forbidden`, `human_required`, `step_up_required` (401 + RFC 9470
`WWW-Authenticate` with `acr_values` and `max_age=600`), `step_up_unavailable`, `mfa_required`,
`org_inactive`, `org_read_only`, `seat_limit` (409), `agent_limit` (409), `seats_in_use`,
`handle_taken`, `invalid_handle`, `domain_not_verified`, `delegation_required`, `invite_invalid`,
`invite_address_mismatch`, `pairing_not_found`, `pairing_expired`, `authorization_pending`,
`enrollment_invalid`, `rate_limited` (429 + `Retry-After`), `revision_conflict`.

## 1. Model

| Thing | Owner | Notes |
|---|---|---|
| Org namespace `/acme` | the org's internal (non-login) account | Nobody can mint under it through the personal handle routes. |
| Member mailbox `/acme/john` | the member's **personal** account | Appears in their normal apps. The org keeps administrative control (suspend, remove, reassign). |
| Agent mailbox `/acme/agent1` | the org account | Operated through delegations and enrolled device credentials, never owned by a person. |
| Room | a group with `org_id` | E2EE like every group (`docs/groups.md`). Listed rooms appear in the org directory. |

**Roles** (a member may hold several): `owner` (exactly one), `admin`, `people_admin`,
`agent_admin`, `billing_admin`. Everyone with an active membership is a member.

**Seats**: owner + active + suspended members + live invitations. **Agents**: active + suspended
org agents, limit `10 × seats`. Over the limit: `409 seat_limit` / `409 agent_limit`.

**Lifecycle** (`org.status`), driven by the org's entitlement:
`active` → payment lapsed: `past_due` (7 days, banner only) → `read_only` (14 days: no invites, no
new agents, org mailboxes cannot send, rooms are read-only) → `suspended` (org mailboxes do not
receive — senders see "address not found" — and cannot be used). `pending` and
`paid_unprovisioned` orgs have no members yet.

### Permission matrix (§6.4)

| Action | owner | admin | people_admin | agent_admin | billing_admin | member |
|---|---|---|---|---|---|---|
| Invite / suspend / remove members, edit names | ✓ | ✓ | ✓ (not admins/owner) | | | |
| Grant `admin` | ✓ (step-up) | | | | | |
| Grant `people_admin`, `agent_admin`, `billing_admin` | ✓ | ✓ | | | | |
| Create / delete / suspend agents, enrollment codes, delegations | ✓ | ✓ | | ✓ | | |
| Approve a device login for an agent | ✓ | ✓ | | ✓ | | delegation `enroll` |
| Org rules | ✓ | ✓ | members' rules | agents' rules | | |
| Rooms: create | ✓ | ✓ | ✓ | ✓ | ✓ | per `rooms_creation` |
| Rooms: delete any / change auto-join | ✓ | ✓ | ✓ | | | own rooms |
| Billing view | ✓ | view | | | ✓ | |
| Security settings, domains, audit log | ✓ | ✓ | | | | |
| Audit export, org delete, ownership transfer | ✓ (step-up) | export (step-up) | | | | |
| Read a member's messages | never | never | never | never | never | own only |

Agents' credentials (mailbox tokens, device credentials, API keys) can never perform any of these
actions: they are human-only (`403 human_required`).

## 2. Organizations

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/v1/orgs` | | `{orgs:[OrgSummary], operated:[{address, handle, org_id, capabilities}]}` — orgs the caller is an active member of, and agents delegated to them with `read_inbox`. |
| GET | `/v1/orgs/{id}` | | `{org: Org, me: {member_id, roles, mailbox}}` |
| PATCH | `/v1/orgs/{id}` | `{name?, settings?: {require_mfa?, invite_domains_only?, rooms_creation?: "everyone"\|"admins"}, expected_revision?}` | `{org}`. Owner/admin. |
| DELETE | `/v1/orgs/{id}` | `{confirm_namespace}` | `{deleted: true}`. Owner, step-up. Suspends mail, removes member mailboxes from their accounts, revokes agent credentials; the namespace enters the 30-day recovery window. |
| POST | `/v1/orgs/{id}/transfer` | `{member_id}` | Owner → an active admin. Step-up. |
| POST | `/v1/orgs/{id}/me/mailbox` | `{handle}` | `201 {mailbox:{address, handle}}` — the owner picks their own org address once (`/acme/<name>`); the org's `General` room is created with them as its keying owner. |
| GET | `/v1/orgs/{id}/directory` | `?q=&limit≤50` | `{entries:[{handle, address, kind: member\|agent, name?}]}`. Active members only. |
| GET | `/v1/orgs/{id}/billing` | | `{plan_code, seats, seats_used, agent_limit, agents_used, status, period_end, paid_through, storage_bytes}`. Owner/admin/billing admin. Payment changes go through the billing bridge. |

`Org`: `{id, name, namespace, status, seats, seats_used, agent_limit, agents_used, plan_code,
period_end, settings:{require_mfa, invite_domains_only, rooms_creation}, created_at, revision}`.
`OrgSummary`: `Org` plus `{my_roles, my_mailbox}`.

## 3. People

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/v1/orgs/{id}/members` | `?status=` | `{members:[Member]}` |
| POST | `/v1/orgs/{id}/invites` | `{invites:[{first_name, last_name, contact, handle, roles?:[]}]}` (≤ 200) | `{results:[{status: "invited"\|<error code>, member_id?, invite_id?, handle, link?, expires_at?}]}`. `link` = `https://enterprise.pigeonpost.dev/invite#<token>` and is **shown once**. |
| POST | `/v1/orgs/{id}/invites/{invite_id}/resend` | | New token (the old one stops working), `{link, expires_at}`. Rechecks seats, role authority and the handle. |
| DELETE | `/v1/orgs/{id}/invites/{invite_id}` | | Revoked at once; seat and handle released. |
| PATCH | `/v1/orgs/{id}/members/{member_id}` | `{first_name?, last_name?, roles?:[…], status?: "active"\|"suspended"}` | `{member}`. Granting/removing `admin` needs the owner with step-up. |
| POST | `/v1/orgs/{id}/members/{member_id}/remove` | `{mode: "reserve"\|"delete"}` | Offboarding (§6.7, D-15). `reserve` keeps the address for a new person (invite to the same handle); history stays with the old, now detached mailbox. `delete` releases the address. Either way: memberships, roles, delegations, rooms and derived device credentials are revoked. |

`Member`: `{id, first_name, last_name, handle, address, roles, status: invited|active|suspended,
contact_masked, invited_at, joined_at, last_active_at, invite?: {id, expires_at, state:
pending|expired}}`.

`contact` is the address the invitation goes to. It is matched case-insensitively after trimming;
no other normalization (dots and `+tags` are significant). With `invite_domains_only` on, its domain
must be a verified org domain (`403 domain_not_verified`).

### Invitation acceptance (no org flag check on preview beyond the global `orgs` flag)

| Method | Path | Body | Result |
|---|---|---|---|
| POST | `/v1/org-invites/preview` | `{token}` (no auth) | `{org:{name, namespace}, handle, role_names, inviter, contact_masked, expires_at, rooms:[name], requires_mfa}`; any problem → `404 invite_invalid` (one answer for wrong, expired, used, revoked). |
| POST | `/v1/org-invites/accept` | `{token}` (member JWT, a person) | `{org, member, mailbox:{address, handle}}`. The token's verified `email` claim must equal the invited address (`email_verified` true) → else `403 invite_address_mismatch {contact_masked}`. If the org requires MFA: `401 step_up_required`. A retry by the same account returns the same result. |

The token lives in the URL fragment. Pages read it, `history.replaceState` it away, keep it only in
`sessionStorage` across sign-in, and send it only in POST bodies.

## 4. Agents and delegations

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/v1/orgs/{id}/agents` | | `{agents:[Agent]}` |
| POST | `/v1/orgs/{id}/agents` | `{handle, label?, operators?: [member_id]}` | `201 {agent}`. `operators` get an `enroll`+`bind_handle` delegation. |
| PATCH | `/v1/orgs/{id}/agents/{address}` | `{label?, status?: active\|suspended}` | `{agent}` |
| DELETE | `/v1/orgs/{id}/agents/{address}` | | Deleted; every device credential revoked. |
| POST | `/v1/orgs/{id}/agents/{address}/enrollments` | `{ttl_hours?: 1..24}` | `201 {code: "enr_…", expires_at}`, shown once. Step-up. |
| GET | `/v1/orgs/{id}/agents/{address}/devices` | | `{devices:[{id, label, created_at, expires_at, last_used_at, revoked_at, source: pairing\|enrollment}]}` |
| DELETE | `/v1/orgs/{id}/devices/{device_id}` | | Revokes one device credential. |
| GET | `/v1/orgs/{id}/delegations` | | `{delegations:[{id, member_id, member_handle, target, capabilities, expires_at, granted_by, granted_at}]}` |
| POST | `/v1/orgs/{id}/delegations` | `{member_id, target: "/acme/agent1" \| "/acme/build-*", capabilities:[…], expires_at?}` | `201 {delegations:[…]}` — a pattern is materialized to every matching agent now; agents created later are not covered. |
| DELETE | `/v1/orgs/{id}/delegations/{delegation_id}` | | Revoked; device credentials it approved stop working. |

`Agent`: `{address, handle, label, status, created_at, last_seen_at, devices: n, operators:
[{member_id, handle}]}`. Capabilities: `enroll`, `bind_handle`, `configure_rules`, `read_inbox`
(`ssh` is accepted and stored for Phase 8 but grants nothing yet). `read_inbox` lets the delegate
open the agent's mailbox in their apps (it is listed under `operated` in `GET /v1/orgs`).

## 5. Rooms

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/v1/orgs/{id}/rooms` | | `{rooms:[{id, name, description, join_policy, post_policy, listed, auto_join, member_count, activation_state, created_by}]}` — admins see every room; members see listed rooms and rooms they are in. |
| POST | `/v1/orgs/{id}/rooms` | `{name, description?, join_policy?, post_policy?, listed?, auto_join?}` | `201 {room}`. Created `provisioning` with the caller's org mailbox as keying owner; their inbox activates it (E2EE needs a device). |
| PATCH | `/v1/orgs/{id}/rooms/{group_id}` | `{auto_join?, listed?}` | Room admins and people admins. |
| DELETE | `/v1/orgs/{id}/rooms/{group_id}` | | Owner/admin/people admin, or the room's owner. |

Inbox: `GET /v1/rooms?org_id=<id>` lists listed org rooms for an org member (acting as their org
mailbox); `POST /v1/groups {…, org_id}` creates a room from the inbox; joining a public org room
requires an active membership of that org. New members are queued (`keying`) into every active
`auto_join` room when they accept their invitation.

## 6. Rules (§9)

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/v1/orgs/{id}/rules` | | `{rules:[Rule], revision}` |
| POST | `/v1/orgs/{id}/rules` | `Rule` without id | `201 {rule}` |
| PATCH | `/v1/orgs/{id}/rules/{rule_id}` | partial `Rule` | `{rule}` |
| DELETE | `/v1/orgs/{id}/rules/{rule_id}` | | |
| POST | `/v1/orgs/{id}/rules/evaluate` | `{from, to, verb?}` | `{admitted, known, autonomy, verbs, matched_rule, decided_by: "personal_block"\|"personal_contact"\|"org_rule"\|"inbox_policy", explanation}` |

`Rule`: `{id, owners, peers, admission: "allow"|"block"|null, autonomy: "review"|"auto"|null,
verbs: [..]|null, known: bool, locked: bool, priority: 1..1000 (default 100), description?}`.

Pattern grammar (both `owners` and `peers`): `members`, `agents`, an exact handle `/acme/agent1`,
`/acme/*`, or one trailing wildcard on the last segment `/acme/build-*`. `peers` also accepts `*`
(anyone). `owners` must stay inside the org namespace. Evaluation (`do_send`, call admission and
the inbox's per-message verdict): a personal block always wins (D-02); then org rules ordered by
priority (lower first) then specificity (exact > wildcard > members/agents > `*`), first rule to set
a field wins; then the personal contact; then the inbox policy, filters and throttles. `locked`
rules bind **agent** mailboxes only (a member can still add personal contacts or blocks). Org
rules may set `autonomy: auto` for agents only, and never for `never_auto` verbs (`git_push`,
`deploy`, `read_credentials`, `spend`, `delete_files`, `run_shell`). Received messages carry
`matched_rule` (`{id, description}`) beside `matched_contact`.

Templates the portal offers: *Everyone at Acme is known to each other* (`/acme/*` → `/acme/*`,
allow, known — created on activation), *Let a person direct an agent*, *Agents only talk to the
company* (`agents` ← `*`, block; plus `agents` ← `/acme/*`, allow, priority 50).

## 7. Security, domains, audit

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/v1/orgs/{id}/domains` | | `{domains:[{domain, txt_name, txt_value, verified_at, last_checked_at, failures}]}` |
| POST | `/v1/orgs/{id}/domains` | `{domain}` | `201 {domain…}` — publish `TXT <domain> "pigeonpost-verify=<value>"`. |
| POST | `/v1/orgs/{id}/domains/{domain}/verify` | | Checks two DNS-over-HTTPS resolvers; `{verified, domain…}`. Rechecked weekly; three failures in a row unverify. |
| DELETE | `/v1/orgs/{id}/domains/{domain}` | | |
| GET | `/v1/orgs/{id}/audit` | `?action=&before_id=&limit≤200` | `{entries:[{id, at, actor_kind, actor, action, target, details}], next_before_id}` |
| GET | `/v1/orgs/{id}/audit/export` | `?format=csv\|json` | File download. Step-up. |

## 8. Device login with a pre-assigned handle (§10)

CLI: `pigeonpost login --device --handle /acme/agent1 [--label "Build box"] [--onboard]` and
`pigeonpost login --enroll CODE` (or `--enroll-stdin`). Both end with the machine holding **only**
a mailbox credential for that agent: an existing account session on the machine is revoked at the
realm and deleted.

1. The machine makes an Ed25519 device key and a 32-byte claim secret, then
   `POST /v1/device-pairings {handle, device_public_key, claim_challenge, label?}` (no auth;
   `device_public_key` b64 32 bytes; `claim_challenge` = b64 SHA-256 of the secret) →
   `201 {pairing_id, user_code: "ABCD-EFGH", verification_uri, verification_uri_complete,
   confirmation: "123 456", expires_in: 600, interval: 5}`. It prints the code, the confirmation
   and a QR of `verification_uri_complete` (`https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH`).
2. A person opens the page, signs in, and the page calls
   `POST /v1/device-pairings/resolve {user_code}` → `{pairing_id, org:{id, name}, handle,
   address, label, confirmation, capabilities, ip_country, requested_at, expires_at, can_approve,
   reason?}`. The page shows handle, label, confirmation, IP country (an untrusted hint) and a
   hold-to-approve button.
3. `POST /v1/device-pairings/{pairing_id}/approve {user_code}` (person, step-up, role or `enroll`
   delegation) or `…/deny`.
4. The machine polls `POST /v1/device-pairings/claim {pairing_id, claim_secret, signature}` where
   `signature` = Ed25519 over `"pigeonpost-pairing-claim-v1\0" + pairing_id`. Answers:
   `428 authorization_pending`, `410 pairing_expired`, `403 access_denied`, or `200 {credential:
   "cap_…", address, handle, expires_at, device_id}`. A retry after success with the same proofs
   (within 10 minutes) replaces the credential rather than adding a second one.

Enrollment: `POST /v1/enrollments/redeem {code, device_public_key, signature, label?}` (no auth;
`signature` = Ed25519 over `"pigeonpost-enroll-v1\0" + code`) → `200 {credential, address, handle,
expires_at, device_id}` or `400 enrollment_invalid` (one answer for wrong, used, expired or
revoked).

The credential is a mailbox capability token (`cap_…`): every existing mailbox route accepts it. It
stops working when it is revoked, the agent is suspended or deleted, the org is suspended, or the
approver's role or delegation changes.

## 9. Events

`org.member {org_id, member_id, status, roles}` to the member's account; `org.policy {org_id,
revision}` to every member's account after a rule change; `org.status {org_id, status}` to members
on lifecycle changes. Clients ignore unknown types.
