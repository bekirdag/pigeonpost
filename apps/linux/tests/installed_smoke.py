"""Smoke-test the installed sandbox itself, not a source-tree import."""
import os
import pathlib
import subprocess
import time

directory = pathlib.Path("dist/screenshots")
directory.mkdir(parents=True, exist_ok=True)
environment = dict(os.environ, GSK_RENDERER="cairo")
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
