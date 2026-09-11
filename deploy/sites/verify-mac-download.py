#!/usr/bin/env python3
"""Verify a Mac archive before publication, or after downloading its public URL."""
import argparse
import hashlib
import json
import plistlib
import re
import subprocess
import tempfile
from pathlib import Path


def run(*args):
    return subprocess.run(args, check=True, capture_output=True).stdout


def verify(archive, expected_sha256, version, build):
    if not re.fullmatch(r"[0-9a-f]{64}", expected_sha256):
        raise ValueError("Expected SHA-256 must be 64 lowercase hexadecimal characters")
    digest = hashlib.sha256()
    with archive.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != expected_sha256:
        raise ValueError("Archive SHA-256 does not match the approved build")

    with tempfile.TemporaryDirectory(prefix="pigeonpost-mac-verify-") as folder:
        run("ditto", "-x", "-k", str(archive), folder)
        app = Path(folder) / "Pigeonpost Desktop.app"
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        if (info.get("CFBundleIdentifier"), info.get("CFBundleShortVersionString"), info.get("CFBundleVersion")) != ("dev.pigeonpost.inbox", version, build):
            raise ValueError("App identity or version does not match the intended release")
        binary = app / "Contents/MacOS" / info["CFBundleExecutable"]
        architectures = run("lipo", "-archs", str(binary)).decode().split()
        if set(architectures) != {"arm64", "x86_64"}:
            raise ValueError("The Mac download must include Apple silicon and Intel code")
        run("codesign", "--verify", "--deep", "--strict", "--all-architectures", str(app))
        for arch in architectures:
            signature = subprocess.run(
                ["codesign", "-d", "--verbose=4", "--arch", arch, str(app)],
                check=True, capture_output=True, text=True,
            ).stderr
            if "TeamIdentifier=AH277897AV" not in signature or "Authority=Developer ID Application:" not in signature:
                raise ValueError(f"{arch}: expected Wodo Developer ID signature")
            if not re.search(r"flags=.*runtime", signature):
                raise ValueError(f"{arch}: hardened runtime is missing")
            entitlements = plistlib.loads(run("codesign", "-d", "--entitlements", "-", "--xml", "--arch", arch, str(app)))
            if entitlements.get("com.apple.security.get-task-allow"):
                raise ValueError(f"{arch}: distribution app permits debugging")
        run("xcrun", "stapler", "validate", str(app))
        run("spctl", "--assess", "--type", "execute", str(app))
        return {
            "version": version, "build": build, "architectures": architectures,
            "minimum_macos": info.get("LSMinimumSystemVersion"),
            "sha256": digest.hexdigest(), "gatekeeper": "accepted", "stapled": True,
        }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--build", required=True)
    args = parser.parse_args()
    print(json.dumps(verify(args.archive.resolve(), args.sha256, args.version, args.build), indent=2))
