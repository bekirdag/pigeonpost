#!/usr/bin/env python3
"""Audit/provision the Pigeonpost Nests subscription group (Nests Phase B1).

Three monthly Nest packages in their own group, separate from the handle plans group. Read-only
audit by default; APPLY=true creates what is missing. Idempotent: a rerun reads existing rows
first. Nothing is submitted for review. The review screenshot is added later (Products page, B2),
so the products stay at MISSING_METADATA until then. Mirrors crates/pigeonpost-postbox/src/nest_catalog.rs.
"""

import json
import os
import sys
import time
from decimal import Decimal
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cubemeld_iap import Client, ApiError, mint_token  # noqa: E402

BUNDLE = "dev.pigeonpost.inbox"
APP_ID = "6803521541"
GROUP_NAME = "Pigeonpost Nests"
HANDLE_GROUP = "Pigeonpost handles"
HANDLE_REFERENCE = "dev.pigeonpost.inbox.handle.yearly"  # the live handle product
APPLY = os.environ.get("APPLY", "false").lower() == "true"

REVIEW_NOTE = (
    "Pigeonpost Nests are hosted AI agents. These subscriptions are prepared ahead of launch and are "
    "not yet sold in the app: the Nests feature is switched off for every account. They will be "
    "submitted with the app version that ships the Products page, together with a review screenshot "
    "and a demo account."
)


def manifest():
    """Highest capacity = level 1. Pro (10 slots) is not offered and not created."""
    return [
        {"product_id": "dev.pigeonpost.inbox.nests.ai.plus.monthly", "name": "Pigeonpost AI Plus monthly",
         "usd_monthly": "25.00", "group_level": 1,
         "display": "Pigeonpost AI Plus", "description": "3 AI agents with their own workspaces, monthly."},
        {"product_id": "dev.pigeonpost.inbox.nests.ai.starter.monthly", "name": "Pigeonpost AI monthly",
         "usd_monthly": "10.00", "group_level": 2,
         "display": "Pigeonpost AI", "description": "1 AI agent with its own workspace, monthly."},
        {"product_id": "dev.pigeonpost.inbox.nests.host.starter.monthly", "name": "Empty Nest monthly",
         "usd_monthly": "5.00", "group_level": 3,
         "display": "Empty Nest", "description": "1 private agent workspace, no AI included."},
    ]


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


def rel(kind, identifier):
    return {"data": {"type": kind, "id": identifier}}


def create(client, kind, attributes, relationships):
    if not APPLY:
        raise RuntimeError("Read-only catalog audit attempted a mutation")
    return client.call("POST", f"/v1/{kind}", {"data": {
        "type": kind, "attributes": attributes, "relationships": relationships,
    }})["data"]


def usa_prices(client, subscription):
    result = client.call("GET", f"/v1/subscriptions/{subscription}/prices", params={
        "filter[territory]": "USA", "include": "subscriptionPricePoint", "limit": 200,
    })
    points = {p["id"]: p for p in result.get("included", []) if p["type"] == "subscriptionPricePoints"}
    return [Decimal(points[p["relationships"]["subscriptionPricePoint"]["data"]["id"]]["attributes"]["customerPrice"])
            for p in result["data"]]


def availability(client, subscription):
    """The product's availability record (POST /v1/subscriptionAvailabilities), or None."""
    found = client.call("GET", f"/v1/subscriptions/{subscription}/subscriptionAvailability", allow_404=True)
    return found.get("data") if found else None


def priced_territories(client, subscription):
    """Territories with a price. Apple's availability territory listing reads empty for the live
    handle product even though it sells in 175 territories, so the priced set is the reference."""
    prices = client.list_all(f"/v1/subscriptions/{subscription}/prices", {"limit": 200, "include": "territory"})
    return sorted({p["relationships"]["territory"]["data"]["id"] for p in prices})


def check_description(spec):
    if len(spec["description"]) > 55:
        raise RuntimeError(f"Apple caps descriptions at 55 characters: {spec['product_id']}")


def metadata(client, group, product, spec):
    check_description(spec)
    locales = client.list_all(f"/v1/subscriptionGroups/{group}/subscriptionGroupLocalizations", {"limit": 50})
    if not any(x["attributes"]["locale"] == "en-US" for x in locales):
        create(client, "subscriptionGroupLocalizations", {"name": GROUP_NAME, "locale": "en-US"},
               {"subscriptionGroup": rel("subscriptionGroups", group)})
    locales = client.list_all(f"/v1/subscriptions/{product}/subscriptionLocalizations", {"limit": 50})
    if not any(x["attributes"]["locale"] == "en-US" for x in locales):
        create(client, "subscriptionLocalizations", {"name": spec["display"], "locale": "en-US",
               "description": spec["description"]}, {"subscription": rel("subscriptions", product)})


