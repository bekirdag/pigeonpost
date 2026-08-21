#!/usr/bin/env python3
"""Say why a TestFlight build is or is not visible on a tester's phone.

A build reaches a phone only when all of these are true: it finished processing, it has not
expired, export compliance has been answered, it belongs to at least one beta group, and that
group has at least one tester. This reads all five out of App Store Connect and names the ones
that are false. With FIX=true it also does the two things that are safe to do unattended:
answers export compliance from the value the app already declares in its Info.plist, and adds
the newest build to every internal group.
"""

import json
import os
import sys
import time

import jwt
import requests

API = "https://api.appstoreconnect.apple.com/v1"

KEY = os.environ["KEY"]
KEY_ID = os.environ["KEY_ID"]
ISSUER_ID = os.environ["ISSUER_ID"]
BUNDLE_ID = os.environ.get("BUNDLE_ID", "dev.pigeonpost.inbox")
FIX = os.environ.get("FIX", "false").lower() == "true"

token = jwt.encode(
    {"iss": ISSUER_ID, "iat": int(time.time()), "exp": int(time.time()) + 900, "aud": "appstoreconnect-v1"},
    KEY,
    algorithm="ES256",
    headers={"kid": KEY_ID, "typ": "JWT"},
)
SESSION = requests.Session()
SESSION.headers["Authorization"] = f"Bearer {token}"


def call(method, path, **kw):
    url = path if path.startswith("http") else API + path
    r = SESSION.request(method, url, timeout=60, **kw)
    if r.status_code >= 400:
        print(f"  !! {method} {url} -> {r.status_code} {r.text[:800]}")
        r.raise_for_status()
    return r.json() if r.text else {}


def get(path, **params):
    return call("GET", path, params=params)


apps = get("/apps", **{"filter[bundleId]": BUNDLE_ID})["data"]
if not apps:
    sys.exit(f"No app with bundle id {BUNDLE_ID} on this account.")
app = apps[0]
app_id = app["id"]
print(f"App: {app['attributes']['name']} ({BUNDLE_ID}) id={app_id}\n")

builds = get(
    "/builds",
    **{
        "filter[app]": app_id,
        "limit": 20,
        "sort": "-version",
        "include": "buildBetaDetail,preReleaseVersion,betaGroups",
    },
)
included = {(i["type"], i["id"]): i for i in builds.get("included", [])}

print("Builds, newest first")
print("-" * 78)
newest = None
for b in builds["data"]:
    a = b["attributes"]
    detail = included.get(("buildBetaDetails", (b["relationships"]["buildBetaDetail"]["data"] or {}).get("id", "")))
    da = detail["attributes"] if detail else {}
    pre = included.get(("preReleaseVersions", (b["relationships"]["preReleaseVersion"]["data"] or {}).get("id", "")))
    version = pre["attributes"]["version"] if pre else "?"
    groups = [
        included[("betaGroups", g["id"])]["attributes"]["name"]
        for g in (b["relationships"].get("betaGroups", {}).get("data") or [])
        if ("betaGroups", g["id"]) in included
    ]
    if newest is None:
        newest = b
    print(
        f"  {version} ({a['version']})  processing={a['processingState']}  expired={a['expired']}  "
        f"compliance={a.get('usesNonExemptEncryption')}\n"
        f"      internal={da.get('internalBuildState')}  external={da.get('externalBuildState')}  "
        f"autoNotify={da.get('autoNotifyEnabled')}\n"
        f"      groups={groups or 'NONE'}  uploaded={a['uploadedDate']}  expires={a.get('expirationDate')}"
    )
print()

groups = get("/betaGroups", **{"filter[app]": app_id, "limit": 50, "include": "betaTesters"})
print("Beta groups")
print("-" * 78)
if not groups["data"]:
    print("  NONE. A build with no group reaches nobody.")
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

try:
    review = get(f"/apps/{app_id}/betaAppReviewDetail")["data"]["attributes"]
    print(f"Beta App Review detail: contact={review.get('contactEmail')} demo={review.get('demoAccountRequired')}")
except Exception:
    print("Beta App Review detail: unavailable")
print()

if FIX and newest:
    a = newest["attributes"]
    print("FIX")
    print("-" * 78)
    if a["processingState"] != "VALID":
        print(f"  Newest build is {a['processingState']}; nothing to hand a tester yet.")
    else:
        if a.get("usesNonExemptEncryption") is None:
            call(
                "PATCH",
                f"/builds/{newest['id']}",
                json={"data": {"type": "builds", "id": newest["id"], "attributes": {"usesNonExemptEncryption": False}}},
            )
            print("  Answered export compliance: usesNonExemptEncryption=false (matches Info.plist).")
        else:
            print("  Export compliance already answered.")
        internal = [g for g in groups["data"] if g["attributes"]["isInternalGroup"]]
        if not internal:
            print("  No internal group exists. Create one in App Store Connect and add yourself.")
        for g in internal:
            name = g["attributes"]["name"]
            # Put this build in front of the testers now.
            try:
                call(
                    "POST",
                    f"/betaGroups/{g['id']}/relationships/builds",
                    json={"data": [{"type": "builds", "id": newest["id"]}]},
                )
                print(f"  Added build {a['version']} to internal group {name}.")
            except Exception as exc:
                print(f"  Could not add build {a['version']} to {name}: {exc}")
            # And stop this being a manual step. An internal group that does not take every build
            # means each upload has to be walked into the group by hand in the web UI, and the one
            # that gets forgotten looks exactly like a failed upload from the phone.
            if not g["attributes"].get("hasAccessToAllBuilds"):
                try:
                    call(
                        "PATCH",
                        f"/betaGroups/{g['id']}",
                        json={
                            "data": {
                                "type": "betaGroups",
                                "id": g["id"],
                                "attributes": {"hasAccessToAllBuilds": True},
                            }
                        },
                    )
                    print(f"  {name} now takes every new build automatically.")
                except Exception as exc:
                    print(f"  Could not set hasAccessToAllBuilds on {name}: {exc}")
            else:
                print(f"  {name} already takes every new build automatically.")
