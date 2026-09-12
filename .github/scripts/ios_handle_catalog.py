#!/usr/bin/env python3
"""Inspect Pigeonpost's Apple handle subscriptions without exposing signing material."""

import json
import os
from cubemeld_iap import Client

BUNDLE = "dev.pigeonpost.inbox"
APP_ID = "6803521541"


def audit(client):
    apps = client.list_all("/v1/apps", {"filter[bundleId]": BUNDLE})
    if len(apps) != 1 or apps[0]["id"] != APP_ID:
        raise RuntimeError("Pigeonpost App Store identity does not match")
    groups = client.list_all(f"/v1/apps/{APP_ID}/subscriptionGroups", {"limit": 200})
    for group in groups:
        print(json.dumps({"group": group["id"], "attributes": group["attributes"]}))
        products = client.list_all(f"/v1/subscriptionGroups/{group['id']}/subscriptions", {"limit": 100})
        for product in products:
            print(json.dumps({"subscription": product["id"], "attributes": product["attributes"]}))
            prices = client.call("GET", f"/v1/subscriptions/{product['id']}/prices", params={
                "filter[territory]": "USA", "include": "subscriptionPricePoint", "limit": 200,
            })
            print(json.dumps({"usd_prices": prices}))


if __name__ == "__main__":
    if os.environ.get("APPLY", "false").lower() == "true":
        raise RuntimeError("Catalog mutation has not been implemented or validated yet")
    audit(Client())
