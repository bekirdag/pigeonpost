#!/usr/bin/env python3
"""Audit or provision Cubemeld's immutable StoreKit consumable catalog.

The checked-in iOS manifest is the source of truth. APPLY=false performs a read-only
App Store Connect audit; APPLY=true creates missing products and fills only missing
metadata/prices. Existing immutable/type drift and existing price drift fail closed.
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
import uuid
from dataclasses import dataclass
from decimal import Decimal

API = "https://api.appstoreconnect.apple.com"
EXPECTED_BUNDLE_ID = "com.wodo.gamehub"
EXPECTED_TEAM_ID = "AH277897AV"
EXPECTED_PRODUCTS = {
    "com.bekirdag.cubeio.gold.100": (100, "0.99"),
    "com.bekirdag.cubeio.gold.1000": (1_000, "4.99"),
    "com.bekirdag.cubeio.gold.10000": (10_000, "19.99"),
    "com.bekirdag.cubeio.gold.100000": (100_000, "49.99"),
    "com.bekirdag.cubeio.gold.1000000": (1_000_000, "99.99"),
}
EDITABLE_VERSION_STATES = {"PREPARE_FOR_SUBMISSION"}

KEY = os.environ["KEY"]
KEY_ID = os.environ["KEY_ID"]
ISSUER_ID = os.environ["ISSUER_ID"]
APPLY = os.environ.get("APPLY", "false").lower() == "true"
MANIFEST_PATH = os.environ.get("MANIFEST_PATH", "cubemeld/fastlane/iap_products.json")
STOREKIT_PATH = os.environ.get("STOREKIT_PATH", "cubemeld/Cubeio/Resources/Cubemeld.storekit")


class ApiError(RuntimeError):
    def __init__(self, method: str, url: str, status: int, detail: str):
        super().__init__(f"{method} {url} -> {status} {detail}")
        self.status = status


@dataclass(frozen=True)
class Product:
    product_id: str
    reference_name: str
    gold_amount: int
    price_usd: Decimal
    display_name: str
    description: str


def _b64(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def _der_to_raw(der: bytes) -> bytes:
    """Convert OpenSSL's DER ECDSA signature to the 64-byte JWT r||s form."""
    if not der or der[0] != 0x30:
        raise RuntimeError("OpenSSL returned an invalid ECDSA signature")
    body = der[2:] if der[1] < 0x80 else der[2 + (der[1] & 0x7F) :]
    out = b""
    while body:
        if body[0] != 0x02:
            raise RuntimeError("OpenSSL returned an invalid ECDSA integer")
        length = body[1]
        value = body[2 : 2 + length].lstrip(b"\x00")
        out += value.rjust(32, b"\x00")
        body = body[2 + length :]
    if len(out) != 64:
        raise RuntimeError("OpenSSL returned an invalid ES256 signature length")
    return out


