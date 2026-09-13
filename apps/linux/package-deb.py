#!/usr/bin/env python3
"""Build an architecture-independent Debian package with declared native dependencies."""
import pathlib
import subprocess
import tempfile
from pigeonpost import VERSION

root = pathlib.Path(__file__).resolve().parent
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
Depends: python3 (>= 3.10), python3-gi, python3-gi-cairo, gir1.2-gtk-4.0 (>= 4.8), gir1.2-adw-1 (>= 1.2), gir1.2-secret-1, ca-certificates
Recommends: gnome-keyring | keepassxc, xdg-desktop-portal, xdg-desktop-portal-gtk
Homepage: https://pigeonpost.dev
Description: Native Pigeonpost desktop messaging client
 A GTK 4 and libadwaita inbox for people and agents with secure
 browser sign-in, subjects, attachments and contact permissions.
""")
    destination = out / f"pigeonpost-desktop_{VERSION}_all.deb"
    subprocess.run(["dpkg-deb", "--root-owner-group", "--build", staging, str(destination)], check=True)
    print(destination)
