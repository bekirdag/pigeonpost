# App Store privacy labels: iPhone, iPad and Mac

Checklist for the App Privacy answers in App Store Connect, for Pigeonpost **1.1 (50)** and later.
iPhone, iPad and Mac share one app record (`dev.pigeonpost.inbox`), so one set of answers covers all
three. The labels must agree with `apps/ios/Pigeonpost/PrivacyInfo.xcprivacy` in the build and with
`https://pigeonpost.dev/privacy` (and `/app-privacy`). If one changes, change the others in the same
release: a label that disagrees with the manifest is a rejection.

New in 1.1: **meeting audio** (Phase 7, `docs/meeting-summaries.md`) and **live streams** (Phase 6,
`docs/streaming.md`).

## 1. Before answering

- [ ] Privacy Policy URL is `https://pigeonpost.dev/privacy`, and the live page has the "Meeting
      summaries" section (`site/privacy.html#meeting-summaries`) and the "Live streams in groups"
      section. Both were deployed with Phase 6/7 on 2026-10-27; check that they are still there.
- [ ] "Do you or your third-party partners use data for tracking?" **No.** There is no analytics
      or advertising identifier; `NSPrivacyTracking` is `false`. The app does include third-party
      libraries. Review their data handling with the app's own behavior; their presence alone
      does not mean tracking, and the manifest alone does not prove its absence. See
      [Apple's privacy guidance](https://developer.apple.com/app-store/app-privacy-details/).
- [ ] The manifest in the build being submitted declares every type in §2 (`AudioData` and
      `OtherUserContent` are already declared there; 1.1 changes no manifest entry).

## 2. Data types

Every type: **Linked to the user: Yes · Used for tracking: No · Purpose: App Functionality only.**

| Category | Type | Why it is collected |
| --- | --- | --- |
| Contact Info | Name | Account and handle names |
| Contact Info | the account's contact address (as declared in the manifest) | Sign-in, account notices |
| Identifiers | User ID | Account and mailbox ids |
| Identifiers | Device ID | Push tokens, group-encryption devices, meeting-notes keys (public halves) |
| Contacts | Contacts | Contacts a person saves in Pigeonpost |
| User Content | Other messages (text messages) | Messages, stored by the postbox |
| User Content | Photos or Videos | Attachments and profile pictures |
| User Content | **Audio Data** | **Meeting notes (new in 1.1, see §3)**; audio attachments |
| User Content | Other User Content | Calendar events, group content; **meeting summaries and transcripts (§3)** |
| User Content | Customer Support | Support conversations |
| Purchases | Purchase History | Handle subscriptions |
| Diagnostics | Other Diagnostic Data | Delivery and error records needed to run the service |
| Other Data | Other Data Types | Call and stream metadata (who, when, how long) |

Not collected: location, health and fitness, financial info, browsing history, search history,
sensitive info, usage data for analytics, crash data from a third party, gameplay content.

## 3. Meeting audio: how to answer, and the facts behind it

Answer **Audio Data → Collected → Linked to you: Yes → Tracking: No → App Functionality**. Do not
tick Analytics, Product Personalization, Developer's Advertising, Third-Party Advertising or Other
Purposes.

The facts the answer rests on (`docs/meeting-summaries.md`):

- **Consented.** Only the host of a call can ask for notes. Every participant, guests included, sees
  the disclosure and must tap Agree; capture runs only while *everyone present* has agreed to the
  current disclosure. A decline, "Stop for me" or a newcomer pauses capture for everyone.
- **Own microphone only.** Each agreeing device records only the microphone track it already sends
  in the call. No device records another participant's stream.
- **Transient audio.** Chunks are encrypted on the device to the notes service's per-meeting key,
  decrypted on Pigeonpost's server in memory only, transcribed with a self-hosted model and wiped;
  no audio is written to disk or kept after transcription. No third party (no cloud speech or AI
  service) receives it.
- **Results.** The summary and transcript are encrypted to the attendees' own device keys; the
  postbox stores ciphertext only, deleted after 90 days or when the last attendee deletes their copy.
  This is why "Other User Content" stays ticked.
- **Linked, not tracking.** Speaker labels are the attendees' Pigeonpost handles, so the data is
  linked to the user; it is never used to track anyone across apps or sites and never shared with a
  data broker.

Live streams need no new type: frames are encrypted end to end in the streaming device and are not
recorded (D-23); the server keeps only metadata (title, who watched, how long), covered by "Other
Data Types" and "Other User Content".

## 4. Review notes to paste (App Review Information → Notes)

> Meeting notes are optional and consent-based: only the call's host can request them, every
> participant must explicitly agree, and each device records only its own microphone while everyone
> present has agreed. Audio is encrypted on the device, transcribed on our own server in memory and
> discarded; summaries are encrypted to the attendees' devices. Live streams in groups are end-to-end
> encrypted and not recorded. The microphone and camera usage strings describe calls, live streams
> and meeting notes.

## 5. Check after saving

- [ ] The product page preview in App Store Connect shows "Data Linked to You" with Audio Data
      listed, and **no** "Data Used to Track You" section.
- [ ] `apps/ios/STORE-LISTING.md` (App Privacy) points here.
