#!/usr/bin/env python3
"""Audit/provision ten annual capacity levels in one Pigeonpost subscription group.

Legacy receipt bindings remain unchanged; removing unused products is a separate operation.
"""

import json
import os
import time
import hashlib
import urllib.parse
import urllib.request
import urllib.error
from pathlib import Path
from decimal import Decimal
from concurrent.futures import ThreadPoolExecutor
from cubemeld_iap import Client, ApiError, mint_token, validate_upload_operation

BUNDLE = "dev.pigeonpost.inbox"
APP_ID = "6803521541"
PRIMARY = "dev.pigeonpost.inbox.handle.yearly"
GROUP_NAME = "Pigeonpost handle plans"

def plan_manifest():
    return [{"product_id": f"dev.pigeonpost.inbox.handles.{n}.yearly", "capacity": n,
             "usd_yearly": 8 * n, "group_level": 11 - n} for n in range(1, 11)]

PRODUCTS = [item["product_id"] for item in plan_manifest()]
APPLY = os.environ.get("APPLY", "false").lower() == "true"


class CatalogClient(Client):
    def __init__(self):
        super().__init__()
        self.issued = time.monotonic()

    def call(self, *args, **kwargs):
        if time.monotonic() - self.issued > 900:
            self.token = mint_token()
            self.issued = time.monotonic()
        for attempt in range(5):
            try:
                return super().call(*args, **kwargs)
            except ApiError as error:
                if args[0] != "GET" or error.status not in (429, 500, 502, 503, 504) or attempt == 4:
                    raise
                time.sleep(2 ** attempt)

    def upload(self, operation, content):
        # The subscription API now returns this Apple host. Apply the shared method,
        # range and header validation too, without emitting its signed query string.
        parsed = urllib.parse.urlsplit(operation.get("url", ""))
        if parsed.hostname != "northamerica-1.object-storage.apple.com":
            return super().upload(operation, content)
        checked = dict(operation)
        checked["url"] = urllib.parse.urlunsplit(parsed._replace(netloc="validated.blobstore.apple.com"))
        method, _, headers, offset, length = validate_upload_operation(checked, len(content))
        if parsed.scheme != "https" or parsed.port not in (None, 443) or parsed.username or parsed.password or parsed.fragment:
            raise RuntimeError("Invalid Apple object storage upload destination")
        request = urllib.request.Request(operation["url"], data=content[offset:offset+length], method=method)
        for name, value in headers:
            request.add_header(name, value)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                response.read()
        except (urllib.error.URLError, urllib.error.HTTPError) as error:
            raise RuntimeError(f"Apple review image upload failed ({getattr(error, 'code', 'transport')})") from None


def rel(kind, identifier):
    return {"data": {"type": kind, "id": identifier}}


def create(client, kind, attributes, relationships):
    if not APPLY:
        raise RuntimeError("Read-only catalog audit attempted a mutation")
    return client.call("POST", f"/v1/{kind}", {"data": {
        "type": kind, "attributes": attributes, "relationships": relationships,
    }})["data"]


def usa_price(client, subscription, expected=8):
    result = client.call("GET", f"/v1/subscriptions/{subscription}/prices", params={
        "filter[territory]": "USA", "include": "subscriptionPricePoint", "limit": 200,
    })
    points = {p["id"]: p for p in result.get("included", []) if p["type"] == "subscriptionPricePoints"}
    values = [Decimal(points[p["relationships"]["subscriptionPricePoint"]["data"]["id"]]["attributes"]["customerPrice"]) for p in result["data"]]
    if any(value != Decimal(expected) for value in values):
        raise RuntimeError(f"Existing US billing differs from ${expected}/year: {subscription}")
    return values


def plan(client, subscription):
    plans = client.list_all(f"/v1/subscriptions/{subscription}/planAvailabilities", {"limit": 200})
    upfront = [p for p in plans if p["attributes"]["planType"] == "UPFRONT"]
    if not upfront:
        return None, []
    if len(upfront) != 1:
        raise RuntimeError("Ambiguous subscription availability")
    item = upfront[0]
    territories = client.list_all(f"/v1/subscriptionPlanAvailabilities/{item['id']}/availableTerritories", {"limit": 200})
    return item, sorted(t["id"] for t in territories)


