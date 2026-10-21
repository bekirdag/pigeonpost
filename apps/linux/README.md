# Pigeonpost Desktop for Linux

Native GTK 4/libadwaita client, using the macOS app's three-column layout. See [BUILD-PLAN.md](BUILD-PLAN.md) and [PROGRESS.md](PROGRESS.md) for scope and release evidence.

## Develop on Debian or Ubuntu

```sh
sudo apt install python3-gi python3-gi-cairo gir1.2-gtk-4.0 gir1.2-adw-1 gir1.2-secret-1 gnome-keyring
cd apps/linux
python3 -m pigeonpost
```

Requires a running desktop session with a Secret Service keyring. Flatpak users grant the same keyring access through the application sandbox. No tokens or message database are written into the app's configuration directory. Closing the window exits; notifications arrive while the app is running.

## Build and test

```sh
./build-native.sh            # group encryption library for this processor (Rust; see below)
python3 -m unittest discover -s tests -p 'test_core.py' -v
python3 -m unittest discover -s tests -p 'test_groups.py' -v
dbus-run-session -- xvfb-run -a python3 -m unittest discover -s tests -p 'test_ui*.py' -v
POSTBOX_BIN=../../target/debug/pigeonpost-postbox node tests/live_interop.mjs   # optional, real postbox
python3 package-deb.py       # needs pigeonpost/native/x86_64 and aarch64 (CI builds both)
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --force-clean --repo=repo build-dir dev.pigeonpost.Desktop.json
flatpak build-bundle repo dist/Pigeonpost-Desktop-1.0.9-x86_64.flatpak dev.pigeonpost.Desktop --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
```

Install the Debian package with `sudo apt install ./pigeonpost-desktop_1.0.9_all.deb` so native dependencies are resolved. Install the Flatpak with `flatpak install --user ./Pigeonpost-Desktop-1.0.9-x86_64.flatpak`, then launch Pigeonpost from the application menu or `flatpak run dev.pigeonpost.Desktop`. The first Flatpak installation also downloads the GNOME runtime. This is a direct release bundle, not a Flathub listing; install a newer bundle to update.

## Authentication and payments

The production issuer is `https://auth.pigeonpost.dev/realms/pigeonpost-prod`; the dedicated public client is `pigeonpost-linux`. Device authorization requires explicit browser consent. The client has no shipped secret and does not collect a password. Handle availability is native; registration, payment confirmation, subscription management and account deletion open the existing Pigeonpost website. No purchase takes place just by checking a name.

## Release

`.github/workflows/linux-desktop.yml` builds, validates and uploads packages. Version tags use `linux-desktop-*`; desktop releases do not replace the CLI's latest release. Publish only after unit/native UI, installed Debian and Flatpak checks pass. Include `SHA256SUMS` and GitHub build attestations. Keep download links out of the website until release assets exist.

Subject rows show unread counts. Message notifications open their original subject. Ubuntu Dock/Unity-compatible taskbars show a numeric badge; other shells retain the count in the window title and thread list. Closing the Linux app still exits it.

## Design, attachments and calls (1.0.8)

The interface uses the Pigeonpost conversation design shared with the iPhone, iPad, Mac and web apps (palette from `apps/ios/Shared/Design/Theme.swift`; dark exact, light matched) and the bundled Geist faces (SIL OFL 1.1, `pigeonpost/fonts/Geist-OFL.txt`). Settings offers System, Light or Dark; the choice is kept in `preferences.json` in the app's configuration directory and contains nothing else.

Pictures, video, sound, PDFs and plain text open in an in-app viewer with a Save button. HTML, SVG and other markup are never rendered; they can only be saved. Video and sound use GTK's GStreamer media backend (`libgtk-4-media-gstreamer`).

Voice and video call buttons open the web inbox (`https://inbox.pigeonpost.dev`) in the default browser and copy the conversation's address, because calls need WebRTC and no Linux WebKitGTK build we can depend on enables it: Ubuntu 24.04 and 26.04 (WebKitGTK 2.52.6), Debian 13 (2.54.0) and the Flathub GNOME 50 runtime (2.54.1) all expose `getUserMedia` but not `RTCPeerConnection` (checked 2026-10-19). Embedding `native-call.html` becomes possible once a runtime ships WebRTC.


## Incoming calls

