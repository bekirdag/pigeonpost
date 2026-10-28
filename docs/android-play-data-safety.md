# Google Play: Data safety and app content for Android 0.4.1

A checklist for the Play Console when Android 0.4.1 (versionCode 13) is submitted. Releases are batched,
so it is filled in with that submission, not before. The answers follow `site/privacy.html` (including
[Meeting notes](https://pigeonpost.dev/privacy.html#meeting-summaries) and "Live streams in groups")
and `docs/meeting-summaries.md`. Re-read both before submitting: the form must match the privacy policy.

## What changed since 0.4.0

| Feature | New data | Where it is described |
| --- | --- | --- |
| Meeting notes (Phase 7) | **Microphone audio**, only from people who agree, captured by their own device; transcribed and summarized on Pigeonpost's server; the audio is deleted once transcribed (never kept more than 24 hours). Encrypted notes and consent records for 90 days. | privacy.html#meeting-summaries, docs/meeting-summaries.md |
| Meeting notes reader | A device public key per device (the notes key), registered with the account. No new personal data. | docs/meeting-summaries.md §3 |
| Live streams (Phase 6) | Camera, microphone, stream title, who watched, viewer counts. Media is end-to-end encrypted and not recorded; metadata deleted 7 days after the stream. | privacy.html ("Live streams in groups"), docs/streaming.md |

## Data safety form

Keep every answer already given for 0.4.0 (account info, messages, contacts, photos for profile
pictures, app activity for calls, device IDs for push, crash data: none). Then:

### Audio → Voice or sound recordings

- [ ] **Collected: Yes.** Meeting notes send call audio off the device to Pigeonpost's server.
- [ ] **Shared: No.** It is processed only by Pigeonpost's own notes service on Pigeonpost's server (whisper.cpp and a local model). No third party, no cloud AI provider receives it.
- [ ] **Processed ephemerally: Yes.** The audio is decrypted in memory, transcribed and discarded; it is never written to disk and never kept longer than 24 hours. (The text result is covered below.)
- [ ] **Required or optional: Optional.** Nothing is captured unless the person chooses Agree in the call; declining never removes anyone from the call.
- [ ] **Purposes: App functionality** only (the meeting notes the person asked for). Not analytics, advertising, personalization or fraud prevention.
- [ ] Only the person's **own microphone**, on their own device: no device records anyone else's audio, and remote participants' streams are never captured.
- [ ] Call audio and video themselves (not for notes) are end-to-end encrypted between participants and are not collected; Google's definition excludes end-to-end encrypted data from "collected".

### Messages → Other in-app messages (meeting summaries and transcripts)

- [ ] **Collected: Yes.** The transcript and summary are made in plain text on Pigeonpost's server, then encrypted for the attendees' devices; the server keeps only that encrypted copy. They are not end-to-end encrypted in Google's sense, so declare them.
- [ ] **Shared: No.** **Optional.** **Purpose: App functionality.**
- [ ] Retention: 90 days, or until each attendee deletes their copy (in the app: Meeting notes → Delete my copy).

### Photos and videos / Audio — live streams

- [ ] Live streams are end-to-end encrypted on the streamer's device and not recorded: **not collected**. Keep the existing call answers.
- [ ] Stream metadata (title, who watched, counts) is app activity tied to the account: **Collected, not shared, optional, app functionality**; deleted 7 days after the stream ends.

### Security practices

- [ ] **Data is encrypted in transit: Yes.**
- [ ] **You can request that data be deleted: Yes** (account deletion; Delete my copy for notes).
- [ ] Do not claim an independent security review.

## App content and permissions

- [ ] **Foreground service permissions declaration** (Play Console → App content → Foreground service permissions). 0.4.1 declares `FOREGROUND_SERVICE_MEDIA_PLAYBACK`, `FOREGROUND_SERVICE_CAMERA` and `FOREGROUND_SERVICE_MICROPHONE` for `LiveStreamService`:
  - `mediaPlayback`: continues playing a group's live stream the person chose to watch when they switch apps.
  - `camera` / `microphone`: continues sending the streamer's (or an accepted stage speaker's) camera and microphone while they switch apps during their own live stream.
  - Started only by a tap on Watch or Go live; shows an ongoing notification with Leave/End stream; stops when the stream ends. Provide a short video of watching a stream with the notification visible.
- [ ] `RECORD_AUDIO` and `CAMERA` are unchanged in kind (calls), now also used for going live and for meeting notes capture inside a call.
- [ ] Privacy policy URL unchanged: https://pigeonpost.dev/privacy.html (already describes meeting notes and live streams).
- [ ] Release notes (en-US): "Watch and start live streams in your groups. Read meeting notes from calls where you agreed to them, decrypted on your phone."

## Before uploading

- [ ] The AAB is the one in `~/Documents/releases/pigeonpost/android-0.4.1/` (check `SHA256SUMS`), signed with the upload key.
- [ ] Batch with the other native submissions; Apple waits for the owner's review.
