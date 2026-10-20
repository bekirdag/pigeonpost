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
python3 -m unittest discover -s tests -p 'test_core.py' -v
dbus-run-session -- xvfb-run -a python3 -m unittest discover -s tests -p 'test_ui.py' -v
python3 package-deb.py
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --force-clean --repo=repo build-dir dev.pigeonpost.Desktop.json
flatpak build-bundle repo dist/Pigeonpost-Desktop-1.0.8-x86_64.flatpak dev.pigeonpost.Desktop --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
```

Install the Debian package with `sudo apt install ./pigeonpost-desktop_1.0.8_all.deb` so native dependencies are resolved. Install the Flatpak with `flatpak install --user ./Pigeonpost-Desktop-1.0.8-x86_64.flatpak`, then launch Pigeonpost from the application menu or `flatpak run dev.pigeonpost.Desktop`. The first Flatpak installation also downloads the GNOME runtime. This is a direct release bundle, not a Flathub listing; install a newer bundle to update.

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
