#!/usr/bin/env python3
"""Place synthetic calendar input in one explicit simulator's local document provider."""
import argparse
import pathlib
import subprocess
import time
import uuid

FOLDER = "Pigeonpost calendar tests"
FILENAME = "Pigeonpost import fixture.ics"
MARKER = "Synthetic Pigeonpost calendar UI fixtures only.\n"
DATA = (
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:pp-local-picker-test\r\n"
    "DTSTAMP:20261025T000000Z\r\nDTSTART:20261025T090000Z\r\nDTEND:20261025T100000Z\r\n"
    "SUMMARY:Local picker interview\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
).encode()


def local_provider_group(output: str) -> pathlib.Path:
    paths = []
    for line in output.splitlines():
        name, separator, path = line.partition("\t")
        if name == "group.com.apple.FileProvider.LocalStorage" and separator and path:
            paths.append(pathlib.Path(path))
    if len(paths) != 1:
        raise ValueError("Expected exactly one local file-provider group")
    return paths[0]


def prepare(device: str) -> None:
    device = str(uuid.UUID(device)).upper()  # Never target another agent's implicit 'booted' device.
    group = None
    for attempt in range(15):
        result = subprocess.run(
            ["xcrun", "simctl", "get_app_container", device, "com.apple.DocumentsApp", "groups"],
            capture_output=True, text=True, check=False,
        )
        try:
            if result.returncode == 0:
                group = local_provider_group(result.stdout).resolve(strict=True)
                break
        except (ValueError, FileNotFoundError):
            pass
        if attempt == 0:
            subprocess.run(["xcrun", "simctl", "launch", device, "com.apple.DocumentsApp"], capture_output=True, check=True)
        time.sleep(0.2)
    if group is None or group.parent.name != "AppGroup" or device not in (part.upper() for part in group.parts):
        raise ValueError("A local simulator file-provider container is required")
    storage = group / "File Provider Storage"
    if not storage.is_dir() or storage.is_symlink():
        raise ValueError("The simulator's local document storage is unavailable")
    folder = storage / FOLDER
    marker = folder / ".pigeonpost-calendar-ui"
    if folder.exists():
        if folder.is_symlink() or not folder.is_dir() or marker.is_symlink() or not marker.is_file() or marker.read_text() != MARKER:
            raise ValueError("Refusing to alter an existing non-fixture folder")
    else:
        folder.mkdir(mode=0o700)
        marker.write_text(MARKER)
        marker.chmod(0o600)
    target = folder / FILENAME
    if target.is_symlink() or target.exists() and (not target.is_file() or target.read_bytes() != DATA):
        raise ValueError("Refusing to replace a non-fixture document")
    if not target.exists():
        target.write_bytes(DATA)
        target.chmod(0o600)
    print("Prepared a synthetic local calendar document for the explicit simulator.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--simulator", required=True)
    prepare(parser.parse_args().simulator)
