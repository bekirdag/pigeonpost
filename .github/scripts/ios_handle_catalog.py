#!/usr/bin/env python3
"""Provision only Pigeonpost's nine additional independent $8/year handle slots.

The original product and existing prices remain read-only. No public review submission.
"""

import json
import os
import time
import hashlib
from pathlib import Path
from decimal import Decimal
from cubemeld_iap import Client, mint_token

BUNDLE = "dev.pigeonpost.inbox"
APP_ID = "6803521541"
PRIMARY = "dev.pigeonpost.inbox.handle.yearly"
PRODUCTS = [PRIMARY] + [f"dev.pigeonpost.inbox.handle{n}.yearly" for n in range(2, 11)]
APPLY = os.environ.get("APPLY", "false").lower() == "true"


class CatalogClient(Client):
    def __init__(self):
        super().__init__()
        self.issued = time.monotonic()

    def call(self, *args, **kwargs):
        if time.monotonic() - self.issued > 900:
            self.token = mint_token()
            self.issued = time.monotonic()
        return super().call(*args, **kwargs)


def rel(kind, identifier):
    return {"data": {"type": kind, "id": identifier}}


def create(client, kind, attributes, relationships):
    if not APPLY:
        raise RuntimeError("Read-only catalog audit attempted a mutation")
    return client.call("POST", f"/v1/{kind}", {"data": {
        "type": kind, "attributes": attributes, "relationships": relationships,
    }})["data"]


def usa_price(client, subscription):
    result = client.call("GET", f"/v1/subscriptions/{subscription}/prices", params={
        "filter[territory]": "USA", "include": "subscriptionPricePoint", "limit": 200,
    })
    points = {p["id"]: p for p in result.get("included", []) if p["type"] == "subscriptionPricePoints"}
    values = [Decimal(points[p["relationships"]["subscriptionPricePoint"]["data"]["id"]]["attributes"]["customerPrice"]) for p in result["data"]]
    if any(value != Decimal("8") for value in values):
        raise RuntimeError(f"Existing US billing differs from $8/year: {subscription}")
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
        create(client, "subscriptionGroupLocalizations", {"name": f"Pigeonpost handle {slot}", "locale": "en-US"},
               {"subscriptionGroup": rel("subscriptionGroups", group)})
    locales = client.list_all(f"/v1/subscriptions/{product}/subscriptionLocalizations", {"limit": 50})
    if not any(x["attributes"]["locale"] == "en-US" for x in locales):
        create(client, "subscriptionLocalizations", {"name": f"Handle {slot} — yearly", "locale": "en-US",
               "description": "Register one personal name with its own inbox for one year."},
               {"subscription": rel("subscriptions", product)})
    shot = client.call("GET", f"/v1/subscriptions/{product}/appStoreReviewScreenshot", allow_404=True)
    if not shot or not shot.get("data"):
        content = Path("apps/ios/Store/handle-subscription-review.png").read_bytes()
        asset = create(client, "subscriptionAppStoreReviewScreenshots",
            {"fileName": "handle-subscription-review.png", "fileSize": len(content)},
            {"subscription": rel("subscriptions", product)})
        for operation in asset["attributes"]["uploadOperations"]:
            client.upload(operation, content)
        client.call("PATCH", f"/v1/subscriptionAppStoreReviewScreenshots/{asset['id']}", {"data": {
            "type": "subscriptionAppStoreReviewScreenshots", "id": asset["id"],
            "attributes": {"uploaded": True, "sourceFileChecksum": hashlib.md5(content).hexdigest()},
        }})
    elif shot["data"]["attributes"].get("assetDeliveryState", {}).get("state") not in ("COMPLETE", "UPLOAD_COMPLETE"):
        raise RuntimeError(f"Review image is incomplete for {product}; inspect before retry")


def provision_prices(client, product, territories):
    existing = client.list_all(f"/v1/subscriptions/{product}/prices", {"limit": 200})
    present = {p["relationships"]["territory"]["data"]["id"] for p in existing}
    usa_price(client, product)
    points = client.list_all(f"/v1/subscriptions/{product}/pricePoints", {
        "filter[territory]": "USA", "filter[planType]": "UPFRONT", "limit": 8000,
    })
    eight = [p for p in points if Decimal(p["attributes"]["customerPrice"]) == Decimal("8")]
    if len(eight) != 1:
        raise RuntimeError(f"Expected one exact $8 price point for {product}")
    base = eight[0]
    equalized = client.list_all(f"/v1/subscriptionPricePoints/{base['id']}/equalizations", {"limit": 8000})
    mapping = {p["relationships"]["territory"]["data"]["id"]: p for p in equalized}
    mapping["USA"] = base
    if set(territories) - mapping.keys():
        raise RuntimeError("Apple has no equalized prices for all required territories")
    for territory in territories:
        if territory in present:
            continue
        create(client, "subscriptionPrices", {"startDate": None, "preserveCurrentPrice": False, "planType": "UPFRONT"}, {
            "subscription": rel("subscriptions", product),
            "subscriptionPricePoint": rel("subscriptionPricePoints", mapping[territory]["id"]),
            "territory": rel("territories", territory),
        })


