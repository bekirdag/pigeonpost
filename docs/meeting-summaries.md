# Meeting summaries (Phase 7)

Consented meeting notes for Pigeonpost calls: SDS §14 (R-22), decision D-21. This is the contract for
clients and operators and the threat model. Feature flag: `meeting_summaries` (postbox; per account or
global). Runbook: `deploy/meeting-intel/README.md`.

## 1. What happens

1. **Request.** Only the host can ask: the person who started an ad-hoc, 1:1 or group call, or the host
   of a scheduled meeting. Guests and other members cannot. The calls service asks the postbox whether
   the host's account has `meeting_summaries`; when it does, the host sees a Notes control.
2. **Disclosure and consent.** Every participant, guests included, gets the same disclosure: processor,
   what is captured, audio handling, audience, retention, acoustic limits and how to stop. Capture runs
   only while *every person currently in the call* has explicitly agreed to the current disclosure.
   - A decline, a withdrawal ("Stop for me") or a newcomer pauses capture for everyone. Declining never
     removes anyone from the call. When the person who declined leaves, or changes their answer, capture
     resumes once everyone present has agreed.
   - A newcomer pauses capture in `addParticipant`, before they receive anyone's media or the `joined`
     event.
   - A yes counts only for the disclosure hash and consent version the person saw (`consent_stale`
     otherwise). A no is always accepted.
3. **Capture.** Each agreeing device records only its own microphone track: the processed track the
   call sends (after noise suppression), never a remote stream. It cuts ~10 s self-contained chunks
   (MediaRecorder; Opus in WebM where available, AAC in MP4 on WebKit), skips chunks that were muted
   throughout, and encrypts each chunk before upload.
4. **Transcription and summary.** meeting-intel on raijin decrypts chunks in memory, decodes them with
   ffmpeg through pipes, batches each speaker's audio into windows of up to ~28 s (cut at the quietest
   quarter second near the end), transcribes them with whisper.cpp and wipes the PCM. When notes stop
   (host stops, host leaves, call ends, 30 min idle, 4 h cap) it asks the local model for the brief,
   validates it, encrypts it to the attendees' notes keys and stores it in the postbox. Then it drops the
   session: keys, transcript and any audio.
5. **Results.** Members who agreed see the brief under **Meeting notes** in the inbox: summary,
   decisions, action items, open questions and the full transcript, decrypted on a device that holds one
   of their notes keys. Guests can agree but receive no copy.

## 2. Components and interfaces

| Component | Where | Role |
|---|---|---|
| `site-inbox/calls-summary.js` | every call page (inbox, join, meeting, native call hosts) | Indicator, consent sheet, own-mic capture, encryption, upload; device notes key in IndexedDB |
| `site-inbox/meeting-notes.js` | inbox | Meeting notes reader; registers this browser's notes key |
| `calls-service/src/summaries.js` | raijin `pigeonpost-calls` | Host check, consent state machine, capture credentials, relays ciphertext |
| `meeting-intel/` | raijin `pigeonpost-meeting-intel` (127.0.0.1:8797) | Decrypt, STT, summary, encrypt brief |
| whisper.cpp server | raijin `pigeonpost-whisper` (127.0.0.1:8798) | Speech-to-text on CPU |
| local model | raijin 127.0.0.1:11452 (`qwen-latest-flash-next`) | Summary; OpenAI-compatible, no cloud fallback |
| `crates/pigeonpost-postbox/src/meetings.rs` | postbox | Eligibility, consent records, notes keys, ciphertext store, attendee-only reads |

### Calls service (`/calls/v1`, member or guest credential)

| Method | Path | Body | Notes |
|---|---|---|---|
| POST | `/rooms/{room}/summary` | `{session, enabled}` | Host only (`host_only`); `notes_disabled` when the postbox says no; `summary_active`; `summary_unavailable`/`summary_busy` (503) when meeting-intel is down or full |
| POST | `/rooms/{room}/summary/consent` | `{session, consent, version, disclosure_hash, notes_key?}` | `consent_stale` (409, carries `version`); `notes_key` = `{key_id, public_key, label}` from members only |
| POST | `/rooms/{room}/summary/audio?session=&seq=` | encrypted envelope (JSON, ≤192 KiB), header `x-capture-token` | `capture_paused` (409), `consent_required`, `bad_chunk`, `summary_busy` (503); duplicates answer `{accepted, duplicate:true}` |

