#!/usr/bin/env python3
"""Say why a TestFlight build is or is not visible on a tester's phone, and put it there.

A build reaches a phone only when all of these are true: it finished processing, it has not
expired, export compliance has been answered, it belongs to at least one beta group, and that
group has at least one tester. This reads all five out of App Store Connect and names the ones
that are false.

With FIX=true it also does the two things that are safe to do unattended: answers export
compliance from the value the app already declares in its Info.plist, and adds the build to every
internal group. With WAIT_FOR=<build number> it waits for that build to finish processing first,
which is what the upload workflow needs — an upload returns long before Apple has a build to hand
anybody.

Standard library only, on purpose. This runs on the macOS upload runner as well as on Linux, and
a pip install is one more thing that can be down on the morning you need this to work.
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
FIX = os.environ.get("FIX", "false").lower() == "true"
WAIT_FOR = (os.environ.get("WAIT_FOR") or "").strip()
WAIT_MINUTES = int(os.environ.get("WAIT_MINUTES", "30"))


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


def call(method, path, body=None, **params):
    url = path if path.startswith("http") else API + path
    if params:
        url += "?" + urllib.parse.urlencode(params)
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    if data:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read()
        return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode(errors="replace")[:600]
        raise RuntimeError(f"{method} {url} -> {exc.code} {detail}") from None


def get(path, **params):
    return call("GET", path, None, **params)


apps = get("/apps", **{"filter[bundleId]": BUNDLE_ID})["data"]
if not apps:
    sys.exit(f"No app with bundle id {BUNDLE_ID} on this account.")
app = apps[0]
app_id = app["id"]
print(f"App: {app['attributes']['name']} ({BUNDLE_ID}) id={app_id}\n")


def fetch_builds():
    builds = get(
        "/builds",
        **{
            "filter[app]": app_id,
            "limit": 20,
            "sort": "-version",
            "include": "buildBetaDetail,preReleaseVersion,betaGroups",
        },
    )
    return builds, {(i["type"], i["id"]): i for i in builds.get("included", [])}


target = None
if WAIT_FOR:
    # An upload returns as soon as Apple has the package. There is nothing to distribute until
    # processing finishes, which is usually a couple of minutes and occasionally much longer.
    deadline = time.time() + WAIT_MINUTES * 60
    while True:
        builds, included = fetch_builds()
        match = [b for b in builds["data"] if b["attributes"]["version"] == WAIT_FOR]
        if match:
            target = match[0]
            state = target["attributes"]["processingState"]
            if state == "VALID":
                print(f"Build {WAIT_FOR} finished processing.\n")
                break
            if state in ("INVALID", "FAILED"):
                sys.exit(f"Build {WAIT_FOR} came out of processing {state}. Nothing to distribute.")
            print(f"  build {WAIT_FOR} is {state}; waiting…")
        else:
            print(f"  build {WAIT_FOR} has not appeared yet; waiting…")
        if time.time() > deadline:
            sys.exit(f"Build {WAIT_FOR} was still not ready after {WAIT_MINUTES} minutes.")
        time.sleep(30)
else:
    builds, included = fetch_builds()

print("Builds, newest first")
print("-" * 78)
for b in builds["data"]:
    a = b["attributes"]
    detail = included.get(("buildBetaDetails", (b["relationships"]["buildBetaDetail"]["data"] or {}).get("id", "")))
    da = detail["attributes"] if detail else {}
    pre = included.get(("preReleaseVersions", (b["relationships"]["preReleaseVersion"]["data"] or {}).get("id", "")))
    version = pre["attributes"]["version"] if pre else "?"
    group_names = [
        included[("betaGroups", g["id"])]["attributes"]["name"]
        for g in (b["relationships"].get("betaGroups", {}).get("data") or [])
        if ("betaGroups", g["id"]) in included
    ]
    if target is None:
        target = b
    print(
        f"  {version} ({a['version']})  processing={a['processingState']}  expired={a['expired']}  "
        f"compliance={a.get('usesNonExemptEncryption')}\n"
        f"      internal={da.get('internalBuildState')}  external={da.get('externalBuildState')}  "
        f"autoNotify={da.get('autoNotifyEnabled')}\n"
        f"      groups={group_names or 'NONE'}  uploaded={a['uploadedDate']}"
    )
print()

groups = get("/betaGroups", **{"filter[app]": app_id, "limit": 50})
print("Beta groups")
print("-" * 78)
if not groups["data"]:
    print("  NONE. A build in no group reaches nobody.")
for g in groups["data"]:
    a = g["attributes"]
    testers = get(f"/betaGroups/{g['id']}/betaTesters", limit=200)["data"]
    print(
        f"  {a['name']}  internal={a['isInternalGroup']}  publicLink={a.get('publicLinkEnabled')}  "
        f"autoAddBuilds={a.get('hasAccessToAllBuilds')}  testers={len(testers)}"
    )
    for t in testers[:20]:
        ta = t["attributes"]
        print(f"      - {ta.get('email')}  state={ta.get('state')}  invite={ta.get('inviteType')}")
print()

if FIX and target:
    a = target["attributes"]
    print("FIX")
    print("-" * 78)
    if a["processingState"] != "VALID":
        print(f"  Build {a['version']} is {a['processingState']}; nothing to hand a tester yet.")
        sys.exit(0)

    if a.get("usesNonExemptEncryption") is None:
        call(
            "PATCH",
            f"/builds/{target['id']}",
            {"data": {"type": "builds", "id": target["id"], "attributes": {"usesNonExemptEncryption": False}}},
        )
        print("  Answered export compliance: usesNonExemptEncryption=false (matches Info.plist).")
    else:
        print("  Export compliance already answered.")

    internal = [g for g in groups["data"] if g["attributes"]["isInternalGroup"]]
    if not internal:
        print("  ::warning::No internal group exists. Create one in App Store Connect and add yourself.")
    for g in internal:
        name = g["attributes"]["name"]
        already = [
            b for b in get(f"/betaGroups/{g['id']}/builds", limit=200)["data"] if b["id"] == target["id"]
        ]
        if already:
            print(f"  Build {a['version']} was already in {name}.")
            continue
        call(
            "POST",
            f"/betaGroups/{g['id']}/relationships/builds",
            {"data": [{"type": "builds", "id": target["id"]}]},
        )
        print(f"  Added build {a['version']} to internal group {name}.")
    # `hasAccessToAllBuilds` is create-only — App Store Connect rejects it on an UPDATE with
    # ENTITY_ERROR.ATTRIBUTE.NOT_ALLOWED — so an existing group cannot be told to take every build
    # afterwards. That is why this step is part of the upload workflow rather than a setting.
