#!/usr/bin/env python3
"""Inspect or update the existing CosmosMap review with the validated build 33."""

import base64
import hashlib
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
PREVIOUS_BUILD_ID = "0e2928b8-8f0c-414c-bd8d-beeb86b90e63"
REVIEW_ID = "1e894035-0a6c-44a7-828e-16742b67bb1d"
ITEM_SET_SHA256 = "ec1d451998494a7cdfbe4cdd3e12a1243d6154fe7eb0afe3c3883439abb7740f"
RESOURCE_SET_SHA256 = "b2cd17741ac1bde626d06e65bfcad2bdc698641cc0593bfbd5696bb66e450f1b"


def withdraw_review(version_id):
    """Withdraw only the observed build-31 submission after build 33 validates."""
    version = get(f"/appStoreVersions/{version_id}")["data"]
    selected = get(f"/appStoreVersions/{version_id}/build")["data"]
    if not selected or selected["id"] != PREVIOUS_BUILD_ID:
        raise RuntimeError("Another build is selected; preserve the concurrent change.")
    items = get(f"/reviewSubmissions/{REVIEW_ID}/items", **{
        "limit": 200, "include": "appStoreVersion,inAppPurchaseVersion",
        "fields[reviewSubmissionItems]": "state,appStoreVersion,inAppPurchaseVersion",
    })["data"]
    ids = sorted(item["id"] for item in items)
    refs = []
    for item in items:
        values = [(key, value["data"]) for key, value in item.get("relationships", {}).items() if value.get("data")]
        if len(values) != 1:
            raise RuntimeError("Unknown review item relationship; preserve it.")
        key, resource = values[0]
        refs.append((key, resource["type"], resource["id"]))
    digest = hashlib.sha256("\n".join("|".join(ref) for ref in sorted(refs)).encode()).hexdigest()
    if len(ids) != 7 or len(set(ids)) != 7 or hashlib.sha256("\n".join(ids).encode()).hexdigest() != ITEM_SET_SHA256 or digest != RESOURCE_SET_SHA256:
        raise RuntimeError("The seven review resources changed; preserve them.")
    if ("appStoreVersion", "appStoreVersions", version_id) not in refs:
        raise RuntimeError("Another app version is under review; preserve it.")
    review = get(f"/reviewSubmissions/{REVIEW_ID}")["data"]
    active = [r for r in get(f"/apps/{APP_ID}/reviewSubmissions", limit=20)["data"]
              if r["attributes"].get("platform") in {None, "IOS"} and r["attributes"].get("state") != "COMPLETE"]
    state = review["attributes"]["state"]
    if state == "COMPLETE" and not active and version["attributes"]["appStoreState"] == "DEVELOPER_REJECTED":
        return {"reviewId": REVIEW_ID, "state": state, "withdrawn": True}
    if state != "WAITING_FOR_REVIEW" or version["attributes"]["appStoreState"] != "WAITING_FOR_REVIEW" or [r["id"] for r in active] != [REVIEW_ID]:
        raise RuntimeError("Review state changed; preserve the existing submission.")
    call("PATCH", f"/reviewSubmissions/{REVIEW_ID}", {"data": {
        "type": "reviewSubmissions", "id": REVIEW_ID, "attributes": {"canceled": True},
    }})
    for attempt in range(37):
        state = get(f"/reviewSubmissions/{REVIEW_ID}")["data"]["attributes"]["state"]
        if state == "COMPLETE":
            return {"reviewId": REVIEW_ID, "state": state, "withdrawn": True}
        if state not in {"WAITING_FOR_REVIEW", "CANCELING"}:
            raise RuntimeError("Unexpected withdrawal state; inspect before continuing: " + state)
        time.sleep(5)
    raise RuntimeError("Withdrawal is still processing; inspect before resubmitting.")



