# Pigeonpost Desktop for Windows

Native C# / WinUI 3 inbox for Windows 11 on x64 and ARM64. Browser device authorization connects
an existing or newly registered Pigeonpost account. Refresh tokens are held by Windows Credential
Locker; access tokens and message history stay in memory. The app never executes received requests.

## Features

- Live mailbox and conversation selection, subject organization, search and unread/held indicators.
- New conversations can open without a message or send a first message; failed sends retain the draft.
- Per-subject drafts, confirmed subject deletion, archive/restore and automatic refresh.
- Free first-mailbox creation; existing named mailboxes load from the same account.
- File sending (up to 25 MiB), explicit Save attachments from the message context menu.
- Clickable sender names/addresses, sender and own-address clipboard actions, and full handle labels.
- Known sender, full/specific server-defined request permissions, and confirmed blocking that clears grants.
- Sender details close before switching to another owned mailbox. Find highlights words and retains its match across refresh.
- Account management and optional handle purchases through the web inbox; visible app version in the window.

No offline message database, tray, push notifications or rich Markdown rendering is included.
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
are excluded from the release Core and Desktop assemblies. UI-test screenshots use demonstration
data and are uploaded separately from Store packages.

## Native controls

| Action | Control |
|---|---|
| Switch mailbox | Mailbox selector |
| Resize columns | Drag either separator |
| New conversation | Ctrl+N |
| Search senders | Ctrl+K |
| Find in current subject | Ctrl+F; previous/next arrows |
| Refresh | Ctrl+R |
| Send message | Ctrl+Enter or Send |
| Insert a new line | Enter |
| Send a file | Attach file |
| Save received files | Message context menu → Save attachments |
| Contact settings or archive | Conversation actions menu |
| Sender details | Click the conversation name or address |
| Copy peer address | Copy address beside the heading or conversation context menu |
| Delete subject | Delete subject, then confirm |
| Account / sign out | Bottom bar |

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
