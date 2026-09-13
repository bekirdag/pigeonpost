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
flatpak build-bundle repo dist/Pigeonpost-Desktop-1.0.1-x86_64.flatpak dev.pigeonpost.Desktop --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
```

Install the Debian package with `sudo apt install ./pigeonpost-desktop_1.0.1_all.deb` so native dependencies are resolved. Install the Flatpak with `flatpak install --user ./Pigeonpost-Desktop-1.0.1-x86_64.flatpak`, then launch Pigeonpost from the application menu or `flatpak run dev.pigeonpost.Desktop`. The first Flatpak installation also downloads the GNOME runtime. This is a direct release bundle, not a Flathub listing; install a newer bundle to update.

## Authentication and payments

The production issuer is `https://auth.pigeonpost.dev/realms/pigeonpost-prod`; the dedicated public client is `pigeonpost-linux`. Device authorization requires explicit browser consent. The client has no shipped secret and does not collect a password. Handle availability is native; registration, payment confirmation, subscription management and account deletion open the existing Pigeonpost website. No purchase takes place just by checking a name.

## Release

`.github/workflows/linux-desktop.yml` builds, validates and uploads packages. Version tags use `linux-desktop-*`; desktop releases do not replace the CLI's latest release. Publish only after unit/native UI, installed Debian and Flatpak checks pass. Include `SHA256SUMS` and GitHub build attestations. Keep download links out of the website until release assets exist.