SSE event `summary`, per participant: `{room, state: off|waiting|active|stopped, meeting, version, host,
disclosure, disclosure_hash, retention_days, service_key{session,key,key_id,signature,expires_at},
waiting_for, agreed[], reason, you{participant, consent, can_control, can_request}, capture{token,
version}|null}`. Others' answers are not revealed: a decline and an unanswered prompt both show as
"waiting". The SDS API names (`/v1/meetings/{room}/notes|consent|audio`) map to these calls-service
routes; `/v1/meetings/{id}` in the postbox serves results.

### Postbox

Member (`apiKey` or member session; not mailbox tokens or connectors): `GET /v1/meetings`,
`GET|DELETE /v1/meetings/{id}`, `GET /v1/meetings/keys`, `PUT|DELETE /v1/meetings/keys/{key_id}`.
Internal: `POST /internal/v1/meetings` (calls; `check_only` for eligibility), `…/{id}/attendees`,
`…/{id}/end` (calls); `GET …/{id}/recipients`, `POST …/{id}/brief` (meeting_intel). Account event
`meeting.brief {meeting_id, state}` to consenting attendees. Tables: `meetings`, `meeting_attendees`,
`meeting_notes_keys`, `meeting_brief_keys`. Audit actions (metadata only): `meeting.notes_started`,
`meeting.consent`, `meeting.notes_ended`, `meeting.brief_stored|brief_failed|brief_removed`,
`meeting.notes_key_registered|notes_key_deleted`.

## 3. Cryptography

All primitives are available in WebCrypto on every engine the call pages run in and in Node:
P-256 ECDH and ECDSA (IEEE P1363 signatures), HKDF-SHA-256, AES-256-GCM (ciphertext ‖ 16-byte tag).
Binary values are standard base64; P-256 public keys are 65-byte uncompressed points.

- **Service key.** meeting-intel holds a long-term P-256 signing key (`/etc/pigeonpost-meeting-intel/signing.pem`,
  root-only, handed to the service with systemd `LoadCredential`). Its public key is pinned in
  `calls-summary.js`. For each meeting it makes a fresh ECDH key and signs
  `"pigeonpost-meeting-intel-v1\n" + session + "\n" + meeting + "\n" + key + "\n" + expires_at`. Browsers
  refuse a key whose signature does not verify against the pinned key, or that has expired.
- **Audio.** Per consent interval the browser makes an ephemeral ECDH key, derives
  `HKDF(ECDH(eph, service), salt = session, info = "pigeonpost meeting audio v1|" + participant)` and
  seals each chunk with a random 96-bit IV and AAD `session|participant|seq|codec|t|dur`. The calls
  service asserts the participant from the authenticated session, so a chunk sealed for another
  participant, sequence number or session fails to decrypt.
- **Brief.** One random 256-bit content key encrypts the brief JSON (AAD = meeting id). For every notes
  key of every consenting attendee: a fresh ECDH key, `HKDF(ECDH, salt = meeting, info =
  "pigeonpost meeting notes v1|" + key_id)`, AES-GCM wrap of the content key with AAD `meeting|key_id`.
