"""Smoke-test the installed sandbox itself, not a source-tree import."""
import os
import pathlib
import subprocess
import time

directory = pathlib.Path("dist/screenshots")
directory.mkdir(parents=True, exist_ok=True)
environment = dict(os.environ, GSK_RENDERER="cairo")
# A disposable CI keyring, with no account token. Exercise the exact installed vault code.
subprocess.run(["gnome-keyring-daemon", "--unlock", "--components=secrets"], input=b"\n", capture_output=True, check=True)
probe = "import sys; sys.path.insert(0, '/app/share/pigeonpost-desktop'); from pigeonpost.vault import Vault; v=Vault(); v.save('ephemeral-ci-keyring-probe'); assert v.load()=='ephemeral-ci-keyring-probe'; v.clear(); assert v.load() is None; print('Sandbox keyring round trip passed')"
subprocess.run(["flatpak", "run", "--command=python3", "dev.pigeonpost.Desktop", "-c", probe], check=True, timeout=30)
# Document portal must actually be mounted, not just an app window that tolerates a failed portal.
result = subprocess.check_output(["gdbus", "call", "--session", "--dest", "org.freedesktop.portal.Documents", "--object-path", "/org/freedesktop/portal/documents", "--method", "org.freedesktop.portal.Documents.GetMountPoint"], text=True)
assert "/doc" in result or "0x2f" in result, "Document portal mount is unavailable"
forwarded = pathlib.Path(os.environ["XDG_RUNTIME_DIR"]) / "pigeonpost-portal-probe.txt"
forwarded.write_text("Pigeonpost file portal probe")
subprocess.run(["flatpak", "run", "--file-forwarding", "--command=python3", "dev.pigeonpost.Desktop", "-c",
                "import sys; assert open(sys.argv[1]).read() == 'Pigeonpost file portal probe'; print('Sandbox portal file read passed')",
                "@@", str(forwarded), "@@"], check=True, timeout=30)
with open("dist/flatpak-launch.log", "w") as log:
    process = subprocess.Popen(["flatpak", "run", "dev.pigeonpost.Desktop"], env=environment, stdout=log, stderr=log)
    try:
        time.sleep(5)
        assert process.poll() is None, "Installed Flatpak exited before showing its window"
        window = subprocess.check_output(["xdotool", "search", "--onlyvisible", "--name", "Pigeonpost"], text=True).splitlines()
        assert window, "Installed app has no visible window"
        subprocess.run(["import", "-window", "root", str(directory / "installed-flatpak.png")], check=True)
    finally:
        process.terminate()
        process.wait(timeout=10)
print("Installed Flatpak presented its native sign-in window")
