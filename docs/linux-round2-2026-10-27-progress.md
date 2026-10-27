# Linux native round two — progress

Plan: [linux-round2-2026-10-27-plan.md](linux-round2-2026-10-27-plan.md). Branch
`claude/linux-round2-20261027`, rebased onto main `8e45968` (PR136). The exact-head CI
qualification is recorded in the pull request, because a later documentation-only commit would not
re-run the Linux workflow.

## 27 October 2026

### Inputs and constraints

- Read the Codex → Claude handoff (its "Preserve these constraints" section is binding), the
  21 October handoff and ledger, SDS §4/§6/§10/§14B/§14C and the contracts `docs/message-actions.md`,
  `docs/groups.md`, `docs/calendar.md`, `docs/orgs.md`, `docs/device-pairing-native.md`. Mirrored
  the Windows delivery coordinator/recovery scope, calendar and enterprise code and the Android
  approval store for behaviour and safety.
- Docdex: the worktree was mounted as its own repository (`/v1/initialize` with this exact root; the
  MCP session was bound to another repository, so the HTTP API was used). The impact graph for
  `api.py` lists `auth`, `binding`, `calls`, `ui`, `window_groups`, `window_privacy` and the new
  modules; every `api.py` change is additive (new keyword arguments with old defaults, an optional
  `headers` on `APIError`), so existing callers are unchanged.
- OAuth: a single unauthenticated, read-only authorization request for `pigeonpost-linux` with an
  `http://127.0.0.1` redirect returned `400 Invalid parameter: redirect_uri` (no sign-in, no token
  exchange). This matches `provision-oauth.py`: device flow only, standard flow off, no redirects.
  Keycloak was not changed.

### Implemented

- **Login binding** (`binding.py`, `auth.py`, `api.py`, `ui.py`): login epochs and owner keys, leases
  with mailbox children, lease-checked token use (also after renewal), `post_exact`, `bound_work`
  publishing only while the originating login is current, `Retired` silently dropped.
- **Protected store** (`protected.py`): sealed per account/mailbox records, keyring-held key, owner and
  mailbox verified on open, tamper refusal, removal at sign-out; memory-only fallback when the group
  library or an owner is unavailable.
- **Message actions and receipts** (`actions.py`, `delivery.py`, `message_ui.py`,
  `window_messages.py`): see the plan §1. Uncertain sends are kept and shown inline and under
  "Unconfirmed"; Retry posts identical bytes, Discard is local only; one unconfirmed request per
  recipient and subject; recipients frozen through `recipient_lookup`; drafts saved and restored.
- **Groups** (`groups_core.py`, `window_groups.py`): verified same-sender edit folding with explicit
  missing history, reply validation, 24 h edit rule with retained attachments, whole-chain deletion
  (response `seqs` and metadata-only reconciliation), receipts/ticks and member lists in message
  info, a sealed outbox (16 requests / 8 MiB, 24 h retry) with re-encryption only after
  `epoch_conflict`/`key_activation_pending`.
- **Calendar** (`calendar_core.py`, `window_calendar.py`): week/day/month/agenda, editor with zones,
  all-day and repeat rules (nonexistent/ambiguous times asked), this/following/all with
  `expected_revision` (a conflict reloads, nothing is overwritten), cancel, guests, invitations and
  RSVP with the attendee revision, personal reminders as desktop notifications (title only with
  previews), `.ics` export and replay-safe import, feeds, settings, protected creates with
  `request_key`. Join re-checks the exact occurrence and the calls service's scheduled status, then
  opens `https://inbox.pigeonpost.dev/m/<CODE>`; the web page has no occurrence parameter, so
  overlapping windows are explained in words.
- **Organization rooms** (`orgs_core.py`, `window_orgs.py`): strict catalog, room directory as the org
  inbox, join after a fresh server resolve, create with durable custody of an uncertain
  `POST /v1/groups`, operated agents in their own section and never added to owned mailboxes; a
  refusal clears cached catalog/rooms.
- **Machine approval** (`pairing.py`, `strong_signin.py`, `window_pairing.py`): strict code/link
  parsing, login-bound approval state machine, confirmation screen with hold-to-approve (keyboard and
  assistive activation confirm instead), Deny native, Retry-After honoured, lost decisions never
  resent, step-up parsing (body and challenge, quote-aware, duplicate-safe), complete PKCE S256
  loopback broker with same-person proof. `STRONG_SIGNIN_CLIENTS` is empty: native Approve is
  disabled with a clear message and an "Approve in browser" button for `pair.html#CODE`.
- **Release surfaces**: version 1.0.10, metainfo release, Debian `tzdata` dependency and
  description, workflow package names, draft release notes, README. The website keeps 1.0.9 until
  the batched release.

### Local qualification (synthetic only)

- Core on macOS and in an Ubuntu 24.04 container: `test_[!ug]*.py` — 110 tests OK (the group-library
  sealer test runs where the library is present).
- MLS on Ubuntu 24.04 (container on raijin with the CI-built x86_64 library): `test_groups.py` 25/25.
- GTK under Xvfb (same container): `test_ui*.py` 64 run, 1 skipped (screenshots, CI only), 1 failure
  only in that scratch image — the MP4 thumbnail, because the image lacks `gstreamer1.0-libav`, which
  CI installs; the test is unchanged. A shutdown-time `gtk_root_get_focus` warning appears after the
  last test finishes (no Python frame, after interpreter finalization starts); assertions are
  unaffected.
- Live loopback postbox (`tests/e2e-messages/linux.sh`, postbox built from this worktree): messages
  32/32 (response loss before/after commit with exact explicit retry, single delivery, conflicts,
  replies, folded edits and versions, ack with edits, seen/delivered and the read-receipts mask,
  retired-login refusal and same-account recovery), calendar 32/32, organizations 26/26. The groups
  suite needs the MLS library and runs in CI.
- Scratch containers on raijin were throwaway (`--rm`, own `/tmp/pp-r2-*` folders removed); no
  services or `/var/www` touched.

### Gated / follow-ups

- Strong sign-in for native machine approval needs this `pigeonpost-linux` client change (owner
  decision; not made): enable the standard flow for the public client, require PKCE `S256`, allow the
  native loopback redirect `http://127.0.0.1/*` with random ports, keep device authorization, explicit
  consent, scopes and mappers. Back up only that client's configuration; do not rerun
  `provision-oauth.py` (it replaces the client). Then add `pigeonpost-linux` to
  `STRONG_SIGNIN_CLIENTS` and qualify the browser flow.
- Calendar reminders run only while the app is open and for the selected mailbox; operated agents'
  inboxes are listed but not opened natively in this round.
