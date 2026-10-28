# Windows native round three progress (1.0.11)

PR #145, branch `claude/desktop-round3-20261027`. Scope: Phase 6 live streams in groups, Phase 7 meeting notes, the composer-focus memory TODO and 1.0.11 packaging. Linux round three is PR #144 (`docs/linux-round3-2026-10-27-progress.md`).

## Built

- **Live streams** (`Groups/GroupStreams.cs`, `GroupsModel`, `CallBridge.Streams.cs`, `MainWindow.Streams.cs`):
  - The group view polls `GET /v1/groups/{id}/streams` every 10 s and when a group opens. Answers for another group, inbox or login are dropped.
  - The LIVE banner shows "Design is live: Launch rehearsal" with Watch, or Open / Return for your own or watched stream. The streamer is named by the inbox's name rule, falling back to `by_handle`.
  - Go live (title, camera, screen) follows `stream_policy`: admins by default, never restricted members. Admins switch it in Group info. The `stream` notice reads "Jane streamed for 34 min · 18 viewers".
  - Watch and Go live run `PigeonpostNative.watchStream`/`goLive` in the WebView2 call host. The `stream_result` answer is bound to a fresh operation id, the page document and the login.
  - The native page has no group event stream, so while a stream is on screen the app reads the stream record from the postbox and hands stage changes (invited, taken off the stage) to `live.js` via `onGroupStream`. A stage invitation is also shown in the group view.
- **Meeting notes** (`Meetings/*`, `PostboxClient.Meetings.cs`, `MainWindow.MeetingNotes.cs`):
  - Each signed-in person gets a P-256 notes key on this computer. It is sealed with DPAPI in `%LOCALAPPDATA%\Pigeonpost\meeting-notes\<owner-hash>.key` and restored only for the same person and as a matching key pair.
  - The key is registered as "Windows app" while `meeting_summaries` is on. A `key_exists` conflict makes a new key.
  - The list, open (ECDH → HKDF-SHA256 → AES-GCM, as `meeting-intel/src/crypto.js` seals), Copy as text and Delete my copy are all here. A brief naming another meeting is refused, and the content is plain text with control and bidi characters removed.
  - Requests, results and the key reference belong to the login. Sign-out, another login or another person retires them.
- **Composer focus:** the group composer keeps the caret after Send or Ctrl+Enter, whatever the outcome. Fixed in passing: WinUI raises `TextChanged` asynchronously, so page refreshes overwrote text being typed and moved the caret. Only changes the model makes itself are written into the box now. The main composer and the in-app attachment viewer already shipped in 1.0.9 (#66).
- **1.0.11:** `Pigeonpost.Desktop.csproj` and `Package.appxmanifest`. `STORE.md` has the release notes and a meeting-audio privacy checklist. No new package capabilities.

## Tests (synthetic data only)

- **Core:** 482/482, 7 new. One opens a brief sealed by meeting-intel's own `sealBrief` (fixture `Fixtures/meeting-brief.json`). Others cover tampering, a wrong meeting, wrong key ids, stale stream answers across selection and login changes, bridge script encoding, key store binding, and isolation between logins and people.
- **`test-live-notes.ps1`** (UI-test build, standard Windows user): 21 checks covering the notes list, brief, copy, other-device and in-preparation states, delete, Go live, group composer focus with Ctrl+Enter and the Send button, the LIVE banner, the stage invitation, Watch, and who can go live.
- **`test-streams.ps1` + `call-harness/streams.mjs`:** LiveKit 1.13.10 (Windows zip, SHA-256 pinned), the stream stand-in postbox, the repository's call service and a headless Chrome peer. 16/16:
  - WebView2 decodes the encrypted camera with 0 decryption errors, as a viewer that publishes nothing.
  - The app forwards a stage invitation read from the postbox; nothing is published until it is accepted; then the app's camera reaches the streamer.
  - The app goes live through `GoLiveScript` and a browser decodes it (0 errors); stopping ends the stream.
  - Harness notes: the integrity-pinned LiveKit client is served from the committed blob, because the Windows checkout rewrites line endings. Media is offered on the runner's network address, because WebView2 does not use loopback-only candidates.
- The existing call (34 + 1 informational), calendar, approval, rooms, delivery, message-mode and notification suites all pass on the same head.

## Not done / follow-ups

- `host_key_changed` (a streamer reloading) reaches native viewers only through the calls service, not as a group event.
- No store submission; releases are batched. Real-device checks with a live account remain owner QA.
