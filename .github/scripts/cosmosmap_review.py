#!/usr/bin/env python3
"""Inspect CosmosMap review and optionally attach an explicitly selected valid build."""

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
from pathlib import Path

API = "https://api.appstoreconnect.apple.com/v1"

KEY = os.environ["KEY"]
KEY_ID = os.environ["KEY_ID"]
ISSUER_ID = os.environ["ISSUER_ID"]


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



APP_ID = "6815358482"
VERSION = "1.11"
PREVIOUS_BUILD_ID = "3189f64a-4e73-4928-ae77-800078d389b5"


def main():
    action = os.environ.get("ACTION", "inspect")
    number = os.environ.get("BUILD_NUMBER", "31")
    if action not in {"inspect", "attach"} or number != "31":
        raise RuntimeError("Only inspection or attaching the reviewed CosmosMap build 31 is supported.")
    versions = get(f"/apps/{APP_ID}/appStoreVersions", **{
        "filter[platform]": "IOS", "filter[versionString]": VERSION, "include": "build", "limit": 10,
    })
    if len(versions["data"]) != 1:
        raise RuntimeError("Expected one existing CosmosMap iOS version 1.11.")
    version = versions["data"][0]
    selected = version["relationships"]["build"]["data"]
    builds = get("/builds", **{"filter[app]": APP_ID, "filter[version]": number, "limit": 10})["data"]
    reviews = get(f"/apps/{APP_ID}/reviewSubmissions", limit=20)["data"]
    review_items = {}
    for row in reviews:
        if row["attributes"].get("state") in {"READY_FOR_REVIEW", "WAITING_FOR_REVIEW", "UNRESOLVED_ISSUES", "IN_REVIEW"}:
            items = get(f"/reviewSubmissions/{row['id']}/items", limit=200)["data"]
            review_items[row["id"]] = [{"id": item["id"], "state": item["attributes"].get("state"), "relationships": {key: value.get("data") for key, value in item.get("relationships", {}).items()}} for item in items]
    report = {
        "appId": APP_ID, "versionId": version["id"], "version": VERSION,
        "appStoreState": version["attributes"].get("appStoreState"),
        "releaseType": version["attributes"].get("releaseType"),
        "selectedBuildId": selected["id"] if selected else None,
        "selectedBuildNumber": next((b["attributes"]["version"] for b in versions.get("included", []) if b["type"] == "builds" and selected and b["id"] == selected["id"]), None),
        "targetBuild": [{"id": b["id"], "number": b["attributes"]["version"], "processingState": b["attributes"]["processingState"], "expired": b["attributes"]["expired"], "usesNonExemptEncryption": b["attributes"].get("usesNonExemptEncryption")} for b in builds],
        "reviews": [{"id": row["id"], "state": row["attributes"].get("state"), "platform": row["attributes"].get("platform")} for row in reviews],
        "reviewItems": review_items,
        "action": action, "attached": False,
    }
    print(json.dumps(report, indent=2))
    if action == "inspect":
        return report
    if len(builds) != 1:
        raise RuntimeError("Build 31 is not uniquely available; review is unchanged.")
    target = builds[0]
    a = target["attributes"]
    pre = get(f"/builds/{target['id']}/preReleaseVersion")["data"]
    if a["processingState"] != "VALID" or a["expired"] or a.get("usesNonExemptEncryption") is not False or pre["attributes"]["version"] != VERSION:
        raise RuntimeError("Target build is not valid, unexpired, compliant version 1.11; review is unchanged.")
    if selected and selected["id"] == target["id"]:
        report["attached"] = True
    else:
        if not selected or selected["id"] != PREVIOUS_BUILD_ID:
            raise RuntimeError("Another build is selected; preserve the concurrent change.")
        if report["appStoreState"] not in {"WAITING_FOR_REVIEW", "READY_FOR_REVIEW", "PREPARE_FOR_SUBMISSION", "REJECTED", "DEVELOPER_REJECTED", "METADATA_REJECTED"}:
            raise RuntimeError("The version is in an unexpected review/release state; preserve it.")
        # One documented relationship change. No submission cancellation, prices or metadata edits.
        call("PATCH", f"/appStoreVersions/{version['id']}/relationships/build", {"data": {"type": "builds", "id": target["id"]}})
        current = get(f"/appStoreVersions/{version['id']}/build")["data"]
        if current["id"] != target["id"]:
            raise RuntimeError("Attached build did not verify.")
        report["attached"] = True
    report["selectedBuildId"] = target["id"]
    report["selectedBuildNumber"] = number
    report["appStoreState"] = get(f"/appStoreVersions/{version['id']}")["data"]["attributes"].get("appStoreState")
    print("VERIFIED_REVIEW " + json.dumps(report))
    return report


if __name__ == "__main__":
    main()