def metadata(client, group, product, slot):
    locales = client.list_all(f"/v1/subscriptionGroups/{group}/subscriptionGroupLocalizations", {"limit": 50})
    if not any(x["attributes"]["locale"] == "en-US" for x in locales):
        create(client, "subscriptionGroupLocalizations", {"name": GROUP_NAME, "locale": "en-US"},
               {"subscriptionGroup": rel("subscriptionGroups", group)})
    locales = client.list_all(f"/v1/subscriptions/{product}/subscriptionLocalizations", {"limit": 50})
    if not any(x["attributes"]["locale"] == "en-US" for x in locales):
        create(client, "subscriptionLocalizations", {"name": f"{slot} {'name' if slot == 1 else 'names'} — yearly", "locale": "en-US",
               "description": f"One plan for up to {slot} personal names and inboxes."},
               {"subscription": rel("subscriptions", product)})
    if os.environ.get("SKIP_REVIEW_SCREENSHOT") == "true":
        return  # Provision pricing first; the normal audit/apply requires the matching new image.
    shot = client.call("GET", f"/v1/subscriptions/{product}/appStoreReviewScreenshot", allow_404=True)
    content = Path("apps/ios/Store/handle-plans-review.png").read_bytes()
    if not shot or not shot.get("data"):
        asset = create(client, "subscriptionAppStoreReviewScreenshots",
            {"fileName": "handle-plans-review.png", "fileSize": len(content)},
            {"subscription": rel("subscriptions", product)})
    else:
        asset = shot["data"]
    state = asset["attributes"].get("assetDeliveryState", {}).get("state")
    if state in ("AWAITING_UPLOAD", None):
        if asset["attributes"]["fileSize"] != len(content) or asset["attributes"]["fileName"] != "handle-plans-review.png":
            raise RuntimeError("Pending review asset differs from the prepared image")
        operations = asset["attributes"].get("uploadOperations", [])
        if not operations:
            raise RuntimeError("Apple has not supplied review upload operations")
        for operation in operations:
            client.upload(operation, content)
        client.call("PATCH", f"/v1/subscriptionAppStoreReviewScreenshots/{asset['id']}", {"data": {
            "type": "subscriptionAppStoreReviewScreenshots", "id": asset["id"],
            "attributes": {"uploaded": True, "sourceFileChecksum": hashlib.md5(content).hexdigest()},
        }})
    elif state not in ("COMPLETE", "UPLOAD_COMPLETE"):
        raise RuntimeError(f"Review image is incomplete for {product}; inspect before retry")


def provision_prices(client, product, territories, expected):
    existing = client.list_all(f"/v1/subscriptions/{product}/prices", {"limit": 200, "include": "territory"})
    present = {p["relationships"]["territory"]["data"]["id"] for p in existing}
    usa_price(client, product, expected)
    points = client.list_all(f"/v1/subscriptions/{product}/pricePoints", {
        "filter[territory]": "USA", "filter[planType]": "UPFRONT", "limit": 8000,
    })
    eight = [p for p in points if Decimal(p["attributes"]["customerPrice"]) == Decimal(expected)]
    if len(eight) != 1:
        raise RuntimeError(f"Expected one exact ${expected} price point for {product}")
    base = eight[0]
    # The live adjustedEqualizations endpoint accepts MONTHLY only. Annual up-front
    # billing uses ordinary equalizations after declaring its plan availability.
    equalized = client.list_all(f"/v1/subscriptionPricePoints/{base['id']}/equalizations", {
        "limit": 8000, "include": "territory", "filter[subscription]": product,
    })
    mapping = {p["relationships"]["territory"]["data"]["id"]: p for p in equalized}
    mapping["USA"] = base
    if set(territories) - mapping.keys():
        raise RuntimeError("Apple has no equalized prices for all required territories")
    def write_price(territory):
        create(client, "subscriptionPrices", {"startDate": None, "planType": "UPFRONT"}, {
            "subscription": rel("subscriptions", product),
            "subscriptionPricePoint": rel("subscriptionPricePoints", mapping[territory]["id"]),
        })
    # Separate territories are independent records. Bound concurrency; failed/ambiguous writes
    # stop the run, and a rerun reads existing rows before attempting any remaining territory.
    if "USA" not in present:
        write_price("USA")
    missing = [t for t in territories if t not in present and t != "USA"]
    print(json.dumps({"pricing": product, "missing_territories": len(missing)}), flush=True)
    with ThreadPoolExecutor(max_workers=6) as pool:
        list(pool.map(write_price, missing))


