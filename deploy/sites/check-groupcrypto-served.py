#!/usr/bin/env python3
"""Fail when the served web group-crypto runtime differs from this checkout.

Every loader must only ever see bytes from its own content-addressed bundle, so this checks
each name a browser can reach:

* the versioned bundle (`?v=<WASM_SHA256>`, nginx maps it to `bundles/<sha>/`) - the URL the
  current `groups.js` imports;
* the unversioned names and the legacy `?v=20261020-p3a` alias, which old cached loaders still
  request with unversioned relative imports - they must mirror the current bundle, never an
  older build (2026-11-03 audit P0-1: they still served the 20 October WASM);
* the chain itself: the served `index.html` names the repo's `groups.js` digest, and the served
  `groups.js` imports the repo's loader version;
* optional retained bundles (`--retained <sha>`): still self-consistent, so old clients keep
  working.

Usage (deploy guard and the scheduled CI check):
    python3 -I deploy/sites/check-groupcrypto-served.py [--base https://inbox.pigeonpost.dev]
        [--host-header inbox.pigeonpost.dev] [--retained <sha> ...]
Exit status 0 only when everything matches.
"""
from __future__ import annotations

import argparse
import hashlib
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
VENDOR = REPO / "site-inbox/vendor/groupcrypto"
RUNTIME = ("loader.js", "integrity.js", "pigeonpost_groupcrypto.js", "pigeonpost_groupcrypto_bg.wasm")
LEGACY_ALIASES = ("", "20261020-p3a")


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def fetch(base: str, path: str, host: str | None, attempts: int = 3) -> bytes:
    # A unique, ignored parameter would change the edge cache key and hide a stale cached
    # object, so fetch the exact URL a browser uses, and only ask intermediaries to revalidate.
    request = urllib.request.Request(base.rstrip("/") + "/" + path, headers={
        "User-Agent": "pigeonpost-groupcrypto-served-check/1",
        "Cache-Control": "no-cache",
    })
    if host:
        request.add_header("Host", host)
    for attempt in range(attempts):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return response.read()
        except (urllib.error.URLError, TimeoutError) as error:
            if attempt == attempts - 1:
                raise RuntimeError(f"{path}: {error}") from error
            time.sleep(2 * (attempt + 1))
    raise AssertionError("unreachable")


def expected() -> tuple[str, dict[str, bytes]]:
    files = {name: (VENDOR / name).read_bytes() for name in RUNTIME}
    match = re.search(rb'WASM_SHA256 = "([0-9a-f]{64})"', files["integrity.js"])
    if not match:
        raise SystemExit("repo integrity.js has no WASM_SHA256")
    digest = match.group(1).decode()
    if sha(files["pigeonpost_groupcrypto_bg.wasm"]) != digest:
        raise SystemExit("repo wasm does not match its own integrity.js; rebuild the bundle")
    return digest, files


def check(base: str, host: str | None, retained: list[str]) -> list[str]:
    digest, files = expected()
    problems: list[str] = []

    def compare(label: str, path: str, want: bytes) -> None:
        try:
            got = fetch(base, path, host)
        except RuntimeError as error:
            problems.append(f"{label}: fetch failed ({error})")
            return
        if got != want:
            problems.append(f"{label}: served {sha(got)[:16]} ({len(got)} B), repo {sha(want)[:16]} ({len(want)} B)")
        else:
            print(f"ok  {label}  {sha(got)[:16]}  {len(got)} B")

    for name, data in files.items():
        compare(f"bundle {name}", f"vendor/groupcrypto/{name}?v={digest}", data)
    for alias in LEGACY_ALIASES:
        suffix = f"?v={alias}" if alias else ""
        for name, data in files.items():
            compare(f"legacy{suffix or ' (unversioned)'} {name}", f"vendor/groupcrypto/{name}{suffix}", data)

    groups = (REPO / "site-inbox/groups.js").read_bytes()
    if f"vendor/groupcrypto/loader.js?v={digest}".encode() not in groups:
        problems.append("repo groups.js does not import the repo loader version")
    compare("groups.js entry", f"groups.js?v={sha(groups)}", groups)
    try:
        index = fetch(base, "", host)
        if f'src="groups.js?v={sha(groups)}"'.encode() not in index:
            problems.append("served index.html does not load the repo groups.js digest")
        else:
            print("ok  index.html loads the repo groups.js")
    except RuntimeError as error:
        problems.append(f"index.html: fetch failed ({error})")

    for old in retained:
        if not re.fullmatch(r"[0-9a-f]{64}", old):
            problems.append(f"retained bundle {old!r} is not a digest")
            continue
        try:
            integrity = fetch(base, f"vendor/groupcrypto/integrity.js?v={old}", host)
            wasm = fetch(base, f"vendor/groupcrypto/pigeonpost_groupcrypto_bg.wasm?v={old}", host)
            loader = fetch(base, f"vendor/groupcrypto/loader.js?v={old}", host)
        except RuntimeError as error:
            problems.append(f"retained {old[:16]}: fetch failed ({error})")
            continue
        pinned = re.search(rb'WASM_SHA256 = "([0-9a-f]{64})"', integrity)
        if sha(wasm) != old or not pinned or pinned.group(1).decode() != old:
            problems.append(f"retained {old[:16]}: bundle is not self-consistent")
        elif re.search(rb"from ['\"]\./(integrity|pigeonpost_groupcrypto)\.js['\"]", loader):
            problems.append(f"retained {old[:16]}: loader imports unversioned files")
        else:
            print(f"ok  retained bundle {old[:16]} is self-consistent")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", default="https://inbox.pigeonpost.dev")
    parser.add_argument("--host-header", default=None, help="Host header for a loopback origin check")
    parser.add_argument("--retained", action="append", default=[], help="older bundle digest that must keep working")
    args = parser.parse_args()
    problems = check(args.base, args.host_header, args.retained)
    for problem in problems:
        print(f"::error::groupcrypto served bytes: {problem}", file=sys.stderr)
    if problems:
        print(f"FAIL: {len(problems)} mismatch(es) against {args.base}", file=sys.stderr)
        return 1
    print(f"served group-crypto runtime matches the repo at {args.base}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