While the app is running and signed in, it listens on the calls event stream (`https://inbox.pigeonpost.dev/calls/v1/events`, member token, reconnecting with backoff; a refused token is renewed once, then listening stops until the next sign-in). A ring shows an urgent desktop notification, "Incoming video call" or "Incoming voice call", naming the caller by contact name or handle, with Answer and Decline. Answer opens `https://inbox.pigeonpost.dev/?answer=<room>` in the default browser, which picks the call up there; Decline declines from the app's own session. The notification is withdrawn when the ring ends anywhere (answered elsewhere, declined, caller hung up, unanswered). If `canberra-gtk-play` is installed, the desktop's `phone-incoming-call` sound plays while it rings; otherwise the notification is silent apart from what the desktop itself does. With the app closed, Linux does not ring. The client is `pigeonpost/calls.py`; room ids and addresses are validated and caller names are reduced to plain text before display.

## Phase 1b: calls, filtering, blocking and pictures (1.0.9)

Every feature below is shown only when `GET /v1/capabilities` lists its flag, so the app behaves like 1.0.8 against an older postbox.

- **Calls** (`call_history`): a Calls entry under the mailbox switcher with the unseen missed-call count, All/Missed, a collapsed Filtered calls section, call back (opens the web inbox), Delete from history, Block and Report spam per row, Clear all. Missed calls also appear as a line in the one-to-one conversation.
- **Filtered** (`message_filtering`): a "Filtered (n)" row at the bottom of the conversation list; Accept, Block (report on by default) and Report spam.
- **Settings → Privacy & filtering**: the six server-side controls, each shown per its flag. **Settings → Blocked**: exact and namespace blocks with Unblock, and Block for an address. **Settings → Profile picture** (`avatars`): file chooser, square crop with zoom (drag, slider, arrow and +/− keys), visibility, Use for all my mailboxes, Remove. The crop is uploaded as a JPEG of at most 1024 px, so no metadata of the original file leaves the computer.
- **Block** from the conversation menu, the contact panel, a message's menu, a call row, a group member and the Filtered list, with "Also report as spam" and "Block everyone at /ns". A blocked conversation keeps its history behind a banner with Unblock, and the composer is off.
- **Pictures** replace initials in the conversation list, conversation header, call rows, group bubbles and member lists. They are fetched with the account token, kept in memory only and never written to disk.

## Groups (Phase 3b, 1.0.9)

End-to-end encrypted groups per `docs/groups.md`, with the shared library `crates/pigeonpost-groupcrypto` (OpenMLS) through its UniFFI Python binding. The packages carry the library for x86_64 and ARM64 in `pigeonpost/native/<arch>`; if it does not load, groups stay hidden (there is no weaker fallback).

- **Device and keys**: each mailbox gets its own device (`linux-<12 base32>`), registered with the postbox, which attests hosted mailboxes. The MLS state, per-group cursors and decrypted history are sealed (XChaCha20-Poly1305, fresh key per save) into `$XDG_DATA_HOME/pigeonpost-desktop/groups/<mailbox hash>/store-*.bin` (mode 0600); the key lives only in the Secret Service keyring. Signing out revokes the device at the postbox and deletes its keys; a later sign-in is a new device without history.
- **Client loop** (`groups_core.py`, a port of the web's `groups-core.js`): key packages kept at ≥ 10 plus a last-resort one, Welcomes joined and acknowledged, every group read strictly in sequence, pending adds and removals committed with jitter and retry on `revision_conflict`, `epoch_conflict` re-encrypted under the same client id, `410 resync_required` handled, weekly self-updates, expiry by the device's own clock. It runs every 8 s (4 s with a group open); there is no event stream on Linux.
- **Interface**: New group beside New conversation; Invitations (n) and groups above direct conversations; the group view with sender names and pictures, notices, attachments (sealed per file), announcement mode and "Waiting for a member's device" states; Group info with members and roles, add people, invite link, settings, timer, notifications, transfer/leave/delete, each member's device fingerprints (with "Mark as verified" and a warning when they change), this device's fingerprint and the group safety number. "X added a new device · <fingerprint>" lines come from verified commits; a commit that disagrees with the postbox's record shows a security warning. Group calls open the web inbox.
- **Disappearing messages** (`disappearing`): timer in a conversation's menu and in Group info, a timer badge in the header, an icon on each expiring message and the stated limits.