def run(client):
    apps = client.list_all("/v1/apps", {"filter[bundleId]": BUNDLE})
    if len(apps) != 1 or apps[0]["id"] != APP_ID:
        raise RuntimeError("Pigeonpost App Store identity does not match")
    groups = client.list_all(f"/v1/apps/{APP_ID}/subscriptionGroups", {"limit": 200})
    products = {}
    for group in groups:
        for product in client.list_all(f"/v1/subscriptionGroups/{group['id']}/subscriptions", {"limit": 100}):
            products[product["attributes"]["productId"]] = (group, product)
    primary_entry = products.get(PRIMARY)
    primary = primary_entry[1] if primary_entry else None
    if primary and (primary["id"] != "6803878071" or not usa_price(client, primary["id"])):
        raise RuntimeError("Existing primary subscription identity or price drift")
    primary_plan, territories = plan(client, primary["id"]) if primary else (None, [])
    # After unused legacy products are removed, the established first capacity level supplies
    # the availability baseline. Never use a same-named product from another group.
    if not primary_plan or not territories:
        fallback = products.get(PRODUCTS[0])
        if fallback and fallback[0]["attributes"]["referenceName"] == GROUP_NAME:
            primary_plan, territories = plan(client, fallback[1]["id"])
    if not primary_plan or "USA" not in territories:
        raise RuntimeError("No valid US availability reference")
    matches = [g for g in groups if g["attributes"]["referenceName"] == GROUP_NAME]
    if len(matches) > 1:
        raise RuntimeError("Duplicate plan group reference")
    group = matches[0] if matches else None
    if group is None and APPLY:
        group = create(client, "subscriptionGroups", {"referenceName": GROUP_NAME}, {"app": rel("apps", APP_ID)})
    for spec in plan_manifest():
        identifier, capacity, expected = spec["product_id"], spec["capacity"], spec["usd_yearly"]
        print(json.dumps({"preparing": identifier}), flush=True)
        entry = products.get(identifier)
        if entry is None:
            if not APPLY:
                print(json.dumps({"product": identifier, "status": "missing"}), flush=True)
                continue
            note = (f"Settings > Get a handle > All handle plans. Select {capacity} names — yearly, then Subscribe. "
                    f"This level includes up to {capacity} names in ONE subscription, USD {expected}/year total in the US. "
                    "All ten levels are visible on the same screen and share the Pigeonpost handle plans group. "
                    "After purchasing, scroll to Register a name, check availability and register each included name without another payment. "
                    "Upgrades replace the current level immediately; downgrades take effect at renewal. "
                    "Restore purchases is at the bottom of the same page. Use the app review demo account. "
                    "The complimentary demo mailbox does not consume plan capacity.")
            product = create(client, "subscriptions", {"name": f"Pigeonpost {capacity} names yearly", "productId": identifier,
                "subscriptionPeriod": "ONE_YEAR", "familySharable": False, "groupLevel": spec["group_level"], "reviewNote": note},
                {"group": rel("subscriptionGroups", group["id"])})
        else:
            actual_group, product = entry
            if not group or actual_group["id"] != group["id"]:
                raise RuntimeError("Plan product is in the wrong subscription group")
        attrs = product["attributes"]
        if attrs["subscriptionPeriod"] != "ONE_YEAR" or attrs["familySharable"]:
            raise RuntimeError("Subscription term or sharing differs")
        if APPLY:
            if attrs.get("multiSeatStatus") != "DISABLED" or attrs.get("marketSettings") != ["APP_STORE"]:
                client.call("PATCH", f"/v1/subscriptions/{product['id']}", {"data": {
                    "type": "subscriptions", "id": product["id"],
                    "attributes": {"multiSeatStatus": "DISABLED", "marketSettings": ["APP_STORE"]},
                }})
            if attrs["groupLevel"] != spec["group_level"]:
                client.call("PATCH", f"/v1/subscriptions/{product['id']}", {"data": {"type": "subscriptions", "id": product["id"], "attributes": {"groupLevel": spec["group_level"]}}})
            metadata(client, group["id"], product["id"], capacity)
            saved_plan, saved_territories = plan(client, product["id"])
            if saved_plan and saved_territories != territories:
                raise RuntimeError("Availability drift")
            if not saved_plan:
                create(client, "subscriptionPlanAvailabilities", {
                    "planType": "UPFRONT", "availableInNewTerritories": primary_plan["attributes"]["availableInNewTerritories"],
                }, {"subscription": rel("subscriptions", product["id"]),
                    "availableTerritories": {"data": [{"type": "territories", "id": t} for t in territories]}})
            provision_prices(client, product["id"], territories, expected)
        _, saved_territories = plan(client, product["id"])
        prices = usa_price(client, product["id"], expected)
        current = client.call("GET", f"/v1/subscriptions/{product['id']}")["data"]
        print(json.dumps({"product": identifier, "id": product["id"], "group": group["id"],
            "level": current["attributes"]["groupLevel"], "state": current["attributes"]["state"],
            "usd_yearly": list(map(str, prices)), "territories": len(saved_territories)}), flush=True)
        if APPLY and (not prices or saved_territories != territories):
            raise RuntimeError("Saved catalog is incomplete")


if __name__ == "__main__":
    run(CatalogClient())
