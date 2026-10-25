# Calendar and scheduled meetings — client contract (Phase 3B)

Status: implemented in the postbox, the calls service and the web inbox (Phase 3B, 2026-10-21).
Native clients follow and must implement exactly this contract. Product rules are SDS §14B; the
privacy commitments are on `pigeonpost.dev/privacy.html#calendar`.

Everything below is on the postbox (`https://postbox.pigeonpost.dev`), JSON unless stated, except
§7, which is the calls service (`https://inbox.pigeonpost.dev/calls`).

## 0. Conventions

| Item | Rule |
|---|---|
| Auth | `Authorization: Bearer <member JWT \| pk_live_… \| cap_…>`. Guest routes (§6) take no bearer. |
| Acting mailbox | `x-pigeonpost-identity: /k/…` (or `?identity=`). A calendar belongs to a **mailbox**, not an account. |
| Flags | `calendar` gates every route here (off → `404 feature_disabled`). `calendar_email` gates invitations, updates, reminders and verification codes sent to **contact addresses** (people without a Pigeonpost handle); while it is off those guests are still recorded and the organizer can copy their personal invitation link by hand (§3.7). Ask `GET /v1/capabilities` (`features[]`). |
| Idempotency | Creating routes take `request_key` (1–64 of `[A-Za-z0-9_-]`): same key + same body → the original response; same key + other body → `409 idempotency_conflict`. Kept 7 days. |
| Revisions | Events carry `revision` (optimistic concurrency, bumps on every change) and `sequence` (the iCalendar `SEQUENCE`, bumps only on organizer scheduling changes). Mutations take `expected_revision`; stale → `409 revision_conflict {current_revision}`. |
| Encryption | Calendar data is **not end-to-end encrypted**. Titles, descriptions, locations and notes are sealed at rest under the postbox vault and are readable by the service, which renders them into reminders and invitations. Clients must not show a lock or "end-to-end" label on calendar data. |
| Errors | `{error, message, …}`. Codes: `feature_disabled`, `not_found`, `forbidden`, `human_required`, `revision_conflict`, `idempotency_conflict`, `recurrence_invalid`, `time_invalid {kind: nonexistent\|ambiguous\|order}`, `zone_unknown`, `window_too_large`, `expansion_limit`, `attendee_limit`, `invite_limit`, `meeting_capacity`, `external_unavailable`, `verification_required`, `guest_token_invalid` (always generic), `window_closed`, `event_cancelled`, `calendar_unavailable`, `rate_limited` (429 + `Retry-After`). |

Human-only: adding contact-address guests (or any change that sends them a new invitation) needs a
person's interactive session **with a verified sign-in address**; API keys, `cap_` tokens and
connectors get `403 human_required`. Handle-only events may be created by agents.

## 1. Identifiers and time

