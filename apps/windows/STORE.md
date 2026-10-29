# Microsoft Store release

Product: **Pigeonpost Desktop** (the shorter Pigeonpost name was unavailable).

- Store ID: `9N0NWJ9L8XDP`
- Identity name: `WodoTeknolojiA.PigeonpostDesktop`
- Publisher: `CN=D850EB9E-D10B-4265-83A3-AC88170D8C6D`
- Publisher display name: `Wodo Teknoloji A.Ş.`
- Package family: `WodoTeknolojiA.PigeonpostDesktop_80zy2mk5xaxza`
- First submission: `1152921505702010327`
- Current public package: `1.0.6.0`; update in certification: `1.0.7.0`; prepared: `1.0.8.0` (design refresh) and
  `1.0.9.0` (calls, attachment viewer), Windows 11, x64 and ARM64. 1.0.9.0 contains everything in 1.0.8.0: if 1.0.8.0 was
  never uploaded, submit 1.0.9.0 alone; if it was, 1.0.9.0 replaces it in a new submission.

## Listing copy

**Short description:** Your people and agents, together in a native Windows inbox.

**Description:**

Pigeonpost Desktop brings your conversations with people and agents to Windows. Sign in with your
Pigeonpost account, switch between your mailboxes, and keep each conversation organized by subject.

Search conversations, find text inside a subject, send messages and files, and keep track of unread
messages and requests needing review. Call anyone you talk with, by voice or with video, straight from
the conversation, and keep reading your messages with the call minimised at the top of the window.
Pictures, videos, audio, PDFs and text files open inside the app, ready to save. The Subjects column is visible by default alongside conversations and messages. Switch subjects
from the column or conversation header, and hide or restore the column when needed. The layout
adapts to smaller windows with simple back navigation.

Paste or drop files into the composer, review the attachments, and send them together with your message.
Archive conversations when you are finished and restore them whenever you need them. Use keyboard
shortcuts to search, start a conversation and send a message quickly.

Sign-in takes place in your browser. The app keeps your renewable session in Windows Credential
Locker. A free account and an internet connection are required. You can create a free mailbox inside
the app. Optional custom handles and billing are managed on the Pigeonpost website; no purchase is
needed for basic messaging.

The app displays messages and server-reported request permissions. It does not execute agent requests.

**Features:**

- Native Windows inbox with conversations, subjects and messages visible together by default.
- Access your existing Pigeonpost mailboxes and create a first free mailbox.
- Send text and staged files together, with clipboard paste, drag and drop, and attachment previews.
- Voice and video calls, end-to-end encrypted, with screen sharing; incoming calls ring while the app runs.
- Open pictures, videos, audio, PDFs and text files in the app and save them with one click.
- Search senders and find messages inside a subject.
- Archive conversations, change contact names and block senders.
- Browser sign-in and secure Windows credential storage.

Privacy: https://pigeonpost.dev/privacy

Support / website: https://pigeonpost.dev

## Release gates and current status

On October 16, 2026, Partner Center confirms **1.0.6.0** publicly released in Submission 7
(modified October 14). **1.0.7.0** is submitted as **Submission 8** (`1152921505702147106`)
and shows **In certification**, with pre-processing in progress. Both x64 and ARM64 packages
validated, and publication starts automatically after certification. The update is not yet confirmed live.

Windows workflow `38870868381`, source `6a57b1f`, passed core behavior, native x64/ARM64 builds,
MSIX installation, UI smoke and ten notification integration checks. The hosted test account is
different from Explorer's account, so the taskbar overlay API is denied there; this is handled without
disabling the tray or native alerts. Overlay display in a normal same-user Explorer session was not
visually tested. Evidence and package hashes are recorded in
`docs/planning/2026-10-16-desktop-notifications-progress.md`.

### Version 1.0.12 release notes

Incoming calls can now ring on this PC while Pigeonpost Desktop is closed: Answer opens the call,
Decline declines it without opening the app.

1.0.12 adds no capabilities. The manifest gains a second COM server (Windows App SDK push, class ID
= the Entra application ID) next to the app-notification activator. Not submitted yet: 1.0.11 is in
certification; submit 1.0.12 after it. Suggested certification note: "Incoming calls use Windows push
notifications (Windows App SDK) to show an incoming-call notification while the app is closed. No
new capabilities."

### Version 1.0.11 release notes

Live streams in groups: a red LIVE banner shows who is streaming, Watch opens the stream full screen,
and members the group allows can go live with their camera or screen. The person streaming can invite
viewers onto the stage; nothing turns on until you join. Group admins choose who may go live. New
Meeting notes screen: summaries, decisions, action items and transcripts of calls where everyone
agreed to notes, decrypted on this computer, with Copy as text and Delete my copy. The group message
box keeps the cursor after you send.

