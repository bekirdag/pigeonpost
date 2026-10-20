# Pigeonpost Desktop for Windows

Native C# / WinUI 3 inbox for Windows 11 on x64 and ARM64. Browser device authorization connects
an existing or newly registered Pigeonpost account. Refresh tokens are held by Windows Credential
Locker; access tokens and message history stay in memory. The app never executes received requests.

## Features

- Live mailbox and conversation selection, subject organization, search and unread/held indicators.
- Three-column desktop layout with conversations, subjects and messages visible by default. The
  Subjects column can be hidden or restored; narrow windows use inbox/conversation navigation without losing the current draft.
- Conversation initials, activity dates, unread counts and distinct needs-review badges.
- Integrated message composer with removable file previews, clipboard paste and native file drop.
- New conversations can open without a message or send a first message; failed sends retain the draft.
- Per-subject drafts, confirmed subject deletion, archive/restore and automatic refresh.
- Free first-mailbox creation; existing named mailboxes load from the same account.
- File sending (up to 25 MiB) with an optional message; empty captions send only the file. Transfers
  allow five minutes on slow connections and report upload failures separately from uncertain sends.
  Pictures, video, audio, PDF and plain-text files open in an in-app viewer with a Save button; every
  file also has a download button, and Save attachments is in the message context menu. HTML, SVG
  and other markup are never rendered, only saved.
- Voice and video calls from the conversation header, using the web inbox's call client in WebView2
  (see Calls below). The composer keeps focus after Send or Ctrl+Enter.
- Clickable sender names/addresses, sender and own-address clipboard actions, and full handle labels.
- Known sender, full/specific server-defined request permissions, and confirmed blocking that clears grants.
- Sender details close before switching to another owned mailbox. Find highlights words and retains its match across refresh.
- Account management and optional handle purchases through the web inbox; visible app version in the window.

Native Windows notifications arrive through the account event stream, with periodic refresh as a fallback. Thread badges, the taskbar overlay and notification-area icon show unread counts; the system indicators include all account mailboxes. Clicking an alert opens its original thread. Closing a signed-in window keeps the app running in the tray; choose Quit Pigeonpost from the tray menu to exit. Alerts follow Windows notification/Do Not Disturb settings. Notifications contain no message preview.

## Calls

The call buttons run the web inbox's own call client (`site-inbox/calls.js`) through
`https://inbox.pigeonpost.dev/native-call.html` in a dedicated WebView2 (user data under
`%LOCALAPPDATA%\Pigeonpost\CallsWebView2`). The page is loaded once after sign-in and stays loaded,
invisible and click-through while idle, so incoming calls ring while the app runs. A call covers the window; minimised, it becomes a 64px strip
above the inbox. The display stays awake during a call. The bridge is `native-call.js`: the page
posts `ready`/`mode`/`renew`/`toast`/`ring`/`ring_ended`/`call`; the app calls
`PigeonpostNative.configure/renewed/call/answer/decline/signOut` (`Pigeonpost.Core/CallBridge.cs`). Microphone, camera and autoplay are granted only to the call
page's origin; navigation elsewhere is blocked and links open in the default browser. Screen
sharing uses WebView2's `getDisplayMedia` and Windows' picker. The MSIX declares the `microphone`
and `webcam` device capabilities, so Windows privacy settings govern access.

### Incoming calls (Phase 1c, `docs/call-alerting.md`)

- Window in front (visible, not minimised, active): the page rings in the window (ring card and
  ringtone), as before; the page is configured with `nativeRing: false`.
- Window in the notification area, minimised or behind other windows: the page is configured with
  `nativeRing: true` and the app shows a Windows toast with `scenario="incomingCall"` (bottom right,
  stays until acted on, looping `Notification.Looping.Call`, Answer and Decline), titled with the
  caller's name from your conversations and their handle. Answer shows the window and calls
  `PigeonpostNative.answer(room, null)`; Decline calls `PigeonpostNative.decline(room, null)`. A ring
  already sounding in the window when it goes to the background gets a silent toast. The app
  reconfigures the page whenever the window moves between the two states.
- App not running: the postbox sends the same toast through WNS (launch arguments
  `action=answer|decline&room=…&ring=…&kind=…`). Answer starts the app, restores the saved session,
  opens the call page and answers with the ring token; Decline posts the ring token to
  `/calls/v1/rings/{room}/decline` without the page and leaves the app in the notification area.
  While the app runs, a WNS call toast is suppressed and re-shown by the app, so it never doubles.
- Toasts are tagged with the room's first 16 characters (WNS's tag limit) in group `calls`, the same
  tag and group the postbox uses, so the local and the WNS toast replace each other and the ring's
  end removes either. A toast is also removed 60 seconds after its ring started.
- After sign-in, on every start and daily, a packaged install creates its WNS channel
  (`PushNotificationChannelManager.CreatePushNotificationChannelForApplicationAsync`) and registers
  it with `POST /v1/devices {token: <channel URI>, platform: "wns", kind: "alert", os: "windows",
  identity}`; a changed channel replaces the old registration and sign-out removes it
  (`DELETE /v1/devices/{channel}`). Unpackaged and test builds skip this; every failure is logged
  and calls still ring while the app runs. The postbox only pushes when it has the Store app's WNS
  credentials (see STORE.md, "Incoming calls with the app closed").

