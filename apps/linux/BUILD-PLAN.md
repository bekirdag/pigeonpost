# Pigeonpost Desktop for Linux — build plan

Date: 2026-09-13. Publisher: Wodo Teknoloji A.Ş.

## Outcome and stack

Ship a usable Linux desktop client with the macOS application's conversations → subjects → messages layout and the same postbox workflows. Use Python 3, PyGObject, GTK 4 and libadwaita: the controls, text rendering, windows, accessibility and input handling are native GTK widgets. Network and keyring work runs off the GTK main thread. There is no browser renderer or local Node service. Keep this application independent of the Rust CLI workspace in `apps/linux`.

Use the system browser for account authorization and handle checkout, libsecret/Secret Service for the refresh token, the desktop notification service, and native file dialogs. Follow the system light/dark preference. Minimum native dependencies: Python 3.10, GTK 4.8, libadwaita 1.2 and libsecret 0.20. Validate these floors against the packaged runtime.

Primary references: [GNOME libraries](https://developer.gnome.org/documentation/introduction/overview/libraries.html), [PyGObject](https://pygobject.gnome.org/tutorials/), [application IDs](https://developer.gnome.org/documentation/tutorials/application-id.html), [Flatpak bundles](https://docs.flatpak.org/en/latest/single-file-bundles.html). Python calls the native GTK libraries through introspection; it does not emulate Linux controls.

## macOS reference and scope

| Mac source | Linux behavior |
| --- | --- |
| `apps/ios/PigeonpostMac/MacRootView.swift` | Sign in, cancel, retry, restore session and explicitly create a first inbox |
| `MacInboxView.swift` | Resizable three-column inbox, mailbox selection, conversation search, unread counts, archive and restore |
| `MacThreadView.swift` | Subject selection, new subject, message search, composer, attachments and keyboard send |
| `Shared/Model/Conversation.swift` | Handle/address normalization, duplicate suppression, exact-contact precedence over wildcard policy |
| `Shared/Views/SettingsSheet.swift` | Contacts and server-defined autonomy permissions, quota, account links and sign out |
| `Shared/Views/BuyHandleSection.swift` | Native handles page with availability lookup, owned mailboxes and secure website purchase management |
| `Shared/API/PostboxClient.swift` | Existing production REST API, account bearer authentication and one refresh/retry on 401 |

Initial release must exchange real messages. Fixtures belong only in tests. Purchasing opens the existing production account page; the desktop never simulates a paid entitlement or asks for card details. Read the current website checkout contract before adding selection parameters. Native Linux does not use Apple or Google purchase receipts.

## Dependency order and implementation

1. **Contracts and packaging foundations.** Confirm server payloads, OAuth client and native dependencies. Add application ID `dev.pigeonpost.Desktop`, existing Pigeonpost artwork, desktop entry, AppStream metadata, installation script, Debian package and Flatpak manifest. Inspect Docdex impact graphs before integration edits; the new standalone app has no existing inbound dependencies.
2. **Domain and transport.** Implement testable conversation/subject assembly, strict peer validation, safe attachment names, API errors, bounded responses and authenticated requests. Never forward credentials on redirects. Include read and sent mail on every inbox poll. Retry reads after transient failures; never automatically repeat an ambiguous send or purchase.
3. **Account and credentials.** Use a dedicated public device-authorization client with explicit consent. Validate issuer verification URLs; respect pending, slow-down, expiration and cancellation. Persist refresh tokens only in Secret Service; never fall back to a plaintext file. Serialize refreshes, discard callbacks from a prior account/mailbox generation, and revoke/clear credentials on sign out.
4. **Native interface.** Build the three columns, blank/error/loading states, mailbox switcher, new conversation and subject dialogs, safe selectable message text, request status, copy/delete/report actions, file upload/save, search, archive and restore. Keep drafts and message bodies in memory. A send failure preserves the draft and explains that an uncertain delivery must be checked before retrying.
5. **Account pages.** Add quota, contacts (allow/block and review/auto with the server vocabulary), handle availability and browser checkout, account deletion link, privacy/terms, about and sign out. Prompt before destructive actions. A contact's name is not an autonomy grant.
6. **Validation.** Run meaningful unit tests for auth, HTTP errors, normalization, subject targeting, account isolation and attachment safety. Run actual GTK integration tests under Linux/Xvfb, inspect screenshots at narrow/wide sizes and both themes, exercise keyboard input, launch the installed Debian package and validate desktop/AppStream metadata. Test live OAuth and a private test-mailbox round trip without charging purchases or messaging other people.
7. **Release.** Build in GitHub Actions on Linux. Publish a versioned Debian installer and a Flatpak bundle with checksums after CI and installed-app checks pass. Flatpak must declare only network, display, notification and Secret Service access; files are selected through portals. Keep CLI releases separate and do not replace their latest-release pointer.
8. **Website and operations.** Only after assets exist, replace the Linux placeholder on `site/download.html`, retain one homepage Download link, auto-recommend Linux through the existing OS detector, show Mac/Windows/Linux icons and all other releases below, and document installation. Test links/OS cases, deploy with a fresh backup, then verify public hashes, server health and clean synchronized Git checkouts.

## Release gates and limits

- No enabled sign-in button with an unprovisioned OAuth client; no download link to an absent artifact.
- No embedded credentials, tokens in logs, disabled TLS verification or executable message markup.
- No account data from a previous login or mailbox rendered after a switch.
- No unacknowledged message marked read unless its conversation/subject is visible in an active window.
- Verify actual package compatibility; describe tested distributions/architectures on the download page. A Debian package does not imply every Linux distribution supports it.
- Wayland is the preferred desktop path; verify X11 in CI and document any physical-desktop validation still outstanding. Flatpak/portal testing is distinct from an unpackaged GTK smoke test.
- Defer system tray integration and offline message storage. The app uses the regular application window and system notifications while running; closing it exits.

## Evidence and delivery

Maintain `PROGRESS.md` separately with completed steps, commands/results, package versions, screenshots, live-test scope, release URLs and deployment evidence. Preserve private credentials and test accounts outside Git. The final handoff must distinguish implemented and verified behavior from remaining distribution or desktop-environment limitations.