| Thing | Form |
|---|---|
| Calendar | `cal_` + 26 base32 |
| Event | `evt_` + 26 base32. iCalendar `UID` = `<event id>@pigeonpost.dev`. |
| Occurrence | `occ_<26 base32 of the event id>_<recurrence key>` (e.g. `occ_…_20261022T090000`), **deterministic** from (event, recurrence key): the same occurrence always has the same id, whether expanded at read time or stored. A single event has exactly one occurrence. Treat it as opaque (≤ 80 characters of `[A-Za-z0-9_]`). |
| Recurrence key | The original, typed start of an occurrence: `YYYYMMDDTHHMMSS` (local wall time in the event's zone) for timed events, `YYYYMMDD` for all-day events. Moving an occurrence never changes its key. |
| Attendee | `att_` + 26 base32. Stable per invitation; removing and re-inviting someone makes a new one. |
| Meeting code | 10 characters of `ABCDEFGHJKMNPQRSTUVWXYZ23456789`. Reserved by the postbox for the meeting series and never handed to an ad-hoc call (ad-hoc codes are 6 characters). Display as `XXXXX-XXXXX`. A code is a **locator**, not a credential. |
| Meeting URL | `https://inbox.pigeonpost.dev/m/<CODE>` |

Timed events are authored as wall times in an IANA zone: `start: "2026-10-22T09:00"`,
`end: "2026-10-22T10:00"`, `tzid: "Europe/Istanbul"`. All-day events use dates with an exclusive end:
`start_date: "2026-10-22"`, `end_date: "2026-10-23"`. Responses also carry `starts_at`/`ends_at`
(Unix seconds) for timed occurrences.

A wall time that does not exist (spring-forward gap) is refused with `time_invalid {kind:
"nonexistent"}`; one that happens twice (fall-back) is refused with `time_invalid {kind: "ambiguous"}`
unless the request says `fold: "earlier"` or `"later"`. Ask the user; do not shift silently.
Recurrence instances follow RFC 5545: an instance in a gap moves forward by the gap length, an
ambiguous instance takes the earlier offset.

Recurrence: `rrule` is the RFC 5545 rule body without the `RRULE:` prefix. Supported parts: `FREQ`
(`DAILY`, `WEEKLY`, `MONTHLY`, `YEARLY`), `INTERVAL` (1–1000), `COUNT` (1–1000), `UNTIL`, `BYDAY`,
`BYMONTHDAY`, `BYMONTH`, `BYSETPOS`, `WKST`. Anything else → `recurrence_invalid`. `exdates` is a list
of recurrence keys. The server expands in the event's own zone, so a weekly 09:00 meeting in
New York stays at 09:00 New York time across a DST change.

## 2. Calendars

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/v1/calendars` | | `{calendars:[{id, name, color, is_default, revision}]}`. A mailbox's default calendar ("Calendar") is created on its first calendar write; GET never creates one, so an empty list is normal. |
| POST | `/v1/calendars` | `{name, color, request_key}` | `201 {calendar}`. Colors: `blue`, `green`, `orange`, `red`, `purple`, `teal`, `gray`. ≤ 20 calendars per mailbox. |
| PATCH | `/v1/calendars/{id}` | `{expected_revision, name?, color?, is_default?}` | |
| DELETE | `/v1/calendars/{id}` | `{expected_revision}` | Cancels the events **you organize** in it (attendees are told) and moves invitations you received in it to your default calendar. The default calendar cannot be deleted while it is the only one. |

## 3. Events

### 3.1 Listing occurrences

`GET /v1/calendar/events?from=<unix>&to=<unix>&calendars=cal_a,cal_b&include=invitations`

The window is at most 62 days. The response lists effective occurrences, with moved ones placed at
their new time (a moved occurrence appears when its new time is in the window even if its original
time is not). More than 1,000 occurrences → `422 expansion_limit`; ask for a smaller window.

```json
{ "occurrences": [ {
  "occurrence_id": "occ_…", "event_id": "evt_…", "recurrence_key": "20261022T090000",
  "calendar_id": "cal_…", "color": "blue",
  "all_day": false, "starts_at": 1792735200, "ends_at": 1792738800, "tzid": "Europe/Istanbul",
  "start_date": null, "end_date": null,
  "title": "Design review", "location": "Room 2",
  "status": "confirmed", "recurring": true, "overridden": false,
  "organizer": {"address": "/k/…", "handle": "/bekir"}, "role": "organizer",
  "my_rsvp": null, "invited": false,
  "meeting": {"code": "ABCDEFGHJK", "url": "https://inbox.pigeonpost.dev/m/ABCDEFGHJK",
              "opens_at": 1792734600, "closes_at": 1792740600},
  "privacy": "default", "transparency": "opaque", "revision": 3
} ] }
```

`include=invitations` adds pending invitations (`invited: true`, `my_rsvp: "needs_action"`). Declined
invitations stay listed with `my_rsvp: "declined"` so the user can change their mind; filtered ones
are never listed.

### 3.2 Creating

`POST /v1/calendar/events`

```json
{ "request_key": "k1", "calendar_id": "cal_…",
  "title": "Design review", "description": "…", "location": "…",
  "all_day": false, "start": "2026-10-22T09:00", "end": "2026-10-22T10:00", "tzid": "Europe/Istanbul",
  "rrule": "FREQ=WEEKLY;BYDAY=TH;COUNT=6", "exdates": [],
  "transparency": "opaque", "privacy": "default", "locale": "en",
  "attendees": [ {"handle": "/alice"}, {"handle": "/bob", "role": "cohost"},
                 {"contact": "guest@example.com", "name": "Guest"} ],
  "meeting": { "enabled": true, "waiting": "guests", "admit": "hosts", "allow_knock": false,
               "join_before_minutes": 10, "guests_see_list": false, "transport": "mesh" },
  "reminders": [ {"minutes_before": 10, "channel": "push"} ] }
```

→ `201 {event, delivery_status: "queued"}`.

- `calendar_id` may be omitted (the default calendar, created if needed).
- `attendees`: `{handle}` takes a handle or a `/k/` address of a Pigeonpost mailbox; `{contact, name?}`
  is a contact-address guest. `role: "cohost"` only for handle attendees. `optional: true` allowed.
  ≤ 500 handle attendees and ≤ 50 contact guests per event; ≤ 200 new contact invitations per
  account per day.
- `meeting.waiting`: who waits in the lobby — `guests` (default; contact guests and unverified link
  holders wait), `everyone_except_hosts`, or `none` (invited handle attendees and verified guests go
  straight in). `meeting.admit`: who may let people in — `organizer`, `hosts` (organizer and
  co-hosts, default) or `delegated` (hosts plus attendees listed in `meeting.admitters`).
  `allow_knock`: uninvited people with the link may ask to join (default off). `transport`:
  `mesh` (≤ 6 people, default) or `sfu` (≤ 30, needs browsers that encrypt frames). The organizer plus
  every non-optional invitee must fit the transport's limit (`meeting_capacity`).
- An all-day event cannot be a meeting (`meeting.enabled` is refused).
- `reminders` are the **caller's own** (organizer). Default: 10 minutes before, push.
- `locale` (`en`, `tr`) is the language of contact-address invitations.

Invitations obey each recipient's admission settings, exactly as messages do: a recipient who
blocked the organizer gets nothing at all (no entry, no push, no event), a recipient whose filters
catch the organizer gets the invitation under **Filtered** (no push, no reminders, not on the
calendar) until they answer it. The organizer cannot tell these apart from an ordinary unanswered
invitation.

### 3.3 Reading one event

`GET /v1/calendar/events/{id}?occurrence_id=occ_…` →

```json
{ "event": { "id": "evt_…", "uid": "evt_…@pigeonpost.dev", "calendar_id": "cal_…",
  "revision": 3, "sequence": 1, "status": "confirmed",
  "organizer": {"address": "/k/…", "handle": "/bekir"},
  "title": "…", "description": "…", "location": "…",
  "all_day": false, "start": "2026-10-22T09:00", "end": "2026-10-22T10:00", "tzid": "Europe/Istanbul",
  "starts_at": 1792735200, "ends_at": 1792738800, "start_date": null, "end_date": null,
  "rrule": "FREQ=WEEKLY;BYDAY=TH;COUNT=6", "exdates": [],
  "transparency": "opaque", "privacy": "default",
  "meeting": {"code": "…", "url": "…", "waiting": "guests", "admit": "hosts", "allow_knock": false,
              "join_before_minutes": 10, "guests_see_list": false, "transport": "mesh",
              "participant_limit": 6, "admitters": []},
  "attendees": [ {"attendee_id": "att_…", "kind": "handle", "address": "/k/…", "handle": "/alice",
                  "name": "/alice", "role": "attendee", "optional": false,
                  "rsvp": "accepted", "rsvp_at": 1792700000},
                 {"attendee_id": "att_…", "kind": "external", "name": "Guest", "role": "attendee",
                  "rsvp": "needs_action", "proof": null} ],
  "counts": {"accepted": 1, "tentative": 0, "declined": 0, "needs_action": 1},
  "my": {"role": "organizer", "attendee_id": null, "rsvp": null, "calendar_id": "cal_…"},
  "reminders": [{"minutes_before": 10, "channel": "push"}],
  "linked_group": null, "can_edit": true },
  "occurrence": { "occurrence_id": "occ_…", "recurrence_key": "…", "starts_at": …, "ends_at": …,
                  "status": "confirmed", "my_rsvp": "accepted" } }
```

Who sees what:
- **Organizer:** everything, plus per attendee `delivery` (`queued`, `delivered`, `failed`,
  `suppressed`) and, for contact guests, `contact` (the address) and `proof`
  (`link` = answered through their invitation link, `verified` = after confirming their address).
- **Invited handle attendees:** the event and the attendee list. Contact guests appear only by the
  name the organizer typed, or "Guest" — never their address.
- **Anyone else:** `404 not_found`.

`GET /v1/calendar/events/{id}.ics` returns `text/calendar` for "Add to my calendar app": the caller's
own projection, `METHOD:PUBLISH`, a `VTIMEZONE`, the stable `UID` and `SEQUENCE`, and only the
caller's own `ATTENDEE` line. It contains no link that grants anything.

### 3.4 Changing

`PATCH /v1/calendar/events/{id}`
`{expected_revision, scope: "all"|"this"|"following", occurrence_id?, …fields from §3.2}`

- Organizer only (`403 forbidden`). `occurrence_id` is required for `this` and `following`.
- `this` changes one occurrence (time, title, details, location) and keeps its id and key.
- `following` ends the series before that occurrence (COUNT/UNTIL rewritten) and creates a new
  event from it, with a new `UID` and the **same meeting code**. The response returns both:
  `{event, split_from: {event_id, revision}, carried: {exceptions, responses}, dropped: {exceptions, responses}}`.
  Exceptions and per-occurrence answers are carried over when the new series still produces the
  same occurrence; otherwise they are dropped and counted.
- `all` changes the series; changing the time of a series keeps past occurrences' exceptions and
  drops future ones whose key no longer occurs (counted in `dropped`).
- Scheduling changes (time, recurrence, title, location, description) bump `sequence` and notify
  attendees; RSVP, reminders and private placement do not.

`POST /v1/calendar/events/{id}/cancel` `{expected_revision, scope, occurrence_id?, message?}` —
whole series (`all`), one occurrence (`this`), or the rest of the series (`following`). Attendees
get `event.cancelled`; contact guests a cancellation with `METHOD:CANCEL`.

`POST /v1/calendar/events/{id}/attendees`
`{expected_revision, add: [attendee input…], remove: ["att_…"], scope: "all"|"this", occurrence_id?}`
— organizer only. `scope: "this"` invites someone to (or removes them from) one occurrence only.
A removed contact guest gets a cancellation for what they were invited to; their links stop working.

`PATCH /v1/calendar/events/{id}/linked-group` `{expected_revision, group_id | null}` — organizer
only; the organizer must be an active member of the group. Meeting chat is an ordinary group: invite
people to it with the groups API; an RSVP never adds anyone to a group.

### 3.5 Answering

`POST /v1/calendar/events/{id}/rsvp`
`{status: "accepted"|"tentative"|"declined", note?, scope: "all"|"this", occurrence_id?}`

Your own answer only (≤ 300-character note). Answering a filtered invitation moves it out of
Filtered (it does not make the organizer a contact). Declining stops reminders and removes direct
entry to the meeting; you can change your answer later.

`PUT /v1/calendar/events/{id}/reminders` `{reminders: [{minutes_before: 0–40320, channel: "push"}]}`
— your own reminders for this event (≤ 5). `channel: "external"` is for guests only (§6).

`PUT /v1/calendar/events/{id}/placement` `{calendar_id}` — move an invitation to another of your
calendars (attendees only; the organizer moves their event by PATCH `calendar_id`).

### 3.6 Invitations

`GET /v1/calendar/invitations?state=pending|filtered` →
`{invitations: [{event_id, occurrence_id, title, starts_at, ends_at, all_day, start_date, end_date,
tzid, organizer, recurring, meeting: bool, invited_at}]}`. `pending` excludes filtered ones.

### 3.7 Contact-address guests, from the organizer's side

`POST /v1/calendar/events/{id}/attendees/{attendee_id}/link` → `{url, expires_at}` — organizer only:
a fresh personal invitation link for one contact guest, to send by any means while address
invitations are off or a message was lost. It lets that guest preview, answer, and ask to join
(they wait in the lobby until admitted). It does not reveal or change their address.

## 4. Feeds, availability, import

| Method | Path | Body | Result |
|---|---|---|---|
| POST | `/v1/calendar/feeds/{calendar_id}` | `{enabled, projection?: "busy"\|"personal_redacted"}` | `{url, expires_at}` on enable — shown **once**; the previous link stops working. `{enabled:false}` revokes. Human only. |
| GET | `/v1/calendar/feeds/{calendar_id}` | | `{enabled, projection, created_at, expires_at}` (never the link). |
| GET | `/v1/calendar/feed/{secret}.ics` | | The subscribable feed (no bearer). 90 days back, 365 ahead. `busy`: only "Busy" blocks. `personal_redacted`: titles and locations of your own events; private events and invitations as "Busy"; never meeting links, attendee lists or descriptions. 60 fetches/hour per link. Expires after 1 year. |
| POST | `/v1/calendar/freebusy` | `{attendees: ["/k/…"], from, to}` | `{busy: {"/k/…": [{start, end}] \| "unknown"}}`. In this phase only your own mailboxes resolve; everyone else is `"unknown"` (org availability arrives with organizations). |
| POST | `/v1/calendar/import/preview?calendar_id=` | raw `text/calendar` body, ≤ 512 KiB | `{draft_id, events: [{index, title, all_day, start, end, start_date, end_date, tzid, rrule, warnings}], warnings, expires_at}`. Nothing is fetched from the network; alarms, attendees and organizers are dropped (and listed in `warnings`). |
| POST | `/v1/calendar/import/{draft_id}/confirm` | `{select: [index…], calendar_id}` | `{created: [event…]}` — private copies with new UIDs. No invitations are sent. |

Confirmation accepts at most 200 selection entries and treats repeated indices as one selection.
An identical selection and destination can replay the original result for up to 7 days after a
successful confirmation, even if its response was lost; no events, reminders or notices repeat.
Omitting the destination and naming the same default calendar are equivalent. Changing the selection
or destination returns 409 `idempotency_conflict`. Receipts are sealed at rest, bound to the acting
mailbox, and require continuing access to the original calendar. Old imports without a receipt
cannot be recovered this way. An unconfirmed preview still expires after 1 hour.

## 5. Settings and events on `/v1/events`

`GET`/`PUT /v1/calendar/settings` `{previews: false, timezone: "Europe/Istanbul" | null,
week_start: 1, working_hours: {start: "09:00", end: "18:00", days: [1,2,3,4,5]}}`. `previews`
puts event titles into reminder pushes; off (default), pushes say only "Event starting soon".

Typed account events (with `account_events`, or `?typed=1`): `event.invite {event_id, revision}`,
`event.updated {event_id, revision, occurrence_id?}`, `event.cancelled {event_id, revision,
occurrence_id?}`, `event.rsvp {event_id, attendee_id, revision, occurrence_id?}` (organizer only),
`event.reminder {event_id, occurrence_id, reminder_id, starts_at}`. They carry ids only; fetch the
event to see details (a removed attendee gets `404`, and must drop what it cached).

Pushes: invitations push "New calendar invitation" (no details). Reminders push "Event starting in
N minutes" (title only with `previews` on). `message_id` is `cal:<occurrence_id>:<reminder_id>`.

APNs and FCM calendar payloads also carry `type: "calendar"`, `calendar_event_id`, and the
recipient mailbox in `mailbox` (FCM additionally has `identity`). Reminders include
`calendar_occurrence_id`; invitations omit it. These identifiers are locators, never credentials.
Open the event using the recipient mailbox's ordinary calendar authorization; a removed attendee
still gets `404`. Do not infer an event ID from an opaque occurrence or route the notice to its
legacy organizer `peer`. Old `cal:` notices without typed locators may open the calendar instead.
Calendar APNs payloads omit the ordinary message badge and use a calendar-specific notification
thread. FCM remains data-only with generic text and string-valued fields.

## 6. Guest routes (no Pigeonpost account; no bearer)

A contact guest's invitation link is `https://inbox.pigeonpost.dev/m/<locator>#t=<token>` (locator =
meeting code, or the event id for events without a meeting; `&r=accepted|tentative|declined`
preselects an answer, which the page then asks to confirm — opening a link never changes anything).
The page must read the fragment, remove it from the address bar, and send the token only in POST
bodies. Unsubscribe links are `…/m/u#u=<token>`.