def resubmit_review(version_id, target_id):
    """Resubmit the withdrawn app and the same six purchase versions; resume partial drafts."""
    def read_items(review_id):
        return get(f"/reviewSubmissions/{review_id}/items", **{
            "limit": 200, "include": "appStoreVersion,inAppPurchaseVersion",
            "fields[reviewSubmissionItems]": "state,appStoreVersion,inAppPurchaseVersion",
        })["data"]

    def references(items):
        refs = set()
        types = {"appStoreVersion": "appStoreVersions", "inAppPurchaseVersion": "inAppPurchaseVersions"}
        for item in items:
            values = [(key, value["data"]) for key, value in item.get("relationships", {}).items() if value.get("data")]
            if len(values) != 1 or values[0][0] not in types or values[0][1]["type"] != types[values[0][0]]:
                raise RuntimeError("Unknown review item relationship; preserve it.")
            key, data = values[0]
            refs.add((key, data["type"], data["id"]))
        if len(refs) != len(items):
            raise RuntimeError("Duplicate review item references; inspect before continuing.")
        return refs

    original = read_items(REVIEW_ID)
    ids = sorted(item["id"] for item in original)
    if len(ids) != 7 or len(set(ids)) != 7 or hashlib.sha256("\n".join(ids).encode()).hexdigest() != ITEM_SET_SHA256:
        raise RuntimeError("The original seven review items changed; preserve them.")
    expected = references(original)
    app_refs = [ref for ref in expected if ref[0] == "appStoreVersion"]
    if app_refs != [("appStoreVersion", "appStoreVersions", version_id)] or sum(ref[0] == "inAppPurchaseVersion" for ref in expected) != 6:
        raise RuntimeError("The original app version and six purchase versions did not verify.")
    if get(f"/reviewSubmissions/{REVIEW_ID}")["data"]["attributes"]["state"] != "COMPLETE":
        raise RuntimeError("The prior review is still active; preserve it.")
    current_build = get(f"/appStoreVersions/{version_id}/build")["data"]["id"]
    if current_build not in {PREVIOUS_BUILD_ID, target_id}:
        raise RuntimeError("Another build is selected; preserve the concurrent change.")
    candidates = [row for row in get(f"/apps/{APP_ID}/reviewSubmissions", limit=20)["data"]
                  if row["id"] != REVIEW_ID and row["attributes"].get("platform") in {None, "IOS"}
                  and row["attributes"].get("state") != "COMPLETE"]
    if len(candidates) > 1:
        raise RuntimeError("Multiple active reviews exist; preserve them.")
    review_id = candidates[0]["id"] if candidates else None
    if review_id:
        current_state = candidates[0]["attributes"]["state"]
        found = references(read_items(review_id))
        if not found.issubset(expected):
            raise RuntimeError("Another review contains different items; preserve it.")
        if current_state in {"WAITING_FOR_REVIEW", "IN_REVIEW"}:
            if found != expected or current_build != target_id:
                raise RuntimeError("An active review differs from the intended update; preserve it.")
            return {"submitted": True, "reviewId": review_id, "state": current_state, "items": 7, "replacesReviewId": REVIEW_ID}
        if current_state != "READY_FOR_REVIEW":
            raise RuntimeError("The replacement review is in an unexpected state; preserve it.")
    version_state = get(f"/appStoreVersions/{version_id}")["data"]["attributes"]["appStoreState"]
    if version_state not in {"DEVELOPER_REJECTED", "PREPARE_FOR_SUBMISSION", "READY_FOR_REVIEW"}:
        raise RuntimeError("The app version is no longer editable; preserve it.")
    if current_build != target_id:
        call("PATCH", f"/appStoreVersions/{version_id}/relationships/build", {"data": {"type": "builds", "id": target_id}})
    if get(f"/appStoreVersions/{version_id}/build")["data"]["id"] != target_id:
        raise RuntimeError("Replacement build did not verify.")
    if not review_id:
        review_id = call("POST", "/reviewSubmissions", {"data": {
            "type": "reviewSubmissions", "attributes": {"platform": "IOS"},
            "relationships": {"app": {"data": {"type": "apps", "id": APP_ID}}},
        }})["data"]["id"]
        print("REVIEW_CHECKPOINT created " + review_id)
    for ref in sorted(expected):
        if get(f"/reviewSubmissions/{review_id}")["data"]["attributes"]["state"] != "READY_FOR_REVIEW":
            raise RuntimeError("Draft changed during preparation; preserve it.")
        found = references(read_items(review_id))
        if not found.issubset(expected):
            raise RuntimeError("Concurrent review items changed; preserve them.")
        if ref not in found:
            key, resource_type, resource_id = ref
            call("POST", "/reviewSubmissionItems", {"data": {"type": "reviewSubmissionItems", "relationships": {
                "reviewSubmission": {"data": {"type": "reviewSubmissions", "id": review_id}},
                key: {"data": {"type": resource_type, "id": resource_id}},
            }}})
    if references(read_items(review_id)) != expected or get(f"/appStoreVersions/{version_id}/build")["data"]["id"] != target_id:
        raise RuntimeError("The exact seven items and build 33 did not verify; draft remains available.")
    call("PATCH", f"/reviewSubmissions/{review_id}", {"data": {
        "type": "reviewSubmissions", "id": review_id, "attributes": {"submitted": True},
    }})
    for attempt in range(37):
        current_state = get(f"/reviewSubmissions/{review_id}")["data"]["attributes"]["state"]
        if current_state in {"WAITING_FOR_REVIEW", "IN_REVIEW"}:
            break
        if current_state != "READY_FOR_REVIEW":
            raise RuntimeError("Unexpected submission state: " + current_state)
        time.sleep(5)
    else:
        raise RuntimeError("Submission transition timed out; inspect before continuing.")
    if references(read_items(review_id)) != expected or get(f"/appStoreVersions/{version_id}/build")["data"]["id"] != target_id:
        raise RuntimeError("Final review items or selected build did not verify.")
    fingerprint = hashlib.sha256("\n".join("|".join(ref) for ref in sorted(expected)).encode()).hexdigest()
    return {"submitted": True, "reviewId": review_id, "state": current_state, "items": 7,
            "replacesReviewId": REVIEW_ID, "resourceSetSha256": fingerprint}