1.0.11 adds no package capabilities: streams and meeting-notes capture run in the existing WebView2
call host with the 1.0.9 `microphone` and `webcam` capabilities. Not submitted yet (releases are
batched).

### Privacy checklist: meeting audio (before submitting 1.0.11)

Meeting notes (docs/meeting-summaries.md) send microphone audio off the device when, and only while,
everyone in a call agreed. Before this submission:

- [ ] Partner Center > Properties > Privacy policy URL is https://pigeonpost.dev/privacy, which has the
  "Meeting notes" section (`#meeting-summaries`): what is captured (each consenting person's own
  microphone, in ~10 s encrypted chunks), the processor (Pigeonpost's own server; local speech-to-text
  and summary model, no third party), retention (audio held in memory only and discarded; brief and
  transcript stored encrypted for attendees, deleted after 90 days or when the last attendee deletes
  theirs), and how to decline or stop ("Stop for me").
- [ ] The listing's description and "What's new" mention meeting notes as opt-in, consent-gated and
  host-requested; never "recording".
- [ ] Certification note: "Meeting notes: a call's host can ask for notes; every participant sees a
  disclosure and must agree before any audio is captured. Audio is encrypted on the device to the
  notes service. Notes appear under Meeting notes, decrypted on the device. Test accounts: two
  Pigeonpost accounts with the meeting_summaries feature on."
- [ ] Age rating questionnaire unchanged (no user-generated public content is added; streams reach
  only group members).
- [ ] Microphone/camera consent text (Settings > Privacy & security) still names calls; streams and
  notes use the same capabilities, and Windows asks once.
- [ ] The device's notes key is stored only on this computer, sealed with Windows data protection for
  the signed-in Windows user and bound to the Pigeonpost account it was made for
  (`%LOCALAPPDATA%\Pigeonpost\meeting-notes`); no notes content is stored locally.

### Version 1.0.10 release notes

End-to-end encrypted groups: start a group, invite people, chat and share files that only members'
devices can read, check device fingerprints and the group safety number, set disappearing messages
and call the whole group. New Calls page with missed calls, a Filtered list for mail from strangers,
Privacy and filtering settings, blocking from any conversation, call or message, and profile
pictures.

1.0.10 ships `pigeonpost_groupcrypto_ffi.dll` (the group encryption library, built from
`crates/pigeonpost-groupcrypto`) inside each architecture's MSIX. No new package capabilities.

### Version 1.0.9 release notes

Voice and video calls: call anyone from the conversation header, answer incoming calls while the app
is running, share your screen, and minimise a call to a strip at the top while you keep reading.
Pictures, videos, audio, PDFs and text files now open inside the app, with a Save button. After you
send a message, the cursor stays in the message box for the next one.

Store screenshots for 1.0.9 (1426x893, demonstration data from the UI-test build, no real messages):
`Packaging/store-screenshots/` — inbox, incoming call over the inbox, a call minimised while reading,
a voice call, and the attachment viewer. Suggested captions: "Your people and agents in one inbox",
"Answer calls without leaving your messages", "Keep reading with the call at the top",
"Voice and video calls, end-to-end encrypted", "Open PDFs, pictures and videos in the app".

### Calls: capabilities and certification notes

