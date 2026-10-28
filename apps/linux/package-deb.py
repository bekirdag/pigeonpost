#!/usr/bin/env python3
"""Build the Debian package with declared native dependencies.

One package serves x86_64 and ARM64: the only native code, the group encryption library, is
shipped for both processors (pigeonpost/native/<arch>) and the app loads the one that matches.
"""
import pathlib
import subprocess
import sys
import tempfile
from pigeonpost import VERSION

root = pathlib.Path(__file__).resolve().parent
missing = [arch for arch in ("x86_64", "aarch64")
           if not (root / "pigeonpost/native" / arch / "libpigeonpost_groupcrypto_ffi.so").is_file()
           or not (root / "pigeonpost/native" / arch / "pigeonpost_groupcrypto.py").is_file()]
if missing and "--allow-missing-native" not in sys.argv:
    raise SystemExit("group encryption library missing for " + ", ".join(missing) + " (see build-native.sh); "
                     "pass --allow-missing-native for a local build without groups there")
out = root / "dist"
out.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="pigeonpost-deb-") as staging:
    subprocess.run(["python3", str(root / "install.py"), "--destdir", staging], check=True)
    control = pathlib.Path(staging) / "DEBIAN"
    control.mkdir()
    (control / "control").write_text(f"""Package: pigeonpost-desktop
Version: {VERSION}
Section: net
Priority: optional
Architecture: all
Maintainer: Wodo Teknoloji A.Ş. <support@pigeonpost.dev>
Depends: python3 (>= 3.10), python3-gi, python3-gi-cairo, gir1.2-gtk-4.0 (>= 4.8), gir1.2-adw-1 (>= 1.2), gir1.2-secret-1, gir1.2-poppler-0.18, gir1.2-gstreamer-1.0, gir1.2-gst-plugins-base-1.0, libgtk-4-media-gstreamer, gstreamer1.0-plugins-good, gstreamer1.0-libav, ca-certificates, tzdata, python3-cryptography
Recommends: gnome-keyring | keepassxc, xdg-desktop-portal, xdg-desktop-portal-gtk
Homepage: https://pigeonpost.dev
Description: Native Pigeonpost desktop messaging client
 A GTK 4 and libadwaita inbox for people and agents with secure
 browser sign-in, subjects, attachments, contact permissions, call
 history, filtering, blocking, profile pictures, end-to-end
 encrypted groups, message edits, replies and receipts, a calendar,
 organization rooms, live stream banners and meeting notes. Includes the group encryption library
 (OpenMLS, MIT) for x86_64 and ARM64.
""")
    destination = out / f"pigeonpost-desktop_{VERSION}_all.deb"
    subprocess.run(["dpkg-deb", "--root-owner-group", "--build", staging, str(destination)], check=True)
    print(destination)
