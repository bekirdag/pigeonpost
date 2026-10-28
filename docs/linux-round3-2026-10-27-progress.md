# Linux native round three — progress

Branch `claude/linux-round3-20261027` from main `d25eff5` (Phase 7). Linux desktop 1.0.11. No tag, no
release: releases are batched.

## Inputs

- Handoffs: `docs/planning/2026-10-21-big-features-HANDOFF.md`, the 27 October Codex → Claude handoff
  ("Preserve these constraints"), the big-features ledger (Phase 6 and Phase 7 sections and their native
  follow-ups), `docs/streaming.md`, `docs/meeting-summaries.md`, `site-inbox/groups.js`,
  `site-inbox/meeting-notes.js`, `site-inbox/calls-summary.js` and `meeting-intel/src/crypto.js`.
- Linux constraint: WebKitGTK has no `RTCPeerConnection` in any distribution or Flathub runtime, so
  watching, going live and joining the stage stay in the browser; the app shows state and hands off.

## Implemented

### Live streams in groups (`pigeonpost/streams.py`, `window_groups.py`)

- While a group is open and the account has `streaming` (with `groups`), the app asks
  `GET /v1/groups/{id}/streams` on opening the group and every 10 s (Linux has no group event stream).
  An answer is used only for the group and mailbox that asked; anything for another group, not live or
  malformed is ignored.
- LIVE banner: "Jane is live: Sprint demo" (or "You are live: …") with **Watch in browser** (**Open in
  browser** for the streamer). A stage invitation naming this mailbox adds "You're invited on stage —
  open the stream in your browser to accept"; an accepted speaker sees "You're on stage".
- **Go live in browser** in the group header only when nobody is live and the web inbox's rule allows it:
  an active, non-restricted member of an activated group, and an admin unless `stream_policy` is
  `everyone`. The postbox still decides (`stream_restricted`).
- Both buttons open `https://inbox.pigeonpost.dev/#group=<id>`. `site-inbox/groups.js` gained a small
  reader for that link: once a mailbox that is a member of the group is on screen it opens the group
  (where the web's own banner and Go live sheet are), then forgets the link; only an exact `grp_` id is
  accepted, the hash is cleared at once, and a mailbox that is not in the group gets "Switch to the
  mailbox that is in this group to open it." Until the inbox files are deployed, the inbox simply opens
  and the app's toast tells the person to choose the group.
- Group info → Settings: **Who can go live** (Only admins / Everyone) for owners and admins, saved with
  the other settings (`stream_policy`).
- The `stream` notice reads as the web words it: "Jane streamed for 34 min · 18 viewers".

### Meeting notes (`pigeonpost/meeting_notes.py`, `window_notes.py`)

- Sidebar **Meeting notes** while the account has `meeting_summaries`. Lists `GET /v1/meetings`; opens
  `GET /v1/meetings/{id}`; shows summary, decisions, action items, open questions, the transcript and
  the "transcribed automatically" footer, all as plain text (labels never parse markup); **Copy as text**;
  **Delete my copy** with a confirmation ("Other attendees keep theirs."). States and failure texts are
  the web reader's; a brief without a wrap for this computer says it was encrypted for other devices.
- This computer's notes key: one P-256 key pair per account, made on first use, its PKCS#8 private half
  sealed in the protected store (the same store as drafts and unconfirmed sends: sealed file, key in the
  desktop keyring, owner checked on every open). The public half is registered as "Linux app" with
  `PUT /v1/meetings/keys/{key_id}` once per login while the feature is on, so notes made afterwards can
  be opened here. Without a keyring the key lasts until the app closes, and the page says so.
- Decryption per `docs/meeting-summaries.md` §3: ECDH(P-256) → HKDF-SHA-256 (salt = meeting id, info =
  "pigeonpost meeting notes v1|" + key id) → AES-256-GCM unwrap (AAD "meeting|key_id") → AES-256-GCM
  of the brief (AAD meeting id). Python `cryptography`: a Debian dependency (`python3-cryptography`),
  bundled in the Flatpak from pinned PyPI wheels (cryptography 46.0.3 abi3, cffi 2.0.0, pycparser 2.23,
  SHA-256 in the manifest).
- Account binding: every request, key use and result runs under the account's login lease
  (`bound_work`); a sign-out or another sign-in drops unfinished work, and each list/brief view carries
  a view number so an older answer never replaces a newer screen. Sign-out deletes the key's
  registration from the account and removes the sealed key with the rest of the account's local work; a
  later sign-in makes a new key, which older notes were not wrapped for.

### Composer focus

- The group message box regains focus after Send or Enter on success, after an unconfirmed or failed
  send, and after saving an edit, unless the person has moved to another page or a dialog is open. The
  direct-message composer already did this (1.0.8, kept through round two).

### Version and packaging

- 1.0.11 in `pigeonpost/__init__.py` (Debian control and the about text read it), the metainfo release
  entry, the workflow's package names and draft release notes, and the README. `site/download.html`
  is unchanged until the batched release.

## Privacy checklist: meeting audio (Linux 1.0.11)

Linux has no store privacy label (direct `.deb` and Flatpak bundles; not on Flathub). Before the batched
release, and before any Flathub listing, confirm:

- [x] The Linux app never captures, records or uploads microphone audio. Calls and live streams open
  in the browser; consent and capture happen in the web call page (`calls-summary.js`), under the web
  inbox's disclosure.
- [x] The Flatpak does not request microphone, camera or PulseAudio/PipeWire access
  (`finish-args` unchanged: network, IPC, Wayland/X11, DRI, Secret Service).
- [x] What the app receives: encrypted briefs (summary, decisions, action items, open questions and the
  transcript of what attendees said) for meetings this account agreed to; decrypted only in memory on
  this computer, never written to disk in the clear, not cached between runs.
- [x] What the app stores: one notes private key per account, sealed (keyring-held key), removed at
  sign-out; its public half is registered with the account and deleted at sign-out.
- [x] What it sends: the public notes key with the label "Linux app"; list, open and delete requests.
  No transcript or summary content is ever sent by the app.
- [ ] Release notes and `site/download.html` mention Meeting notes and that transcription and
  summarisation happen on Pigeonpost's server for consenting attendees (`site/privacy.html#meeting-summaries`).
- [ ] If listed on Flathub later: metainfo content rating stays `social-chat intense`; add a privacy
  statement link (`https://pigeonpost.dev/privacy#meeting-summaries`).
