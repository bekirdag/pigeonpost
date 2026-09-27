#!/usr/bin/env python3
"""Select an available iPad for native tests or the newest-SDK host smoke."""

import argparse
import json
import re
import sys


def version_key(value):
    """Compare numeric versions, treating an omitted patch version as zero."""
    parts = tuple(int(part) for part in value.split("."))
    return parts + (0,) * max(0, 3 - len(parts))


def select_device(data, ios_version=None):
    required = version_key(ios_version) if ios_version is not None else None
    candidates = []
    for runtime, devices in data.get("devices", {}).items():
        match = re.fullmatch(r"com\.apple\.CoreSimulator\.SimRuntime\.iOS-(\d+(?:-\d+)*)", runtime)
        if not match:
            continue
        version = version_key(match.group(1).replace("-", "."))
        if required is not None and version != required:
            continue
        for device in devices:
            if (
                device.get("isAvailable") is True
                and device.get("name", "").startswith("iPad Pro 13-inch")
                and device.get("udid")
            ):
                candidates.append((version, device["udid"]))
    if not candidates:
        requirement = f"iOS {ios_version}" if ios_version else "any installed iOS runtime"
        raise ValueError(f"No available iPad Pro 13-inch simulator for {requirement}.")
    return max(candidates)[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ios-version", help="Require this exact runtime version; never fall back.")
    args = parser.parse_args()
    try:
        print(select_device(json.load(sys.stdin), args.ios_version))
    except (ValueError, TypeError, KeyError) as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