| Method | Path | Body | Result |
|---|---|---|---|
| POST | `/v1/calendar/guest/preview` | `{token}` | `{event: {title, all_day, start, end, start_date, end_date, tzid, starts_at, ends_at, location, organizer_name, recurring, status, meeting: {code, url, opens_at, closes_at} \| null}, occurrences: [next ≤ 10 {occurrence_id, starts_at, ends_at, status}], attendee: {name, rsvp, proof}, guests: [{name, rsvp}] \| null, verification_available}`. |
| POST | `/v1/calendar/guest/ics` | `{token} \| {guest_session}` | `text/calendar` (the guest's own projection). |
| POST | `/v1/calendar/guest/rsvp` | `{token \| guest_session, status, note?, scope, occurrence_id?}` | `{rsvp, proof}`. |
| POST | `/v1/calendar/guest/verify/start` | `{token}` | `{challenge_id, expires_at}` — a 6-digit code goes to the address the organizer entered (never one the caller supplies). `503 external_unavailable` while address invitations are off. 3 per invitation per hour. |
| POST | `/v1/calendar/guest/verify/complete` | `{challenge_id, code, device_public_key, device_proof, ts}` | `{guest_session, session_id, expires_at}` (≤ 24 h). 5 attempts per challenge. Repeating the same call returns the same session. |
| POST | `/v1/calendar/guest/recover` | `{locator, contact}` | Always `{ok: true}`. If that address holds a live invitation for that event, a code is sent to it. |
| POST | `/v1/calendar/guest/join` | `{guest_session, occurrence_id, device_proof, ts} \| {token, occurrence_id}` | `{ticket, code, occurrence_id, admission: "direct"\|"knock", expires_at}` — single use, 2 minutes. With only the invitation link (`token`) admission is always `knock`. |
| POST | `/v1/calendar/guest/unsubscribe` | `{token, scope: "organizer"\|"all"}` | `{ok: true}`. Only stops invitation messages; works after the event ends. |

Device proof: the browser makes an ECDSA P-256 key pair (WebCrypto, non-extractable) and sends the
public key as base64url of the 65-byte uncompressed point. `device_proof` is base64url of the raw
64-byte `r‖s` signature (WebCrypto's format, SHA-256) over the UTF-8 text
`pigeonpost-guest-proof-v1\n<purpose>\n<binding>\n<ts>`, where `ts` is Unix seconds (±120 s) and
`purpose`/`binding` are `verify`/`<challenge_id>` or `join`/`<session_id>\n<occurrence_id>`.

## 7. Joining a scheduled meeting (calls service)

The meeting room is opened by the first authorized person to join within the window: from
`join_before_minutes` (default 10) before the occurrence starts until 30 minutes after it ends.
Rooms are per occurrence; one code, many occurrences.

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/calls/v1/meetings/scheduled/{code}` | optional member bearer | Members: `{state: "open"\|"not_open"\|"unavailable", role, admission, occurrences: [{occurrence_id, starts_at, ends_at, opens_at, closes_at}], room: {participants} \| null}`. Without a bearer: `{state: "open"\|"unavailable", can_knock}` only — no title, time or roster. |
| POST | `/calls/v1/meetings/scheduled/{code}/join` | `{session, as?, occurrence_id?, e2ee, ticket?, name?}` | Joins (`{state: "joined", …room}`) or waits (`{state: "waiting", room, knock}`). Members send `as` (their mailbox); guests send the `ticket` from §6; uninvited people send `name` and may knock only when `allow_knock` is on. `409 choose_occurrence {occurrences}` when two windows overlap. `403 window_closed`, `not_invited`, `declined`. |
| POST | `/calls/v1/rooms/{room}/knocks/{knock}` | `{session, admit}` | In a scheduled room only the people `admit` allows may answer (`403 not_allowed`). |
| POST | `/calls/v1/rooms/{room}/end` | `{session}` | Host only: ends the call for everyone. |
| POST | `/calls/v1/rooms/{room}/extend` | `{session, minutes: 15\|30}` | Host only: keeps the room open longer, at most 2 hours past the scheduled end. |

A scheduled room's `roomView` has `scheduled: {event_id, occurrence_id, role, can_admit, closes_at}`.
Only hosts and co-hosts may send the `mute` signal (and only ever mute). Being the first in the room
grants nothing. People removed from the event, who decline, or whose invitation is cancelled are
removed from a running room within two minutes. Call history entries of scheduled meetings carry
`calendar_event_id` and `occurrence_id`.

## 8. Internal: calls service ↔ postbox

`POST /internal/v1/calendar/meeting-access` on the postbox's internal listener, with the calls
service's own token (`POSTBOX_SVC_TOKEN_CALLS`). Never with a member token.

```json
{ "code": "ABCDEFGHJK", "occurrence_id": "occ_…",
  "actor": {"kind": "member", "mailboxes": [{"address": "/k/…", "handle": "/alice"}]}
         | {"kind": "guest", "ticket": "…"}
         | {"kind": "anonymous"},
  "purpose": "status" | "join" | "recheck" | "extend",
  "extend_minutes": 30 }
```

`mailboxes` are the ones the calls service verified for the member's token (`/v1/identities` +
`/v1/whoami`), one when the member chose `as`; the postbox picks the strongest role among them.
`occurrence_id` is optional; without it the postbox picks the one occurrence whose window is open,
or answers `choose_occurrence`. A guest `ticket` is redeemed exactly once (`purpose: join`); a
`recheck` of a guest uses `{"kind": "guest", "attendee_id": "att_…"}`.

```json
{ "allowed": true, "reason": null,
  "meeting_id": "mtg_…", "code": "ABCDEFGHJK", "event_id": "evt_…", "occurrence_id": "occ_…",
  "starts_at": 0, "ends_at": 0, "opens_at": 0, "closes_at": 0,
  "role": "host" | "cohost" | "attendee" | "guest" | "none",
  "admission": "direct" | "knock" | "deny",
  "can_admit": true,
  "participant": {"address": "/k/…", "handle": "/alice", "attendee_id": "att_…", "name": "/alice",
                  "verified": true},
  "organizer": {"address": "/k/…", "handle": "/bekir"},
  "policy": {"waiting": "guests", "admit": "hosts", "allow_knock": false,
             "participant_limit": 6, "transport": "mesh"},
  "admitters": ["/k/…"],
  "policy_version": "3.0", "occurrences": [] }
```

`allowed: false` comes with `reason`: `unavailable` (no such code — also what anonymous callers get
outside an open window), `window_closed` (with `occurrences` for invited members), `not_invited`,
`declined`, `cancelled`, `ticket_invalid`, `choose_occurrence` (with `occurrences`). The calls
service fails closed when the postbox cannot be asked. `admitters` are addresses allowed to answer
knocks (and `can_admit` says whether this participant is one). `closes_at` already includes any
extension. `extend` is host only and moves `closes_at` by `extend_minutes`, never beyond 2 hours
after the scheduled end.

Call history: the calls service reports scheduled rooms with `from` = the organizer's address,
`join` events with `by` = each member who joins, `end` when the room closes, and
`calendar_event_id` + `occurrence_id` on every event. `room_id` is unique per room.