Code: `Pigeonpost.Core/CallRing.cs` (ring parsing, launch arguments, toast XML, ring state,
ring-token decline; unit-tested in `Pigeonpost.Core.Tests/CallRingTests.cs`) and
`Pigeonpost.Desktop/CallAlerts.cs`.

No offline message database or rich Markdown rendering is included.
Unsent drafts are discarded on sign-out or app exit. Store availability is tracked in STORE.md;
unsigned build artifacts are for validation and are not public installers.

## Build and test

Install .NET 10, PowerShell 7, Visual Studio Windows app development prerequisites and Windows SDK
10.0.26100 or newer. The app targets Windows 11 (build 22000 or later).

```powershell
./apps/windows/build.ps1 -Architecture x64 -Package
./apps/windows/build.ps1 -Architecture ARM64 -Package
```

Each build runs the core tests, publishes a self-contained WinUI executable and uses MakeAppx to
validate an unsigned Store-upload MSIX. Packages contain the .NET and Windows App SDK runtimes.
Artifacts are isolated under `apps/windows/artifacts/<architecture>/<build-id>`.

Core tests also run on macOS/Linux:

```sh
dotnet run --project apps/windows/Pigeonpost.Core.Tests -c Release
```

Docdex run-tests currently assumes Cargo for this workspace; use the native .NET runner for these
tests. The dependency-free runner covers account custody, token rotation, cancellation, sign-out
races, wire contracts, bounded 401 retries, attachments, mailbox creation, contact policy, duplicate
messages, stale loads, subjects, drafts and uncertain sends.

`.github/workflows/windows-desktop.yml` builds both architectures. The x64 job also launches the
actual release app and drives a separate UI-test binary using Windows UI Automation. UI fixtures
are excluded from the release Core and Desktop assemblies.

`test-calls.ps1` (x64 job) proves calls end to end on the runner: `call-harness/drive.mjs` starts a
stand-in postbox, the repository's `calls-service` and the inbox site locally, joins a headless
browser as the other person (synthetic media), and launches the UI-test build with
`PIGEONPOST_CALLS_PAGE/TOKEN/MAILBOX/HANDLE/CALL/BROWSER_ARGS` pointing its WebView2 at the harness
with Chromium's fake devices. It checks an outgoing video call (audio and video both ways, full
window, strip, hang-up, screen sharing where the runner allows) and an incoming ring in the idle
view. Those variables exist only in Debug and UI-test builds, never in Store packages. The same
driver runs anywhere with `node apps/windows/call-harness/drive.mjs <repo> <out> --simulate`, with
a browser page standing in for the app. UI-test screenshots use demonstration
data and are uploaded separately from Store packages.

## Native controls

| Action | Control |
|---|---|
| Switch mailbox | Mailbox selector |
| Switch subject | Current subject selector in the conversation header |
| Show or hide subjects | Pin subjects button (desktop windows; shown by default) |
| Resize columns | Drag a visible separator |
| New conversation | Ctrl+N |
| Search senders | Ctrl+K |
| Find in current subject | Ctrl+F or search button; previous/next arrows; Esc closes search |
| Refresh | Ctrl+R |
| Send message | Ctrl+Enter or Send |
| Insert a new line | Enter |
| Stage files | Attachment button, paste, or drop; Send delivers files and text together |
| Open a received file | Click the file (pictures, video, audio, PDF, text); Save or Ctrl+S in the viewer; Esc closes |
| Save received files | Download button on the file, or message context menu → Save attachments |
| Voice or video call | Phone and camera buttons in the conversation header |
| Contact settings or archive | Conversation actions menu |
| Sender details | Click the conversation name or address |
| Copy peer address | Copy address beside the heading or conversation context menu |
| Delete subject | Subject actions → Delete subject, then confirm |
| Account / sign out | Account menu in the bottom bar |
| Compact inbox | Back to conversations or Ctrl+K |

## Authentication deployment

`provision-oauth.py` provisions only `pigeonpost-windows` in the existing production realm. It
copies native-app scopes and protocol mappers from `pigeonpost-mobile`, requires explicit consent,
and enables only public-client device authorization. Administrator credentials are environment-only;
an existing client is backed up to an owner-only file before changes. Provision and read back the
client before declaring sign-in ready. No administrator credential is bundled with the app.

The standard `HttpClientHandler` disables redirects before bearer tokens are used. Authentication
links must remain in the configured HTTPS issuer realm. Messages and account tokens are not logged.

## Package identity and brand

Publisher: Wodo Teknoloji A.Ş. Product: **Pigeonpost Desktop**, Store ID **9N0NWJ9L8XDP**.
Manifest identity comes from Partner Center. Microsoft signs accepted packages for Store delivery.
The app icon is copied from the current native Mac app artwork; the ICO is a PNG container using
the same pixels. Direct distribution would require a separately trusted signing identity.

Technical references: [WinUI](https://learn.microsoft.com/en-us/windows/apps/winui/winui3/),
[Credential Locker](https://learn.microsoft.com/en-us/windows/apps/develop/security/credential-locker),
[Store packages](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/upload-app-packages).
