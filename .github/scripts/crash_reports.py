#!/usr/bin/env python3
"""Collect what Apple knows about this app crashing, and print it.

Three separate sources, because Apple keeps them in three places and only one of them is the one
people mean by "the crash reports":

  * `betaFeedbackCrashSubmissions` — a TestFlight tester's device crashed and the crash log came
    back with the build it happened on. This is the one that matters while an app is in beta.
  * `diagnosticSignatures` on a build — hangs and disk-write signatures gathered from the field,
    each with a downloadable log of the sampled stacks.
  * `perfPowerMetrics` on the app — the aggregate Xcode Metrics, which need enough adoption in
    production to exist at all and will be empty for a TestFlight-only app.

It never fails the job when a source is empty or absent: "no crashes" and "this account cannot see
that endpoint yet" are both ordinary answers, and a red run for either would train everyone to
ignore it. It exits non-zero only when a source errors in a way that means the report is wrong.

Standard library only, and the token minting is the same shape as testflight_status.py — this runs
on Linux and a pip install is one more thing that can be down.
"""

import base64
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

API = "https://api.appstoreconnect.apple.com/v1"

KEY = os.environ["KEY"]
KEY_ID = os.environ["KEY_ID"]
ISSUER_ID = os.environ["ISSUER_ID"]
BUNDLE_ID = os.environ.get("BUNDLE_ID", "dev.pigeonpost.inbox")
BUILD_LIMIT = int(os.environ.get("BUILD_LIMIT", "10"))


def _b64(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def _der_to_raw(der: bytes) -> bytes:
    """An ES256 JWT signature is r||s, 32 bytes each. openssl emits DER. Convert."""
    assert der[0] == 0x30
    body = der[2:] if der[1] < 0x80 else der[2 + (der[1] & 0x7F) :]
    out = b""
    while body:
        assert body[0] == 0x02
        length = body[1]
        value = body[2 : 2 + length].lstrip(b"\x00")
        out += value.rjust(32, b"\x00")
        body = body[2 + length :]
    return out


def mint_token() -> str:
    header = _b64(json.dumps({"alg": "ES256", "kid": KEY_ID, "typ": "JWT"}).encode())
    now = int(time.time())
    payload = _b64(
        json.dumps({"iss": ISSUER_ID, "iat": now, "exp": now + 1200, "aud": "appstoreconnect-v1"}).encode()
    )
    signing_input = f"{header}.{payload}".encode()
    with tempfile.TemporaryDirectory() as tmp:
        key_path = os.path.join(tmp, "key.p8")
        with open(os.open(key_path, os.O_WRONLY | os.O_CREAT, 0o600), "wb") as fh:
            fh.write(KEY.encode() if KEY.endswith("\n") else (KEY + "\n").encode())
        der = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", key_path],
            input=signing_input,
            capture_output=True,
            check=True,
        ).stdout
    return f"{header}.{payload}.{_b64(_der_to_raw(der))}"


TOKEN = mint_token()


class Missing(Exception):
    """The endpoint is not available to this key, or does not exist on this account."""


def get(path, **params):
    url = path if path.startswith("http") else API + path
    if params:
        url += "?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, method="GET")
    req.add_header("Authorization", f"Bearer {TOKEN}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read()
        return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode(errors="replace")[:600]
        # 404 is "no such collection here", 403 is "this key may not see it". Neither is a broken
        # report — both are facts about the account, and saying which is the useful part.
        if exc.code in (403, 404):
            raise Missing(f"{exc.code} {detail}") from None
        raise RuntimeError(f"GET {url} -> {exc.code} {detail}") from None


def fetch_text(url):
    req = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.read().decode(errors="replace")
    except Exception as exc:  # a log URL is presigned and expires; never fail the report on one
        return f"<could not fetch: {exc}>"


findings = 0
problems = []

apps = get("/apps", **{"filter[bundleId]": BUNDLE_ID})["data"]
if not apps:
    sys.exit(f"No app with bundle id {BUNDLE_ID} on this account.")
app = apps[0]
app_id = app["id"]
print(f"App: {app['attributes']['name']} ({BUNDLE_ID}) id={app_id}\n")

builds = get(
    "/builds",
    **{"filter[app]": app_id, "limit": BUILD_LIMIT, "sort": "-version", "include": "preReleaseVersion"},
)
versions = {}
for inc in builds.get("included", []):
    if inc["type"] == "preReleaseVersions":
        versions[inc["id"]] = inc["attributes"].get("version", "?")
build_rows = []
for b in builds["data"]:
    rel = b.get("relationships", {}).get("preReleaseVersion", {}).get("data") or {}
    build_rows.append((b["id"], versions.get(rel.get("id"), "?"), b["attributes"]["version"]))
print("Builds on record: " + (", ".join(f"{v} ({n})" for _, v, n in build_rows) or "none") + "\n")

# ---- 1. TestFlight crash submissions ---------------------------------------------------------
print("== TestFlight crash submissions ==")
try:
    subs = get(
        f"/apps/{app_id}/betaFeedbackCrashSubmissions",
        **{"limit": 50, "sort": "-createdDate", "include": "build"},
    )
    rows = subs.get("data", [])
    if not rows:
        print("  none — no tester's device has sent a crash back.")
    for row in rows:
        findings += 1
        a = row.get("attributes", {})
        print(f"\n  --- crash {row['id']}")
        for field in ("createdDate", "deviceModel", "osVersion", "appPlatform", "devicePlatform", "crashType"):
            if a.get(field):
                print(f"      {field}: {a[field]}")
        url = a.get("crashLog", {}).get("url") if isinstance(a.get("crashLog"), dict) else a.get("crashLog")
        if url:
            print("      log:")
            for line in fetch_text(url).splitlines()[:120]:
                print("        " + line)
except Missing as exc:
    print(f"  not available to this key: {exc}")
    problems.append("betaFeedbackCrashSubmissions unavailable")

# ---- 2. Diagnostic signatures per build ------------------------------------------------------
print("\n== Diagnostic signatures (hangs, disk writes) ==")
any_sig = False
for build_id, version, number in build_rows[:5]:
    for kind in ("HANGS", "DISK_WRITES"):
        try:
            sigs = get(
                f"/builds/{build_id}/diagnosticSignatures",
                **{"filter[diagnosticType]": kind, "limit": 20},
            )
        except Missing:
            continue
        for sig in sigs.get("data", []):
            any_sig = True
            findings += 1
            a = sig.get("attributes", {})
            print(f"\n  --- {kind} on {version} ({number}) — weight {a.get('weight')}")
            print(f"      signature: {a.get('signature')}")
            try:
                logs = get(f"/diagnosticSignatures/{sig['id']}/logs", **{"limit": 1})
            except Missing:
                continue
            for log in logs.get("data", []):
                for line in json.dumps(log.get("attributes", {}), indent=2).splitlines()[:60]:
                    print("        " + line)
if not any_sig:
    print("  none.")

# ---- 3. Aggregate metrics --------------------------------------------------------------------
print("\n== Xcode Metrics (production aggregate) ==")
try:
    metrics = get(f"/apps/{app_id}/perfPowerMetrics")
    rows = metrics.get("data", [])
    if not rows:
        print("  none — this needs enough adoption in production, which a beta does not have.")
    for row in rows:
        print("  " + json.dumps(row)[:400])
except Missing as exc:
    print(f"  not available: {exc}")

print(f"\n{findings} thing(s) to look at.")
if problems:
    print("Sources that could not be read: " + "; ".join(problems))
