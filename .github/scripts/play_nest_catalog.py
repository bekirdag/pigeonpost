#!/usr/bin/env python3
"""Audit/provision the Google Play Nest package subscriptions (Nests Phase B1).

Read-only by default; APPLY=true creates what is missing. Idempotent. Base plans are created
and left INACTIVE: nothing is purchasable until someone activates them deliberately.

Credentials: GOOGLE_PLAY_SERVICE_ACCOUNT_FILE (service-account JSON). Key material is never
printed; the access token stays in memory.
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

PACKAGE = "dev.pigeonpost.inbox"
BASE_PLAN = "monthly"
API = f"https://androidpublisher.googleapis.com/androidpublisher/v3/applications/{PACKAGE}"
APPLY = os.environ.get("APPLY", "false").lower() == "true"

# Prices approved by the owner on 2026-11-08 (build plan §12). Pro is not sold (capacity).
CATALOG = [
    {"product_id": "pigeonpost.nests.ai.starter", "title": "Pigeonpost AI", "usd": "10.00",
     "benefits": ["One AI agent in a private workspace", "Managed AI included, no key needed"],
     "description": "Your own AI agent in a private cloud workspace, reached in Pigeonpost."},
    {"product_id": "pigeonpost.nests.ai.plus", "title": "Pigeonpost AI Plus", "usd": "25.00",
     "benefits": ["Three AI agents with private workspaces", "Larger managed AI allowance"],
     "description": "Three AI agents in private cloud workspaces, with a larger AI allowance."},
    {"product_id": "pigeonpost.nests.host.starter", "title": "Empty Nest", "usd": "5.00",
     "benefits": ["One private cloud workspace for an agent", "Connect your own AI later"],
     "description": "A private cloud workspace for one agent. No AI model is included."},
]


def b64(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def token():
    with open(os.environ["GOOGLE_PLAY_SERVICE_ACCOUNT_FILE"]) as fh:
        key = json.load(fh)
    now = int(time.time())
    header = b64(json.dumps({"alg": "RS256", "typ": "JWT", "kid": key["private_key_id"]}).encode())
    claims = b64(json.dumps({"iss": key["client_email"], "scope": "https://www.googleapis.com/auth/androidpublisher",
                             "aud": "https://oauth2.googleapis.com/token", "iat": now, "exp": now + 3600}).encode())
    signing_input = f"{header}.{claims}".encode()
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "key.pem")
        with open(os.open(path, os.O_WRONLY | os.O_CREAT, 0o600), "w") as fh:
            fh.write(key["private_key"])
        sig = subprocess.run(["openssl", "dgst", "-sha256", "-sign", path], input=signing_input,
                             capture_output=True, check=True).stdout
    del key
    body = urllib.parse.urlencode({"grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
                                   "assertion": f"{header}.{claims}.{b64(sig)}"}).encode()
    with urllib.request.urlopen("https://oauth2.googleapis.com/token", data=body, timeout=30) as r:
        return json.load(r)["access_token"]


class Play:
    def __init__(self):
        self.bearer = token()

    def call(self, method, path, body=None, params=None, allow_404=False):
        url = API + path + ("?" + urllib.parse.urlencode(params) if params else "")
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Authorization", f"Bearer {self.bearer}")
        if data is not None:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                raw = r.read()
                return json.loads(raw) if raw else {}
        except urllib.error.HTTPError as e:
            if allow_404 and e.code == 404:
                return None
            detail = e.read().decode(errors="replace")[:600]
            raise RuntimeError(f"{method} {path}: HTTP {e.code}: {detail}") from None


def money(usd):
    units, nanos = usd.split(".")
    return {"currencyCode": "USD", "units": units, "nanos": int(nanos.ljust(9, "0"))}


def regional(play, usd):
    converted = play.call("POST", "/pricing:convertRegionPrices", {"price": money(usd)})
    configs = [{"regionCode": code, "price": value["price"], "newSubscriberAvailability": True}
               for code, value in sorted(converted.get("convertedRegionPrices", {}).items())]
    # Conversion rounds to local price points (10.00 becomes 9.99); the US price is the approved one.
    for config in configs:
        if config["regionCode"] == "US":
            config["price"] = money(usd)
    return configs, converted.get("regionVersion", {}).get("version")


def run():
    play = Play()
    existing = {}
    page = None
    while True:
        res = play.call("GET", "/subscriptions", params={"pageSize": 100, **({"pageToken": page} if page else {})})
        for sub in res.get("subscriptions", []):
            existing[sub["productId"]] = sub
        page = res.get("nextPageToken")
        if not page:
            break
    # Mirror grace/hold settings from an existing handle subscription, if any.
    reference = next((s for pid, s in existing.items() if pid.startswith("pigeonpost.handle.")), None)
    ref_plan = (reference or {}).get("basePlans", [{}])[0].get("autoRenewingBasePlanType", {})
    for spec in CATALOG:
        pid = spec["product_id"]
        sub = existing.get(pid)
        if sub is None:
            if not APPLY:
                print(json.dumps({"product": pid, "status": "missing"}), flush=True)
                continue
            configs, version = regional(play, spec["usd"])
            plan_type = {"billingPeriodDuration": "P1M", "resubscribeState": "RESUBSCRIBE_STATE_ACTIVE",
                         "prorationMode": "SUBSCRIPTION_PRORATION_MODE_CHARGE_ON_NEXT_BILLING_DATE"}
            for key in ("gracePeriodDuration", "accountHoldDuration"):
                if ref_plan.get(key):
                    plan_type[key] = ref_plan[key]
            body = {
                "packageName": PACKAGE, "productId": pid,
                "listings": [{"languageCode": "en-US", "title": spec["title"],
                              "benefits": spec["benefits"], "description": spec["description"]}],
                "basePlans": [{"basePlanId": BASE_PLAN, "autoRenewingBasePlanType": plan_type,
                               "regionalConfigs": configs,
                               "otherRegionsConfig": {"usdPrice": money(spec["usd"]),
                                                      "eurPrice": next((c["price"] for c in configs if c["regionCode"] == "DE"), None),
                                                      "newSubscriberAvailability": True}}],
            }
            if body["basePlans"][0]["otherRegionsConfig"]["eurPrice"] is None:
                del body["basePlans"][0]["otherRegionsConfig"]
            sub = play.call("POST", "/subscriptions", body,
                            params={"productId": pid, "regionsVersion.version": version})
        plans = sub.get("basePlans", [])
        if len(plans) != 1 or plans[0]["basePlanId"] != BASE_PLAN:
            raise RuntimeError(f"{pid}: unexpected base plans")
        plan = plans[0]
        if plan.get("autoRenewingBasePlanType", {}).get("billingPeriodDuration") != "P1M":
            raise RuntimeError(f"{pid}: billing period differs")
        us = next((c for c in plan.get("regionalConfigs", []) if c["regionCode"] == "US"), None)
        us_price = None
        if us:
            p = us["price"]
            us_price = f"{p.get('units', '0')}.{str(p.get('nanos', 0)).rjust(9, '0')[:2]}"
            if us_price != spec["usd"] and APPLY and plan.get("state") != "ACTIVE":
                us["price"] = money(spec["usd"])
                _, version = regional(play, spec["usd"])
                play.call("PATCH", f"/subscriptions/{pid}", sub,
                          params={"updateMask": "basePlans", "regionsVersion.version": version})
                us_price = spec["usd"]
            elif us_price != spec["usd"]:
                raise RuntimeError(f"{pid}: US price {us_price} differs from {spec['usd']}")
        print(json.dumps({"product": pid, "base_plan": plan["basePlanId"], "state": plan.get("state"),
                          "usd": us_price, "regions": len(plan.get("regionalConfigs", []))}), flush=True)
        if plan.get("state") == "ACTIVE":
            raise RuntimeError(f"{pid}: base plan is ACTIVE; this script never activates")


if __name__ == "__main__":
    try:
        run()
    except RuntimeError as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)
