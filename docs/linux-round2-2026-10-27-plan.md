# Linux native round two (1.0.10)

Bring the GTK 4/libadwaita Linux desktop (`apps/linux`, 1.0.9 with Phase 1b and 3b groups) to
parity with the round-two features the Apple, Android and Windows clients received. Isolated
worktree `/Users/bekirdag/Documents/apps/pigeonpost-linux-r2-20261027`, branch
`claude/linux-round2-20261027` from main `9737cf6` (PR135). The shared checkout and Codex's
`pigeonpost-windows-*` worktrees are not touched. No release tag: releases stay batched.

Binding constraints (Codex → Claude handoff, 27 October): synthetic loopback fixtures only; no real
accounts, messages, calls, OAuth/token exchanges or production writes; no secrets in source, logs or
artifacts; Apple review and store submissions wait for the owner; Nests/Products/hosted AI remain
planning only; free/open-source infrastructure only; qualify CI on the exact head without waiving
assertions.

## 0. Foundation: work stays with its login, account and selection

- `binding.py`: a `Lease` captures the session's accepted-login epoch and owner (SHA-256 of the
  token's issuer and subject, a local key only). `auth.Session` starts a new epoch for every sign-in,
  restore, sign-out, failed renewal or a renewal naming another account.
- `api.Postbox.bound(lease)`: every request first checks its lease (under the session lock, also after
  a renewal), so work begun under an earlier login stops with `Retired` and never borrows a newer
  token. `post_exact` posts reviewed bytes unchanged. Errors keep only bounded `WWW-Authenticate` and
  `Retry-After` hints.
- `Window.bound_work`: results are published while their lease is current (not merely while the same
  view is selected), so a send that completes after a mailbox switch still clears the right draft,
  and nothing from a retired login lands in a newer one. Mailbox leases are children of the login.
- `protected.py`: one sealed record per account and mailbox (the group store's XChaCha20-Poly1305
  sealing, key/hash/length in Secret Service, fsync + rename, owner/mailbox checked on open, removed
  at sign-out). Memory only, and said so, without the library or a keyring.

## 1. Message actions and receipts (SDS §14C, `docs/message-actions.md`)

- `actions.py`: `msg_ref` generation/validation, the exact `POST /v1/send` shape (reply/quote/edit
  rules), deterministic wire bytes and strict re-parsing, the 24 h edit window, reply quotes, direct
  ticks (sending/sent/delivered/seen/working/failed with accessible labels) and the group tick rule.
- `delivery.py`: protected delivery. The request is frozen (recipient resolved once through
  `recipient_lookup` and checked against this mailbox) and saved before the first POST. Confirmed →
  removed; definite 4xx → removed, draft kept; uncertain (no answer/5xx/408) → kept for explicit
  Retry (identical bytes; a repeated msg_ref returns the original ids) or Discard (local only). One
  unconfirmed request per recipient and subject; nothing replays automatically, even after restart.
- Drafts are saved (debounced) in the same sealed record and restored per account/mailbox.
- Window (`window_messages.py`): inbox read with `actions=1&fold=1` once enabled; action row below
  and outside the bubble (received: copy, reply; sent: edit while editable, copy; More menu and
  right-click/long-press keep reply, copy, edit, info, delete, report, block); ticks inside the
  bubble with a green gear (static + "Working…" under reduced motion); "edited" label; message info
  with versions and receipt times; reply quote that scrolls to and highlights the parent; reply and
  edit modes with a mode bar (✕/Esc restores the previous draft); ack `with_edits`; Settings →
  Privacy "Send read receipts"; unconfirmed messages inline and in a review dialog.
- Groups (`groups_core.py`, `window_groups.py`): fold only edits whose encrypted reference matches the
  server's and the same sender; versions kept per chain; whole-chain deletion; replies; group
  receipts and ticks; protected outbox with explicit retry of the exact request.

## 2. Calendar (SDS §14B, `docs/calendar.md`)

`calendar_core.py` + `window_calendar.py`: week, day, month and agenda views; calendars; create/edit
with recurrence and IANA zones (nonexistent/ambiguous times asked, never shifted); scopes
this/following/all with `expected_revision`; cancel; attendees; invitations and RSVP with the
attendee revision; personal reminders as desktop notifications (titles only with previews on);
`.ics` export and import (preview/confirm replay); feeds; settings. Creating routes use protected
`request_key` attempts with explicit retry. Scheduled meetings open the browser (WebKitGTK has no
WebRTC) at the meeting link with the chosen occurrence. No end-to-end label on calendar data.

## 3. Organization rooms (SDS §4, `docs/orgs.md`)

`orgs_core.py` + `window_orgs.py`: the org membership catalog and room directory acting as the
member's org mailbox; join/create per policy; operated (delegated) agents listed separately from
owned inboxes and never treated as personal mailboxes; cached metadata only hints, every action asks
the server. Room creation keeps durable custody of an uncertain `POST /v1/groups` (same request key
on explicit retry, never re-created silently).

## 4. Device approval (`docs/device-pairing-native.md`, `docs/orgs.md` §8)

`pairing.py`, `strong_signin.py`, `window_pairing.py`: strict code/link parsing (fragment only,
memory only), resolve, an actor-bound approval state machine (login + mailbox + code), the
confirmation screen (organization, handle, label, access, network hint and its limits, expiry,
confirmation number, explicit agent creation, hold or deny), end states and Retry-After. Step-up
parsing and a complete Authorization Code + PKCE (S256) loopback broker with same-person proof are
implemented and tested against loopback fixtures.

**Gated:** a read-only probe of the production `pigeonpost-linux` client answered
`Invalid parameter: redirect_uri` (device flow only; `standardFlowEnabled=false`, no redirect URIs,
as `provision-oauth.py` provisions it). Keycloak is not changed. Native approval therefore ships
disabled behind a capability check with a clear message and an "Approve in your browser" button
that opens `pair.html#CODE` (fragment). Required client change: enable the standard flow for the
public client with PKCE S256 and the native loopback redirect `http://127.0.0.1/*` (random port),
preserving its device flow, consent and scopes; then list the client as strong-sign-in capable.

## 5. Release surfaces and qualification

- Version 1.0.10: `pigeonpost/__init__.py`, metainfo release, Debian control (adds `tzdata`), the
  workflow's package names and draft release notes, README. The website still offers 1.0.9 until the
  batched release.
- Tests: core (`test_core`, `test_actions`, `test_calendar`, `test_orgs`, `test_pairing`), MLS
  (`test_groups`), GTK under Xvfb (`test_ui*.py`), installed .deb and both Flatpaks, and the live
  loopback suites `tests/e2e-messages/linux.sh` (messages, calendar, orgs, groups) against a real
  postbox in the `interop` job. Local GTK runs use a throwaway container on raijin.
- CI qualified on the exact head before merge; packages and SHA256SUMS saved to
  `~/Documents/releases/pigeonpost/linux-1.0.10/`.

Progress: [ledger](linux-round2-2026-10-27-progress.md).
