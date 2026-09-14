# Pigeonpost for Windows

A native C# / WinUI 3 desktop client, using the macOS app as the layout and behavior reference.
The first milestone is an interactive **local preview** and a tested core/API foundation.

The preview has two sample mailboxes, resizable conversation/subject/message columns, sender
search, in-subject find, held/unread indicators, drafts, local composing, new subjects and
conversations, and archive/restore. Sample changes are memory-only and reset on exit. It never
connects to the production postbox. The real `PostboxClient` is implemented and tested with fake
HTTP handlers, but it is not wired into the preview.

Live sign-in, Windows credential storage, live polling, attachment transfer, native Markdown,
complete settings/peer operations, notifications/tray and Store release are subsequent milestones.
Attachment metadata and plain text display are present. This is not a signed public release.

## Build on Windows

Requirements:

- Windows 11, x64 or ARM64.
- .NET 10 SDK. `global.json` allows current stable .NET 10 feature bands.
- Windows SDK 10.0.26100 or newer, including MakeAppx when packaging.
- WinUI development prerequisites; installing Visual Studio with Windows app development tooling
  is the easiest way to obtain the native build prerequisites. The script uses `dotnet publish`.
- PowerShell 7.

From the repository root:

```powershell
./apps/windows/build.ps1 -Architecture x64
./apps/windows/build.ps1 -Architecture ARM64
```

The script runs core tests and publishes a self-contained application with the .NET and Windows
App SDK runtimes. It prints the path to `Pigeonpost.exe` and a ZIP under
`apps/windows/artifacts/<architecture>/<build-id>/`. Run the executable on Windows to inspect the
preview. Build directories are fresh for every invocation and are excluded from Git.

For an unsigned development MSIX, run:

```powershell
./apps/windows/build.ps1 -Architecture x64 -Package
```

`MakeAppx` validates the package layout. The script scales the existing Pigeonpost artwork to
the package logo sizes, writes the architecture into the development manifest and builds an
unsigned package. An unsigned MSIX is a packaging artifact, not a publicly installable release.
For local installation testing use a development certificate that matches its Publisher, trusted
only on the test device. The actual Store identity and release packaging are a later milestone.

`Pigeonpost.sln` opens the core, desktop and test projects. The desktop is an unpackaged,
self-contained WinUI project for development; `build.ps1 -Package` wraps its published files in
MSIX. Visual Studio users should select the x64 or ARM64 platform.

## Tests on any .NET 10 host

```sh
cd apps/windows
dotnet run --project Pigeonpost.Core.Tests/Pigeonpost.Core.Tests.csproj -c Release
```

The dependency-free executable runner exits nonzero on failure. Tests cover Apple/web fixture
parity, identity normalization, duplicate messages/counts, legacy/default subjects, trust display,
10,000-message histories, preview interactions, draft isolation, stale mailbox loads, API query
and send shapes, bounded/coalesced 401 renewal, cancellation and uncertain sends. Test network
requests use in-process fake handlers and never contact a real mailbox.

Docdex's runner currently reports no detected test runner for this project, so use the command
above. Do not interpret a macOS core build or XML check as a Windows XAML/runtime validation.

## Native preview controls

| Action | Control |
|---|---|
| Switch inbox | Mailbox selector |
| Resize columns | Drag either separator |
| New conversation | Ctrl+N |
| Search senders | Ctrl+K |
| Find in current subject | Ctrl+F; previous/next arrows |
| Refresh sample inbox | Ctrl+R |
| Send local message | Ctrl+Enter or Send |
| Insert a new line | Enter |
| Copy text | Select message text; Ctrl+C |
| Copy original body | Message context menu |
| Archive/restore | Conversation actions menu and archive toggle |

Find navigates matches while retaining surrounding messages. Outgoing request envelopes display
their typed text. Received autonomy and held state come only from server fields; message bodies
are never executed.

## Layout and dependencies

```text
Pigeonpost.Core           Models, conversation assembly, API, preview service, inbox state
Pigeonpost.Desktop        WinUI XAML, window behavior and Windows clipboard adapter
Pigeonpost.Core.Tests     Behavioral assertions plus Apple wire fixtures
Packaging                Development MSIX manifest
build.ps1                Windows publish and optional unsigned MSIX
```

`Pigeonpost.Core` has no Windows/UI dependency and no external NuGet packages. The desktop pins
`Microsoft.WindowsAppSDK` 2.4.0 and `Microsoft.Windows.SDK.BuildTools` 10.0.26100.4654, following
Microsoft's current self-contained WinUI sample. The Windows App SDK version was checked against
NuGet's publication metadata (published 2026-08-13). UI rendering uses native controls and
virtualized `ListView` instances; there is no embedded web runtime.

Reference files in this repo:

- `apps/ios/PigeonpostMac/MacInboxView.swift` and `MacThreadView.swift`: layout and interactions.
- `apps/ios/Shared/Model/Conversation.swift`: conversation and subject behavior.
- `apps/ios/Shared/API/Models.swift` and `PostboxClient.swift`: wire contracts.
- `apps/ios/Tests/main.swift`: the source of the copied test fixtures.
- `site-inbox/favicon-180.png` and `favicon.ico`: reused brand assets.

The GitHub workflow `.github/workflows/windows-desktop.yml` runs core tests on Linux and compiles
and packages x64/ARM64 previews on Windows for pull requests or manual invocation. It uploads
development artifacts only; it performs no signing, Store submission or public release. The
workflow must run successfully before claiming the WinUI app builds on Windows.

## Store release path

Publisher: **Wodo Teknoloji A.Ş.**

The development identity `Wodo.Pigeonpost.Preview` / `CN=Pigeonpost Development` is temporary.
Before release, associate the app with the verified company account in Partner Center and copy
the exact Store Identity Name, Publisher and PublisherDisplayName. Final Store MSIX submission
is signed by Microsoft after certification. This workflow does not submit the preview to Store.

Microsoft Artifact Signing Public Trust did not list Türkiye as an eligible organization
jurisdiction when checked on 2026-09-11. Recheck eligibility at release time. A future direct
download needs a publicly trusted organizational signing identity, SHA-256 and trusted
timestamps; signing alone does not guarantee SmartScreen reputation.

The detailed local roadmap and evidence are in
`docs/planning/2026-09-11-windows-desktop-plan.md` and
`docs/planning/2026-09-11-windows-desktop-progress.md` (that directory is Git-ignored).

Sources: [WinUI](https://learn.microsoft.com/en-us/windows/apps/winui/winui3/),
[Microsoft self-contained sample](https://github.com/microsoft/WindowsAppSDK-Samples/tree/main/Samples/SelfContainedDeployment),
[signing options](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options),
[Artifact Signing eligibility](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart).