def run(client):
    apps = client.list_all("/v1/apps", {"filter[bundleId]": BUNDLE})
    if len(apps) != 1 or apps[0]["id"] != APP_ID:
        raise RuntimeError("Pigeonpost App Store identity does not match")
    groups = client.list_all(f"/v1/apps/{APP_ID}/subscriptionGroups", {"limit": 200})
    products = {}
    for group in groups:
        for product in client.list_all(f"/v1/subscriptionGroups/{group['id']}/subscriptions", {"limit": 100}):
            products[product["attributes"]["productId"]] = (group, product)
    primary = products[PRIMARY][1]
    if primary["id"] != "6803878071" or not usa_price(client, primary["id"]):
        raise RuntimeError("Existing primary subscription identity or price drift")
    primary_plan, territories = plan(client, primary["id"])
    if not primary_plan or "USA" not in territories:
        raise RuntimeError("Existing primary subscription has no US availability")
    print(json.dumps({"primary": PRIMARY, "usd_yearly": 8, "territories": territories}), flush=True)
    for slot, identifier in enumerate(PRODUCTS, 1):
        if identifier not in products:
            if not APPLY:
                print(json.dumps({"product": identifier, "status": "missing"}), flush=True)
                continue
            reference = f"Pigeonpost handle {slot}"
            matches = [g for g in groups if g["attributes"]["referenceName"] == reference]
            if len(matches) > 1:
                raise RuntimeError("Duplicate handle group reference")
            group = matches[0] if matches else create(client, "subscriptionGroups", {"referenceName": reference},
                {"app": rel("apps", APP_ID)})
            if client.list_all(f"/v1/subscriptionGroups/{group['id']}/subscriptions", {"limit": 100}):
                raise RuntimeError("A handle slot group already contains a different subscription")
            product = create(client, "subscriptions", {"name": reference, "productId": identifier,
                "subscriptionPeriod": "ONE_YEAR", "familySharable": False, "groupLevel": 1,
                "reviewNote": "Settings > Your handles. Each subscription registers one name and its inbox. Up to ten independently renewable names; $8/year each in the US. Restore purchases recovers existing names for the signed-in account."},
                {"group": rel("subscriptionGroups", group["id"])})
            products[identifier] = (group, product)
        group, product = products[identifier]
        attrs = product["attributes"]
        if attrs["subscriptionPeriod"] != "ONE_YEAR" or attrs["familySharable"] or attrs["groupLevel"] != 1:
            raise RuntimeError(f"Subscription configuration drift: {identifier}")
        if slot > 1 and APPLY:
            metadata(client, group["id"], product["id"], slot)
            provision_prices(client, product["id"], territories)
            saved_plan, saved_territories = plan(client, product["id"])
            if saved_plan and saved_territories != territories:
                raise RuntimeError(f"Availability drift: {identifier}")
            if not saved_plan:
                create(client, "subscriptionPlanAvailabilities", {
                    "planType": "UPFRONT", "availableInNewTerritories": primary_plan["attributes"]["availableInNewTerritories"],
                }, {"subscription": rel("subscriptions", product["id"]),
                    "availableTerritories": {"data": [{"type": "territories", "id": t} for t in territories]}})
        _, saved_territories = plan(client, product["id"])
        saved_prices = usa_price(client, product["id"])
        current = client.call("GET", f"/v1/subscriptions/{product['id']}")["data"]
        print(json.dumps({"product": identifier, "id": product["id"], "group": group["id"],
            "state": current["attributes"]["state"], "usd_yearly": list(map(str, saved_prices)),
            "available_territory_count": len(saved_territories)}), flush=True)
        if APPLY and (not saved_prices or saved_territories != territories):
            raise RuntimeError(f"Saved catalog is incomplete: {identifier}")
    if APPLY and len({g["id"] for g, p in products.values() if p["attributes"]["productId"] in PRODUCTS}) != 10:
        raise RuntimeError("Ten independent subscription groups were not configured")


if __name__ == "__main__":
    run(CatalogClient())