The 1.0.9 package declares the `microphone` and `webcam` device capabilities (in addition to
`runFullTrust`). Windows asks once and lists Pigeonpost Desktop under Settings > Privacy & security >
Microphone and Camera. Calls run in Microsoft Edge WebView2 against https://inbox.pigeonpost.dev; audio
and video travel peer to peer, encrypted (DTLS-SRTP), and are never recorded or stored. Store policy
(10.5) requires the privacy policy to cover microphone and camera use: https://pigeonpost.dev/privacy
must say that calls use them only while a call is active. As of October 19, 2026 that page does not
mention the microphone or camera (only site/app-privacy.html does): update it, or point the listing's
privacy URL at a page that covers calls, before submitting 1.0.9. Suggested certification note: "Calls: open a
conversation, press the phone or camera icon in the header; the other side needs a second Pigeonpost
account (web inbox at https://inbox.pigeonpost.dev). Windows will ask for microphone/camera access."

### Incoming calls with the app closed (WNS)

Since 1.0.12 the app uses Windows App SDK push (`Pigeonpost.Desktop/WindowsPush.cs`). Microsoft
refuses the older package-SID credentials from Partner Center's WNS/MPNS page ("Client credential
flows against login.live.com are no longer supported"), and Windows App SDK push is not configured
in Partner Center at all. The pieces:

1. Entra app registration **Pigeonpost Desktop WNS** in the Wodo Network tenant
   (`48477bf1-ca44-4687-a97c-3d39d7ef7572`), accounts in any organizational directory, as
   Microsoft's push quickstart requires. Application ID `6b717f08-17c7-4433-8ea8-25f06ed63e60`
   (the manifest's push COM class and `WnsChannel.AzureAppId`); service principal object ID
   `d4a94855-b3f5-41af-8b48-c66d4d7eef73` (the channel request's remote ID,
   `WnsChannel.AzureObjectId`). Client secret `pigeonpost-postbox-wns`, valid until 2028-10-28,
   lives only in the postbox's env.
2. Microsoft's quickstart says a packaged app's package family name must be mapped to that
   application by request (Win_App_SDK_Push@microsoft.com, subject "Windows App SDK Push
   Notifications Mapping Request", body `PFN: WodoTeknolojiA.PigeonpostDesktop_80zy2mk5xaxza`,
   `AppId: 6b717f08-17c7-4433-8ea8-25f06ed63e60`, `ObjectId:
   d4a94855-b3f5-41af-8b48-c66d4d7eef73`; processed weekly). On 2026-10-29 no request had been
   sent, yet a CI install of this package got a channel and received a raw push from the postbox's
   credentials (`200 received`, logged by the app). Send the request only if channels start
   failing ("Push channel unavailable" in the desktop log).
3. The postbox has `PIGEONPOST_WNS_TENANT_ID`, `PIGEONPOST_WNS_CLIENT_ID` and
   `PIGEONPOST_WNS_CLIENT_SECRET` (`deploy/postbox/README.md`).
4. `CALLS_RING_PUSH=1` in the calls service and the `call_push` flag (`docs/call-alerting.md`). Then
   sign in on a Windows PC with the Store build, quit it from the notification-area icon, and call
   that account: the incoming-call toast should appear bottom right.

The mapping is by package family name, so the Store build and a sideloaded package with the same
identity both get channels; ZIP and fixture builds ring only while running. The Windows CI install
test logs the channel result to `installed/push.log`.

### Version 1.0.8 release notes

A refreshed look: new typography, message cards, conversation rows, composer and dialogs, in both
dark and light themes. Select any part of a message and copy it; right-clicking selected text now
offers Copy, while right-clicking elsewhere on a message keeps the message menu.

### Version 1.0.7 release notes

Unread counts appear on each subject/thread. Reading one subject preserves unread messages in
other subjects. Windows notifications open the matching conversation and subject. Taskbar and tray
icons show the unread count across account mailboxes. Closing the signed-in window keeps the app
running in the tray; choose Quit Pigeonpost from its icon menu to stop it. Alerts follow Windows
notification settings. Notification text does not reveal message contents.

### Version 1.0.6 release notes

The Subjects/Threads column is back by default, alongside conversations and messages. It remains
visible when you resize an ordinary desktop window, and you can hide or restore it with the pin
button. Small windows retain compact navigation, and switching subjects or resizing preserves drafts.

### Version 1.0.5 release notes

A clearer conversation layout, with a subject switcher and an optional pinned Subjects pane.
The app now adapts to narrow windows. Conversation rows show initials, activity dates, unread counts
and needs-review badges. Search opens when you need it, account actions are grouped in a menu, and
the message composer brings text and removable attachments together.

### Initial release history (September 30)

- 41 core behavior tests pass. Native x64/ARM64 compilation and MakeAppx validation pass.
- Actions run `36713266647` passed native release launch, test-only UI automation, Credential Locker round-trip, MSIX installation/activation and external public-page checks.
- Both packages from run `36712576890` are uploaded and validated by Partner Center.
- Legal Info confirms Active / Authorized / DSA Compliant; the earlier verification warning has cleared. Seller ID: `96339540`.
- Free worldwide pricing, properties, age ratings and the English listing with three captioned 1426x893 native screenshots are saved.
- Certification instructions and the existing review-account credential are stored in Partner Center's separate private fields.
- `runFullTrust` justification and publish-after-certification option are saved. The justification has a 500-character limit.
- Production `pigeonpost-windows` public OAuth client is provisioned with device authorization, explicit consent and mobile-equivalent scopes/mappers. Live review-account authorization, mailbox/inbox loading, refresh, restore and sign-out passed.
- The website Windows card says In Microsoft Store review and retains the working web inbox until the Store listing can install the app. Prepared final Store URL: https://apps.microsoft.com/detail/9N0NWJ9L8XDP.

Store packages are unsigned upload artifacts. Microsoft signs packages after acceptance; do not
publish these MSIX or ZIP files as end-user installers. Test-only fixture builds are separate from
the release package. No reviewer passwords, tokens or administrative credentials belong in this file.