def mint_token() -> str:
    header = _b64(json.dumps({"alg": "ES256", "kid": KEY_ID, "typ": "JWT"}).encode())
    now = int(time.time())
    payload = _b64(
        json.dumps(
            {"iss": ISSUER_ID, "iat": now, "exp": now + 1200, "aud": "appstoreconnect-v1"}
        ).encode()
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


class Client:
    def __init__(self) -> None:
        self.token = mint_token()

    def call(self, method: str, path: str, body=None, params=None, allow_404=False):
        url = path if path.startswith("https://") else API + path
        if params:
            url += ("&" if "?" in url else "?") + urllib.parse.urlencode(params)
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(url, data=data, method=method)
        request.add_header("Authorization", f"Bearer {self.token}")
        if data is not None:
            request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                raw = response.read()
            return json.loads(raw) if raw else {}
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode(errors="replace")[:1200]
            if allow_404 and exc.code == 404:
                return None
            raise ApiError(method, url, exc.code, detail) from None

    def list_all(self, path: str, params=None):
        result = []
        response = self.call("GET", path, params=params)
        while True:
            result.extend(response.get("data", []))
            next_url = response.get("links", {}).get("next")
            if not next_url:
                return result
            response = self.call("GET", next_url)


def read_json(path: str):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def load_catalog() -> list[Product]:
    manifest = read_json(MANIFEST_PATH)
    storekit = read_json(STOREKIT_PATH)
    app = manifest.get("app", {})
    if app.get("bundleID") != EXPECTED_BUNDLE_ID or app.get("teamID") != EXPECTED_TEAM_ID:
        raise RuntimeError("Cubemeld IAP manifest app identity does not match the release contract")

    manifest_products = manifest.get("products", [])
    manifest_ids = {item.get("productID") for item in manifest_products}
    if manifest_ids != set(EXPECTED_PRODUCTS) or len(manifest_products) != len(EXPECTED_PRODUCTS):
        raise RuntimeError("Cubemeld IAP manifest must contain exactly the five approved product IDs")

    storekit_by_id = {item.get("productID"): item for item in storekit.get("products", [])}
    if set(storekit_by_id) != set(EXPECTED_PRODUCTS):
        raise RuntimeError("Cubemeld.storekit must contain exactly the five approved product IDs")

    products = []
    for item in manifest_products:
        product_id = item["productID"]
        expected_gold, expected_price = EXPECTED_PRODUCTS[product_id]
        if item.get("type") != "consumable":
            raise RuntimeError(f"{product_id}: only consumable products are approved")
        if item.get("goldAmount") != expected_gold or Decimal(item.get("priceUSD")) != Decimal(
            expected_price
        ):
            raise RuntimeError(f"{product_id}: manifest amount or price drifted from the release contract")

        store_product = storekit_by_id[product_id]
        if store_product.get("type") != "Consumable":
            raise RuntimeError(f"{product_id}: StoreKit type must be Consumable")
        if Decimal(store_product.get("displayPrice")) != Decimal(expected_price):
            raise RuntimeError(f"{product_id}: StoreKit and manifest prices differ")
        if store_product.get("referenceName") != item.get("referenceName"):
            raise RuntimeError(f"{product_id}: StoreKit and manifest reference names differ")
        localizations = store_product.get("localizations", [])
        english = next((loc for loc in localizations if loc.get("locale") == "en_US"), None)
        if english is None:
            raise RuntimeError(f"{product_id}: StoreKit en_US localization is required")
        products.append(
            Product(
                product_id=product_id,
                reference_name=item["referenceName"],
                gold_amount=expected_gold,
                price_usd=Decimal(expected_price),
                display_name=english["displayName"],
                description=english["description"],
            )
        )
    return products


def create_product(client: Client, app_id: str, product: Product):
    return client.call(
        "POST",
        "/v2/inAppPurchases",
        {
            "data": {
                "type": "inAppPurchases",
                "attributes": {
                    "name": product.reference_name,
                    "productId": product.product_id,
                    "inAppPurchaseType": "CONSUMABLE",
                    "reviewNote": f"Consumable credit of {product.gold_amount:,} Gold in Cubemeld.",
                },
                "relationships": {"app": {"data": {"type": "apps", "id": app_id}}},
            }
        },
    )["data"]


def ensure_parent_metadata(client: Client, remote, product: Product) -> None:
    attrs = remote.get("attributes", {})
    if attrs.get("productId") != product.product_id:
        raise RuntimeError(f"{product.product_id}: App Store product ID mismatch")
    if attrs.get("inAppPurchaseType") != "CONSUMABLE":
        raise RuntimeError(f"{product.product_id}: immutable App Store product type is not CONSUMABLE")
    desired_note = f"Consumable credit of {product.gold_amount:,} Gold in Cubemeld."
    changes = {}
    if attrs.get("name") != product.reference_name:
        changes["name"] = product.reference_name
    if attrs.get("reviewNote") != desired_note:
        changes["reviewNote"] = desired_note
    if not changes:
        return
    if not APPLY:
        print(f"  metadata: needs {', '.join(sorted(changes))}")
        return
    client.call(
        "PATCH",
        f"/v2/inAppPurchases/{remote['id']}",
        {"data": {"type": "inAppPurchases", "id": remote["id"], "attributes": changes}},
    )
    print(f"  metadata: updated {', '.join(sorted(changes))}")


def version_localizations(client: Client, version_id: str):
    return client.list_all(
        f"/v1/inAppPurchaseVersions/{version_id}/localizations", {"limit": 50}
    )


def localization_matches(localization, product: Product) -> bool:
    attrs = localization.get("attributes", {})
    return (
        attrs.get("locale") == "en-US"
        and attrs.get("name") == product.display_name
        and attrs.get("description") == product.description
    )


def ensure_localization(client: Client, iap_id: str, product: Product) -> None:
    versions = client.list_all(f"/v2/inAppPurchases/{iap_id}/versions", {"limit": 50})
    versions.sort(key=lambda item: item.get("attributes", {}).get("version", 0), reverse=True)
    for version in versions:
        if any(localization_matches(loc, product) for loc in version_localizations(client, version["id"])):
            print(f"  localization: en-US exact (version {version['attributes'].get('version')})")
            return

    if not APPLY:
        print("  localization: missing or drifted")
        return

    draft = next(
        (
            version
            for version in versions
            if version.get("attributes", {}).get("state") in EDITABLE_VERSION_STATES
        ),
        None,
    )
    if draft is None:
        draft = client.call(
            "POST",
            "/v1/inAppPurchaseVersions",
            {
                "data": {
                    "type": "inAppPurchaseVersions",
                    "relationships": {
                        "inAppPurchase": {
                            "data": {"type": "inAppPurchases", "id": iap_id}
                        }
                    },
                }
            },
        )["data"]
        print(f"  localization: created draft version {draft['attributes'].get('version')}")

    localizations = version_localizations(client, draft["id"])
    english = next(
        (loc for loc in localizations if loc.get("attributes", {}).get("locale") == "en-US"), None
    )
    attributes = {
        "locale": "en-US",
        "name": product.display_name,
        "description": product.description,
    }
    if english is None:
        client.call(
            "POST",
            "/v2/inAppPurchaseLocalizations",
            {
                "data": {
                    "type": "inAppPurchaseLocalizations",
                    "attributes": attributes,
                    "relationships": {
                        "version": {
                            "data": {"type": "inAppPurchaseVersions", "id": draft["id"]}
                        }
                    },
                }
            },
        )
        print("  localization: created en-US")
    else:
        attributes.pop("locale")
        client.call(
            "PATCH",
            f"/v2/inAppPurchaseLocalizations/{english['id']}",
            {
                "data": {
                    "type": "inAppPurchaseLocalizations",
                    "id": english["id"],
                    "attributes": attributes,
                }
            },
        )
        print("  localization: updated en-US")


def ensure_availability(client: Client, iap_id: str) -> None:
    territories = client.list_all("/v1/territories", {"limit": 200})
    territory_ids = {territory["id"] for territory in territories}
    if "USA" not in territory_ids or len(territory_ids) < 100:
        raise RuntimeError("App Store Connect returned an incomplete territory catalog")

    availability = client.call(
        "GET",
        f"/v2/inAppPurchases/{iap_id}/inAppPurchaseAvailability",
        allow_404=True,
    )
    available_ids = set()
    includes_new = False
    if availability is not None and availability.get("data"):
        availability_data = availability["data"]
        includes_new = availability_data.get("attributes", {}).get("availableInNewTerritories") is True
        available_ids = {
            territory["id"]
            for territory in client.list_all(
                f"/v1/inAppPurchaseAvailabilities/{availability_data['id']}/availableTerritories",
                {"limit": 200},
            )
        }
    if includes_new and available_ids == territory_ids:
        print(f"  availability: all {len(territory_ids)} territories plus future territories")
        return
    if not APPLY:
        print(
            f"  availability: {len(available_ids)}/{len(territory_ids)} territories; "
            f"future={includes_new}"
        )
        return

    client.call(
        "POST",
        "/v1/inAppPurchaseAvailabilities",
        {
            "data": {
                "type": "inAppPurchaseAvailabilities",
                "attributes": {"availableInNewTerritories": True},
                "relationships": {
                    "availableTerritories": {
                        "data": [
                            {"type": "territories", "id": territory_id}
                            for territory_id in sorted(territory_ids)
                        ]
                    },
                    "inAppPurchase": {
                        "data": {"type": "inAppPurchases", "id": iap_id}
                    },
                },
            }
        },
    )
    print(f"  availability: enabled all {len(territory_ids)} territories plus future territories")


def current_usa_price(client: Client, iap_id: str):
    schedule = client.call(
        "GET", f"/v2/inAppPurchases/{iap_id}/iapPriceSchedule", allow_404=True
    )
    if schedule is None or not schedule.get("data"):
        return None
    schedule_id = schedule["data"]["id"]
    prices = client.call(
        "GET",
        f"/v1/inAppPurchasePriceSchedules/{schedule_id}/manualPrices",
        params={
            "filter[territory]": "USA",
            "include": "inAppPurchasePricePoint",
            "fields[inAppPurchasePricePoints]": "customerPrice,territory",
            "fields[inAppPurchasePrices]": "startDate,endDate,inAppPurchasePricePoint,territory",
            "limit": 50,
        },
    )
    points = {
        item["id"]: item.get("attributes", {}).get("customerPrice")
        for item in prices.get("included", [])
        if item.get("type") == "inAppPurchasePricePoints"
    }
    current = [
        item
        for item in prices.get("data", [])
        if item.get("attributes", {}).get("startDate") is None
        and item.get("attributes", {}).get("endDate") is None
    ]
    if len(current) != 1:
        raise RuntimeError(f"{iap_id}: expected one current USA manual price, found {len(current)}")
    point_id = current[0].get("relationships", {}).get("inAppPurchasePricePoint", {}).get("data", {}).get(
        "id"
    )
    raw_price = points.get(point_id)
    if raw_price is None:
        raise RuntimeError(f"{iap_id}: current USA price point did not include customerPrice")
    return Decimal(raw_price)


def find_usa_price_point(client: Client, iap_id: str, wanted: Decimal) -> str:
    points = client.list_all(
        f"/v2/inAppPurchases/{iap_id}/pricePoints",
        {
            "filter[territory]": "USA",
            "fields[inAppPurchasePricePoints]": "customerPrice,territory",
            "limit": 200,
        },
    )
    matches = [
        point["id"]
        for point in points
        if Decimal(point.get("attributes", {}).get("customerPrice", "-1")) == wanted
    ]
    if len(matches) != 1:
        raise RuntimeError(f"{iap_id}: expected one USA {wanted} price point, found {len(matches)}")
    return matches[0]


def ensure_price(client: Client, iap_id: str, product: Product) -> None:
    current = current_usa_price(client, iap_id)
    if current is not None:
        if current != product.price_usd:
            raise RuntimeError(
                f"{product.product_id}: existing USA price is {current}, expected {product.price_usd}; "
                "refusing an unattended price change"
            )
        print(f"  price: USD {current} exact")
        return
    if not APPLY:
        print(f"  price: missing (wanted USD {product.price_usd})")
        return

    price_point_id = find_usa_price_point(client, iap_id, product.price_usd)
    manual_price_id = str(uuid.uuid4())
    client.call(
        "POST",
        "/v1/inAppPurchasePriceSchedules",
        {
            "data": {
                "type": "inAppPurchasePriceSchedules",
                "relationships": {
                    "inAppPurchase": {
                        "data": {"type": "inAppPurchases", "id": iap_id}
                    },
                    "baseTerritory": {"data": {"type": "territories", "id": "USA"}},
                    "manualPrices": {
                        "data": [{"type": "inAppPurchasePrices", "id": manual_price_id}]
                    },
                },
            },
            "included": [
                {
                    "type": "inAppPurchasePrices",
                    "id": manual_price_id,
                    "attributes": {"startDate": None},
                    "relationships": {
                        "inAppPurchaseV2": {
                            "data": {"type": "inAppPurchases", "id": iap_id}
                        },
                        "inAppPurchasePricePoint": {
                            "data": {
                                "type": "inAppPurchasePricePoints",
                                "id": price_point_id,
                            }
                        },
                    },
                }
            ],
        },
    )
    print(f"  price: created USD {product.price_usd} base schedule")


def main() -> None:
    products = load_catalog()
    client = Client()
    apps = client.list_all(
        "/v1/apps", {"filter[bundleId]": EXPECTED_BUNDLE_ID, "limit": 10}
    )
    if len(apps) != 1:
        raise RuntimeError(f"Expected one {EXPECTED_BUNDLE_ID} app, found {len(apps)}")
    app = apps[0]
    print(f"Mode: {'APPLY' if APPLY else 'AUDIT ONLY'}")
    print(f"App: {app.get('attributes', {}).get('name')} ({EXPECTED_BUNDLE_ID}) id={app['id']}")

    remote_products = client.list_all(
        f"/v1/apps/{app['id']}/inAppPurchasesV2", {"limit": 200}
    )
    by_product_id = {}
    for remote in remote_products:
        product_id = remote.get("attributes", {}).get("productId")
        if product_id in by_product_id:
            raise RuntimeError(f"Duplicate App Store product ID: {product_id}")
        by_product_id[product_id] = remote

    for product in products:
        print(f"\n{product.product_id}")
        remote = by_product_id.get(product.product_id)
        if remote is None:
            if not APPLY:
                print("  product: missing")
                continue
            remote = create_product(client, app["id"], product)
            print(f"  product: created id={remote['id']}")
        else:
            remote = client.call("GET", f"/v2/inAppPurchases/{remote['id']}")["data"]
            print(f"  product: exists id={remote['id']} state={remote['attributes'].get('state')}")

        ensure_parent_metadata(client, remote, product)
        ensure_availability(client, remote["id"])
        ensure_localization(client, remote["id"], product)
        ensure_price(client, remote["id"], product)

    if APPLY:
        print("\nApply completed. Apple says product-metadata changes can take up to one hour to appear in Sandbox.")
    else:
        print("\nAudit completed without changing App Store Connect.")


if __name__ == "__main__":
    try:
        main()
    except (ApiError, RuntimeError, ValueError, KeyError) as error:
        print(f"::error::{error}", file=sys.stderr)
        raise SystemExit(1)