def main():
    action = os.environ.get("ACTION", "inspect")
    number = os.environ.get("BUILD_NUMBER", "33")
    if action not in {"inspect", "attach", "withdraw", "resubmit"} or number != "33":
        raise RuntimeError("Only inspection, attachment or resubmission of CosmosMap build 33 is supported.")
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
    review_resources = {}
    for row in reviews:
        if row["id"] == REVIEW_ID or row["attributes"].get("state") in {"READY_FOR_REVIEW", "WAITING_FOR_REVIEW", "UNRESOLVED_ISSUES", "IN_REVIEW"}:
            response = get(f"/reviewSubmissions/{row['id']}/items", **{
                "limit": 200, "include": "appStoreVersion,inAppPurchaseVersion",
                "fields[reviewSubmissionItems]": "state,appStoreVersion,inAppPurchaseVersion",
                "fields[appStoreVersions]": "versionString,platform,appStoreState",
                "fields[inAppPurchaseVersions]": "version,state,inAppPurchase",
            })
            items = response["data"]
            review_items[row["id"]] = [{"id": item["id"], "state": item["attributes"].get("state"), "relationships": {key: value.get("data") for key, value in item.get("relationships", {}).items()}} for item in items]
            review_resources[row["id"]] = response.get("included", [])
    report = {
        "appId": APP_ID, "versionId": version["id"], "version": VERSION,
        "appStoreState": version["attributes"].get("appStoreState"),
        "releaseType": version["attributes"].get("releaseType"),
        "selectedBuildId": selected["id"] if selected else None,
        "selectedBuildNumber": next((b["attributes"]["version"] for b in versions.get("included", []) if b["type"] == "builds" and selected and b["id"] == selected["id"]), None),
        "targetBuild": [{"id": b["id"], "number": b["attributes"]["version"], "processingState": b["attributes"]["processingState"], "expired": b["attributes"]["expired"], "usesNonExemptEncryption": b["attributes"].get("usesNonExemptEncryption")} for b in builds],
        "reviews": [{"id": row["id"], "state": row["attributes"].get("state"), "platform": row["attributes"].get("platform")} for row in reviews],
        "reviewItems": review_items,
        "reviewResources": review_resources,
        "action": action, "attached": False,
    }
    print(json.dumps(report, indent=2))
    if action == "inspect":
        return report
    if len(builds) != 1:
        raise RuntimeError("Build 33 is not uniquely available; review is unchanged.")
    target = builds[0]
    a = target["attributes"]
    pre = get(f"/builds/{target['id']}/preReleaseVersion")["data"]
    if a["processingState"] != "VALID" or a["expired"] or a.get("usesNonExemptEncryption") is not False or pre["attributes"]["version"] != VERSION:
        raise RuntimeError("Target build is not valid, unexpired, compliant version 1.11; review is unchanged.")
    if action == "withdraw":
        report["withdrawal"] = withdraw_review(version["id"])
        print("VERIFIED_REVIEW " + json.dumps(report))
        return report
    if action == "resubmit":
        report["submission"] = resubmit_review(version["id"], target["id"])
        report["attached"] = True
    elif selected and selected["id"] == target["id"]:
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
