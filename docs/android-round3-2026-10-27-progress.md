# Android round three (0.4.1) — progress

Branch `claude/android-round3-20261027`. Scope: group live streams (Phase 6) and meeting notes (Phase 7)
in the Android app, composer focus kept, release prep for 0.4.1 (13) without uploading to Play.

## Built

- **Live streams** — `core/groups/GroupStreams.kt` (wire models, `canGoLive`, banner and stage wording,
  refusal wording, `pageEvents`), `GroupStore` (`streams` state read with the open group's poll,
  `checkStream`, `setStreamPolicy`, `group.stream` events, the `stream` notice text), `Group.streamPolicy`.
  UI: LIVE banner with Watch / Open / Return and the stage note, Go live sheet (title, camera and
  microphone; no screen on Android), "Who can go live" in group info.
- **Call page bridge** — `CallHost.watchStream/goLive/stopStream/showStream/forwardStream/setStreamMuted`,
  `live` state from `PigeonpostLive.current()`; the view model rereads `GET /v1/streams/{id}` while a stream
  is shown and forwards stage invitations, removal from the stage and the end to `live.js`.
- **Audio focus and foreground service** — `calls/LiveMedia.kt`: media audio focus (loss mutes the stream's
  players, gain unmutes), `LiveStreamService` (`mediaPlayback`, plus `camera|microphone` for a streamer or
  speaker when held), started only from a tap, stopped with the stream; manifest permissions declared.
- **Meeting notes** — `core/meetings/` (models, `MeetingNotesCrypto` matching meeting-intel and
  `calls-summary.js`, `MeetingNotesStore`), `meetings/KeystoreNotesKey.kt` (Keystore ECDH on Android 12+,
  Keystore-wrapped PKCS#8 on 8–11; destroyed on sign-out), Meeting notes screen from Settings and the tablet
  sidebar.
- **Composer focus** — already kept on Android (read-only while sending); a group composer test was added.
- **Release prep** — 0.4.1 (13); `docs/android-play-data-safety.md`.

## Tests

- JVM: `GroupStreamsTest` (8), `MeetingNotesTest` (7, including a brief sealed by meeting-intel's own
  `sealBrief`), plus the existing core suite with real MLS.
- Emulator, phone (Pigeonpost_Android_API_34, 1080×2400) and tablet (Pigeonpost_Tablet_API_34,
  2560×1600), synthetic data only: `LiveStreamsUiTest`, `MeetingNotesUiTest`, `NotesKeystoreTest`,
  `GroupsUiTest#theGroupComposerKeepsFocusAfterSend`. Full suite: phone 157 run, 0 failures (4 skipped);
  tablet: all new tests pass; 15 existing tests written for the phone layout (compose menu, "Back to
  conversations", single-pane lists) fail on the tablet layout; none touch this change. CI runs the phone profile.
- `tests/e2e-streams/android-app.mjs` + `LiveStreamE2eTest`, phone and tablet: the app watched a real
  end-to-end encrypted stream through the real call page (local LiveKit, calls service, mock postbox,
  streamer in headless Chrome with a synthetic camera): encrypted camera decoded (640×360 phone,
  1280×720 tablet), 0 decryption errors, viewer role, foreground service running; the streamer's stage
  invitation reached the page through the app's postbox read; the end closed the page and stopped the
  service.
- `tests/e2e-meeting-summaries/native.mjs`, phone and tablet: consent prompt, capture indicator, pinned
  service key verified, own 440 Hz tone captured as encrypted Opus/WebM chunks and transcribed, brief
  wrapped for the app's notes key.

## Not done / follow-ups

- Screen sharing when going live: Android's WebView has no `getDisplayMedia`.
- No push for `meeting.brief`; the list refreshes when opened.
- Play upload is batched with the other native submissions; complete `docs/android-play-data-safety.md`
  and the foreground-service declaration then.