def price_point(client, product, expected):
    points = client.list_all(f"/v1/subscriptions/{product}/pricePoints", {
        "filter[territory]": "USA", "limit": 8000,
    })
    exact = [p for p in points if Decimal(p["attributes"]["customerPrice"]) == Decimal(expected)]
    if len(exact) == 1:
        return exact[0]
    below = sorted((p for p in points if Decimal(p["attributes"]["customerPrice"]) < Decimal(expected)),
                   key=lambda p: Decimal(p["attributes"]["customerPrice"]))
    if not below:
        raise RuntimeError(f"No USD price point at or below {expected} for {product}")
    print(json.dumps({"deviation": product, "wanted": expected, "used": below[-1]["attributes"]["customerPrice"]}), flush=True)
    return below[-1]


def provision_prices(client, product, territories, expected):
    existing = client.list_all(f"/v1/subscriptions/{product}/prices", {"limit": 200, "include": "territory"})
    present = {p["relationships"]["territory"]["data"]["id"] for p in existing}
    base = price_point(client, product, expected)
    current = usa_prices(client, product)
    if current and any(v != Decimal(base["attributes"]["customerPrice"]) for v in current):
        raise RuntimeError(f"Existing US price differs for {product}; reconcile by hand")
    equalized = client.list_all(f"/v1/subscriptionPricePoints/{base['id']}/equalizations", {
        "limit": 8000, "include": "territory", "filter[subscription]": product,
    })
    mapping = {p["relationships"]["territory"]["data"]["id"]: p for p in equalized}
    mapping["USA"] = base
    if set(territories) - mapping.keys():
        raise RuntimeError("Apple has no equalized prices for all required territories")

    def write_price(territory):
        create(client, "subscriptionPrices", {"startDate": None}, {
            "subscription": rel("subscriptions", product),
            "subscriptionPricePoint": rel("subscriptionPricePoints", mapping[territory]["id"]),
        })
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
    reference = products.get(HANDLE_REFERENCE)
    if not reference or reference[0]["attributes"]["referenceName"] != HANDLE_GROUP:
        raise RuntimeError("No handle plan to copy availability from")
    territories = priced_territories(client, reference[1]["id"])
    if "USA" not in territories:
        raise RuntimeError("No valid US availability reference")
    matches = [g for g in groups if g["attributes"]["referenceName"] == GROUP_NAME]
    if len(matches) > 1:
        raise RuntimeError("Duplicate Nests group reference")
    group = matches[0] if matches else None
    if group is None and APPLY:
        group = create(client, "subscriptionGroups", {"referenceName": GROUP_NAME}, {"app": rel("apps", APP_ID)})
    print(json.dumps({"group": group["id"] if group else None}), flush=True)
    for spec in manifest():
        identifier = spec["product_id"]
        check_description(spec)
        entry = products.get(identifier)
        if entry is None:
            if not APPLY:
                print(json.dumps({"product": identifier, "status": "missing"}), flush=True)
                continue
            product = create(client, "subscriptions", {
                "name": spec["name"], "productId": identifier, "subscriptionPeriod": "ONE_MONTH",
                "familySharable": False, "groupLevel": spec["group_level"], "reviewNote": REVIEW_NOTE,
            }, {"group": rel("subscriptionGroups", group["id"])})
        else:
            actual_group, product = entry
            if not group or actual_group["id"] != group["id"]:
                raise RuntimeError("Nest product is in the wrong subscription group")
        attrs = product["attributes"]
        if attrs["subscriptionPeriod"] != "ONE_MONTH" or attrs["familySharable"]:
            raise RuntimeError("Subscription term or sharing differs")
        if APPLY:
            changes = {}
            if attrs.get("multiSeatStatus") != "DISABLED":
                changes["multiSeatStatus"] = "DISABLED"
            if attrs.get("marketSettings") != ["APP_STORE"]:
                changes["marketSettings"] = ["APP_STORE"]
            if attrs["groupLevel"] != spec["group_level"]:
                changes["groupLevel"] = spec["group_level"]
            if changes:
                client.call("PATCH", f"/v1/subscriptions/{product['id']}", {"data": {
                    "type": "subscriptions", "id": product["id"], "attributes": changes}})
            metadata(client, group["id"], product["id"], spec)
            # Availability first: without it Apple refuses prices with a misleading 409.
            if not availability(client, product["id"]):
                create(client, "subscriptionAvailabilities", {"availableInNewTerritories": False}, {
                    "subscription": rel("subscriptions", product["id"]),
                    "availableTerritories": {"data": [{"type": "territories", "id": t} for t in territories]}})
            provision_prices(client, product["id"], territories, spec["usd_monthly"])
        saved_territories = priced_territories(client, product["id"])
        current = client.call("GET", f"/v1/subscriptions/{product['id']}")["data"]
        prices = usa_prices(client, product["id"])
        print(json.dumps({"product": identifier, "id": product["id"], "group": group["id"],
            "level": current["attributes"]["groupLevel"], "state": current["attributes"]["state"],
            "usd_monthly": list(map(str, prices)), "territories": len(saved_territories)}), flush=True)
        if APPLY and (not prices or saved_territories != territories):
            raise RuntimeError("Saved catalog is incomplete")


if __name__ == "__main__":
    run(CatalogClient())