- **Notes keys.** Each browser or native call host keeps one non-extractable P-256 private key in
  IndexedDB. Its public half goes with a member's Agree (registered for the account through the calls
  service) and is registered by the inbox while the feature is on. At most 10 per account; the oldest is
  dropped. A key id is never rebound to a different key. WebKit in app web views (WKWebView) cannot
  store a `CryptoKey` in IndexedDB (`DataCloneError`); there the private key is kept as PKCS#8 bytes in
  that origin's storage on the device and re-imported as non-extractable when used. That is weaker at
  rest than a non-extractable key (anyone who can read the app's web storage on the device can read it).

## 4. Threat model

Assets: call audio, transcripts, briefs; who attended and consented. Trust boundaries: browsers ↔ calls
service (raijin) ↔ meeting-intel (raijin, loopback) ↔ postbox (45.132.181.100, internal API over the
existing SSH tunnel).

| Threat | Mitigation | Residual |
|---|---|---|
| Covert capture | Capture only while every present participant agreed to the current disclosure; persistent indicator; consent bound to disclosure hash and version; new participants and withdrawals pause everyone; capture credentials bound to meeting, participant and version | A participant can record their own screen or speakers with other tools; out of scope |
| Capturing a decliner's voice | No device captures another participant's stream; a single decline pauses all capture | Acoustic leakage: one microphone can pick up a nearby voice or loudspeaker. Disclosed, with headphone advice; speaker labels are per device, not voice recognition |
| Calls service reading audio | Chunks are encrypted to meeting-intel's per-meeting key, signed by a key pinned in the client; the calls service never holds a private key | A compromised raijin host (root) can read meeting-intel's memory; meeting-intel is a consented processor by design (SDS §3.6) |
| Calls service substituting the service key | Signature check against the pinned key | Whoever controls the static site could ship a different pinned key; same trust as the call page itself |
| Forged or replayed uploads | Capture token (HMAC, per process) + participant-bound AAD + per-participant monotonic `seq`; uploads refused unless the meeting is active and the participant's current answer is yes | Tokens are invalidated by any version change, so in-flight chunks after a pause are dropped by design |
| Results reaching the wrong people | Postbox records consent from the calls service; meeting-intel can wrap only for consenting attendees' registered keys (`not_a_recipient`); reads require consented, unremoved attendance and an unexpired meeting, and look the same as "not found" otherwise | The postbox lists notes keys and could in principle add one (the same custodial-attestation limit as other device keys, §3.6); listed under `GET /v1/meetings/keys` |
| Server-side plaintext | Audio held in memory only, zeroed after decode and transcription; transcript and keys dropped with the session; postbox stores ciphertext only; logs carry ids and counts | JavaScript strings cannot be zeroed; they are released with the session. A crash loses the meeting (attendees see `timed_out` after 26 h) |
| Prompt injection via speech | Transcript passed as delimited data, model told to ignore instructions in it; output validated field by field (owners must be speaker labels, timestamps must exist, lengths bounded) | A speaker can still say misleading things; the notes say they are automatic |
| Cloud leakage | Local whisper.cpp and the local model only; no fallback route. LLM failure gives a marked draft (transcript only) | — |
| Denial of service | Bounded sessions (6), queued audio (1,200 s total, 600 s per meeting), chunk size, rate limits per participant and host; whisper and meeting-intel under CPU/memory quotas on a shared box; busy is a 503 the client retries a little then drops | Notes may be incomplete under load; the call itself is never affected |
| Retention creep | 90-day expiry (reaper), last attendee's removal deletes the brief, account erasure removes keys, wraps and attendance | Copies attendees make themselves |

## 5. Limits and measurements (raijin, 2026-10-27)

- STT: whisper.cpp v1.9.5, multilingual `ggml-small` (SHA-256 `1be3a9b2…987b`), Silero VAD v6.2.0, CPU only,
  6 threads under `CPUQuota=600%`. On raijin under its usual load (~45-55 load average on 48 threads)
  the synthetic three-speaker EN/TR fixture (3 × 49 s tracks) transcribed at a real-time factor of
  **0.22** (32.5 s for 147 s of audio). `large-v3-turbo-q5_0` measured 0.66 on the same box and was not
  chosen. Rough capacity: about one and a half continuously talking three-person meetings in real time;
  backlogs beyond the queue bound are refused, and meetings finish a little after they end.
- Summary: `qwen-latest-flash-next` on the box's local OpenAI-compatible server; ~5 s for a short meeting.
  It is shared (one request at a time); retries with backoff, then a marked draft.
- Languages: English and Turkish checked with synthetic voices; other languages are transcribed but not
  validated. Speaker labels come from whose device captured the audio.

## 6. Clients

Web, and the native apps through their web call hosts (`native-call.html` loads `calls-summary.js`).
Verified with synthetic media only: headless Chrome (inbox, native-call and join pages, production CSP;
`tests/e2e-meeting-summaries/run.mjs`), the iOS app's WKWebView on the iOS 18.3 simulator (AAC/MP4
chunks) and the Android app's WebView on an Android 14 emulator (WebView 113, Opus/WebM chunks)
(`tests/e2e-meeting-summaries/native.mjs`).
Native apps read notes in the inbox web app for now; a native reader is a follow-up. Guests on the join
page see the disclosure and can agree or decline. Linux opens calls in the browser.
