#!/usr/bin/env python3
"""Audit or provision Cubemeld's immutable StoreKit consumable catalog.

The checked-in iOS manifest is the source of truth. APPLY=false performs a read-only
App Store Connect audit; APPLY=true creates missing products and fills only missing
metadata/prices/review screenshots. Existing immutable/type drift, existing price
drift, incomplete asset uploads, and unproven local review assets fail closed.
"""

import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zlib
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path

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
REVIEW_SCREENSHOT_TYPE = "inAppPurchaseAppStoreReviewScreenshots"
MAX_REVIEW_SCREENSHOT_BYTES = 50 * 1024 * 1024
REVIEW_SCREENSHOT_POLL_ATTEMPTS = 45
REVIEW_SCREENSHOT_POLL_INTERVAL_SECONDS = 2
# Apple says an IAP review image may use any screenshot size the app supports. Cubemeld is an
# iPhone game, so this allowlist mirrors Apple's current iPhone screenshot specification. Keeping
# it explicit makes a newly introduced size a reviewed broker change instead of an implicit upload.
IPHONE_SCREENSHOT_DIMENSIONS = {
    (1260, 2736),
    (2736, 1260),
    (1290, 2796),
    (2796, 1290),
    (1320, 2868),
    (2868, 1320),
    (1284, 2778),
    (2778, 1284),
    (1242, 2688),
    (2688, 1242),
    (1179, 2556),
    (2556, 1179),
    (1206, 2622),
    (2622, 1206),
    (1170, 2532),
    (2532, 1170),
    (1125, 2436),
    (2436, 1125),
    (1080, 2340),
    (2340, 1080),
    (1242, 2208),
    (2208, 1242),
    (750, 1334),
    (1334, 750),
    (640, 1096),
    (640, 1136),
    (1136, 600),
    (1136, 640),
    (640, 920),
    (640, 960),
    (960, 600),
    (960, 640),
}

KEY = os.environ["KEY"]
KEY_ID = os.environ["KEY_ID"]
ISSUER_ID = os.environ["ISSUER_ID"]
APPLY = os.environ.get("APPLY", "false").lower() == "true"
MANIFEST_PATH = os.environ.get("MANIFEST_PATH", "cubemeld/fastlane/iap_products.json")
STOREKIT_PATH = os.environ.get("STOREKIT_PATH", "cubemeld/Cubeio/Resources/Cubemeld.storekit")
REVIEW_SCREENSHOT_MANIFEST_PATH = os.environ.get(
    "REVIEW_SCREENSHOT_MANIFEST_PATH",
    "cubemeld/fastlane/iap_review_screenshot.json",
)


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


@dataclass(frozen=True)
class ReviewScreenshot:
    path: Path
    file_name: str
    content: bytes
    sha256: str


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
        if path.startswith("https://"):
            parsed = urllib.parse.urlsplit(path)
            if (
                parsed.scheme != "https"
                or parsed.hostname != "api.appstoreconnect.apple.com"
                or parsed.port not in (None, 443)
                or parsed.username is not None
                or parsed.password is not None
                or parsed.fragment
            ):
                raise RuntimeError("App Store Connect pagination returned an untrusted API URL")
            url = path
        else:
            if not path.startswith("/"):
                raise RuntimeError("App Store Connect API paths must be absolute")
            url = API + path
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

    def upload(self, operation, content: bytes) -> None:
        """Perform one Apple-issued, unauthenticated asset upload operation."""
        method, url, headers, offset, length = validate_upload_operation(operation, len(content))
        request = urllib.request.Request(
            url,
            data=content[offset : offset + length],
            method=method,
        )
        for name, value in headers:
            request.add_header(name, value)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                response.read()
        except urllib.error.HTTPError as exc:
            # Apple's upload URL contains a time-limited signature. Never emit its query string.
            parsed = urllib.parse.urlsplit(url)
            safe_url = urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path, "", ""))
            detail = exc.read().decode(errors="replace")[:1200]
            raise ApiError(method, safe_url, exc.code, detail) from None
        except urllib.error.URLError:
            raise RuntimeError(
                "Apple screenshot blob upload failed at the transport layer"
            ) from None


def read_json(path: str):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def _git_tracked_bytes(path: Path) -> tuple[Path, bytes]:
    """Return bytes only when ``path`` is the exact regular file committed at HEAD."""
    if path.is_symlink() or not path.is_file():
        raise RuntimeError(f"Review screenshot input is not a regular file: {path}")
    resolved = path.resolve()
    root_result = subprocess.run(
        ["git", "-C", str(resolved.parent), "rev-parse", "--show-toplevel"],
        capture_output=True,
        check=False,
        text=True,
    )
    if root_result.returncode != 0:
        raise RuntimeError(f"Review screenshot input is not inside a Git checkout: {path}")
    repository_root = Path(root_result.stdout.strip()).resolve()
    try:
        relative = resolved.relative_to(repository_root)
    except ValueError:
        raise RuntimeError(f"Review screenshot input escapes its Git checkout: {path}") from None
    committed = subprocess.run(
        ["git", "-C", str(repository_root), "cat-file", "blob", f"HEAD:{relative.as_posix()}"],
        capture_output=True,
        check=False,
    )
    if committed.returncode != 0:
        raise RuntimeError(f"Review screenshot input is not tracked at HEAD: {relative}")
    current = resolved.read_bytes()
    if current != committed.stdout:
        raise RuntimeError(f"Review screenshot input differs from HEAD: {relative}")
    return repository_root, current


def _png_dimensions(content: bytes) -> tuple[int, int]:
    """Validate a flattened, noninterlaced 8-bit RGB PNG and return its dimensions."""
    signature = b"\x89PNG\r\n\x1a\n"
    if not content.startswith(signature):
        raise RuntimeError("Review screenshot must be a PNG file")
    offset = len(signature)
    ihdr = None
    idat_parts = []
    saw_iend = False
    while offset < len(content):
        if offset + 12 > len(content):
            raise RuntimeError("Review screenshot PNG is truncated")
        chunk_length = int.from_bytes(content[offset : offset + 4], "big")
        chunk_type = content[offset + 4 : offset + 8]
        chunk_end = offset + 12 + chunk_length
        if chunk_end > len(content):
            raise RuntimeError("Review screenshot PNG contains a truncated chunk")
        chunk_data = content[offset + 8 : offset + 8 + chunk_length]
        expected_crc = int.from_bytes(content[offset + 8 + chunk_length : chunk_end], "big")
        actual_crc = zlib.crc32(chunk_type + chunk_data) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            raise RuntimeError("Review screenshot PNG contains an invalid chunk checksum")
        if ihdr is None and chunk_type != b"IHDR":
            raise RuntimeError("Review screenshot PNG does not begin with IHDR")
        if chunk_type == b"IHDR":
            if ihdr is not None or chunk_length != 13:
                raise RuntimeError("Review screenshot PNG has an invalid IHDR")
            ihdr = chunk_data
        elif chunk_type == b"IDAT":
            idat_parts.append(chunk_data)
        elif chunk_type == b"tRNS":
            raise RuntimeError("Review screenshot PNG must not contain transparency")
        elif chunk_type == b"IEND":
            if chunk_length != 0 or chunk_end != len(content):
                raise RuntimeError("Review screenshot PNG has an invalid IEND")
            saw_iend = True
        offset = chunk_end
    if ihdr is None or not idat_parts or not saw_iend:
        raise RuntimeError("Review screenshot PNG is missing required image chunks")

    width = int.from_bytes(ihdr[0:4], "big")
    height = int.from_bytes(ihdr[4:8], "big")
    bit_depth, color_type, compression, filtering, interlace = ihdr[8:13]
    if (bit_depth, color_type, compression, filtering, interlace) != (8, 2, 0, 0, 0):
        raise RuntimeError(
            "Review screenshot PNG must be flattened, noninterlaced 8-bit RGB without alpha"
        )
    if (width, height) not in IPHONE_SCREENSHOT_DIMENSIONS:
        raise RuntimeError(
            f"Review screenshot dimensions {width}x{height} are not an accepted iPhone "
            "screenshot size"
        )

    expected_length = height * (1 + width * 3)
    inflater = zlib.decompressobj()
    try:
        decoded = inflater.decompress(b"".join(idat_parts), expected_length + 1)
    except zlib.error as error:
        raise RuntimeError(f"Review screenshot PNG image data is invalid: {error}") from None
    if (
        len(decoded) != expected_length
        or not inflater.eof
        or inflater.unconsumed_tail
        or inflater.unused_data
    ):
        raise RuntimeError("Review screenshot PNG image data has an invalid length")
    row_length = 1 + width * 3
    if any(decoded[row * row_length] > 4 for row in range(height)):
        raise RuntimeError("Review screenshot PNG uses an invalid row filter")
    return width, height


def load_review_screenshot() -> ReviewScreenshot:
    manifest_path = Path(REVIEW_SCREENSHOT_MANIFEST_PATH)
    repository_root, manifest_content = _git_tracked_bytes(manifest_path)
    try:
        manifest = json.loads(manifest_content)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"Review screenshot manifest is invalid JSON: {error}") from None
    required_keys = {"schemaVersion", "file", "sha256", "productIDs"}
    if not isinstance(manifest, dict) or set(manifest) != required_keys:
        raise RuntimeError(
            "Review screenshot manifest must contain exactly schemaVersion, file, sha256, "
            "and productIDs"
        )
    if manifest["schemaVersion"] != 1:
        raise RuntimeError("Review screenshot manifest schemaVersion must be 1")
    product_ids = manifest["productIDs"]
    if (
        not isinstance(product_ids, list)
        or not all(isinstance(item, str) for item in product_ids)
        or len(product_ids) != len(EXPECTED_PRODUCTS)
        or set(product_ids) != set(EXPECTED_PRODUCTS)
    ):
        raise RuntimeError("Review screenshot manifest must name exactly the five Gold products")
    relative_file = manifest["file"]
    if (
        not isinstance(relative_file, str)
        or not relative_file
        or "\\" in relative_file
        or Path(relative_file).is_absolute()
        or ".." in Path(relative_file).parts
    ):
        raise RuntimeError("Review screenshot manifest file must be a safe relative path")
    screenshot_path = (manifest_path.parent / relative_file).resolve()
    try:
        screenshot_path.relative_to(repository_root)
    except ValueError:
        raise RuntimeError("Review screenshot file escapes the Cubemeld checkout") from None
    screenshot_root, content = _git_tracked_bytes(screenshot_path)
    if screenshot_root != repository_root:
        raise RuntimeError("Review screenshot and manifest must belong to the same Git checkout")
    if not (0 < len(content) <= MAX_REVIEW_SCREENSHOT_BYTES):
        raise RuntimeError("Review screenshot file has an unsafe byte size")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,126}\.png", screenshot_path.name):
        raise RuntimeError("Review screenshot must have a safe .png filename")
    wanted_sha256 = manifest["sha256"]
    actual_sha256 = hashlib.sha256(content).hexdigest()
    if not isinstance(wanted_sha256, str) or not re.fullmatch(r"[0-9a-f]{64}", wanted_sha256):
        raise RuntimeError("Review screenshot manifest sha256 must be lowercase hexadecimal")
    if actual_sha256 != wanted_sha256:
        raise RuntimeError("Review screenshot SHA-256 does not match its committed manifest")
    _png_dimensions(content)
    return ReviewScreenshot(
        path=screenshot_path,
        file_name=screenshot_path.name,
        content=content,
        sha256=actual_sha256,
    )


def validate_upload_operation(operation, total_size: int):
    if not isinstance(operation, dict):
        raise RuntimeError("Apple returned a malformed screenshot upload operation")
    method = operation.get("method")
    offset = operation.get("offset")
    length = operation.get("length")
    url = operation.get("url")
    headers = operation.get("requestHeaders")
    if method != "PUT":
        raise RuntimeError("Apple returned a non-PUT screenshot upload operation")
    if (
        isinstance(offset, bool)
        or not isinstance(offset, int)
        or isinstance(length, bool)
        or not isinstance(length, int)
        or offset < 0
        or length <= 0
        or offset + length > total_size
    ):
        raise RuntimeError("Apple returned an out-of-bounds screenshot upload operation")
    if not isinstance(url, str):
        raise RuntimeError("Apple returned a screenshot upload operation without a URL")
    parsed = urllib.parse.urlsplit(url)
    hostname = parsed.hostname or ""
    if (
        parsed.scheme != "https"
        or not hostname.endswith(".blobstore.apple.com")
        or parsed.port not in (None, 443)
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
        or not parsed.query
    ):
        raise RuntimeError("Apple returned a screenshot upload URL outside its HTTPS blob store")
    if not isinstance(headers, list) or len(headers) > 32:
        raise RuntimeError("Apple returned malformed screenshot upload headers")
    validated_headers = []
    forbidden_headers = {
        "authorization",
        "proxy-authorization",
        "cookie",
        "host",
        "content-length",
        "transfer-encoding",
        "connection",
    }
    for header in headers:
        if not isinstance(header, dict) or set(header) != {"name", "value"}:
            raise RuntimeError("Apple returned a malformed screenshot upload header")
        name, value = header["name"], header["value"]
        if (
            not isinstance(name, str)
            or not re.fullmatch(r"[!#$%&'*+.^_`|~0-9A-Za-z-]+", name)
            or name.lower() in forbidden_headers
            or not isinstance(value, str)
            or len(value) > 8192
            or any(ord(character) < 32 and character != "\t" for character in value)
            or "\x7f" in value
        ):
            raise RuntimeError("Apple returned an unsafe screenshot upload header")
        validated_headers.append((name, value))
    return method, url, validated_headers, offset, length


def ordered_upload_operations(operations, total_size: int):
    if not isinstance(operations, list) or not operations:
        raise RuntimeError("Apple returned no screenshot upload operations")
    validated = [validate_upload_operation(operation, total_size) for operation in operations]
    ordered = sorted(zip(operations, validated), key=lambda item: item[1][3])
    cursor = 0
    for _, (_, _, _, offset, length) in ordered:
        if offset != cursor:
            raise RuntimeError("Apple screenshot upload operations overlap or leave a byte gap")
        cursor += length
    if cursor != total_size:
        raise RuntimeError("Apple screenshot upload operations do not cover the complete file")
    return [operation for operation, _ in ordered]


def load_catalog() -> list[Product]:
    manifest = read_json(MANIFEST_PATH)
    storekit = read_json(STOREKIT_PATH)
    app = manifest.get("app", {})
    if app.get("bundleID") != EXPECTED_BUNDLE_ID or app.get("teamID") != EXPECTED_TEAM_ID:
        raise RuntimeError("Cubemeld IAP manifest app identity does not match the release contract")

    manifest_products = manifest.get("products", [])
    manifest_ids = {item.get("productID") for item in manifest_products}
    if manifest_ids != set(EXPECTED_PRODUCTS) or len(manifest_products) != len(EXPECTED_PRODUCTS):
        raise RuntimeError(
            "Cubemeld IAP manifest must contain exactly the five approved product IDs"
        )

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
            raise RuntimeError(
                f"{product_id}: manifest amount or price drifted from the release contract"
            )

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


def get_review_screenshot(client: Client, iap_id: str):
    return client.call(
        "GET",
        f"/v2/inAppPurchases/{iap_id}/appStoreReviewScreenshot",
        params={
            "fields[inAppPurchaseAppStoreReviewScreenshots]": (
                "fileSize,fileName,sourceFileChecksum,assetType,assetDeliveryState"
            )
        },
        allow_404=True,
    )


def review_screenshot_state(response, product_id: str):
    if response is None:
        return "MISSING", None
    if not isinstance(response, dict):
        raise RuntimeError(f"{product_id}: App Store returned malformed review screenshot data")
    if "data" not in response:
        raise RuntimeError(f"{product_id}: App Store returned malformed review screenshot data")
    if response["data"] is None:
        return "MISSING", None
    screenshot = response.get("data")
    if not isinstance(screenshot, dict):
        raise RuntimeError(f"{product_id}: App Store returned malformed review screenshot data")
    if screenshot.get("type") != REVIEW_SCREENSHOT_TYPE or not isinstance(
        screenshot.get("id"), str
    ):
        raise RuntimeError(f"{product_id}: App Store returned the wrong review screenshot resource")
    attrs = screenshot.get("attributes")
    if not isinstance(attrs, dict):
        raise RuntimeError(f"{product_id}: App Store omitted review screenshot attributes")
    delivery = attrs.get("assetDeliveryState")
    if not isinstance(delivery, dict):
        raise RuntimeError(f"{product_id}: App Store omitted the review screenshot delivery state")
    state = delivery.get("state")
    if state not in {"AWAITING_UPLOAD", "UPLOAD_COMPLETE", "COMPLETE", "FAILED"}:
        raise RuntimeError(
            f"{product_id}: App Store returned an unknown screenshot state {state!r}"
        )
    errors = delivery.get("errors", [])
    warnings = delivery.get("warnings", [])
    if not isinstance(errors, list) or not isinstance(warnings, list):
        raise RuntimeError(f"{product_id}: App Store returned malformed screenshot diagnostics")
    if state == "FAILED" or errors:
        codes = sorted(
            {
                str(error.get("code", "UNKNOWN"))
                for error in errors
                if isinstance(error, dict)
            }
        )
        detail = ",".join(codes) if codes else "UNKNOWN"
        raise RuntimeError(f"{product_id}: review screenshot processing failed ({detail})")
    if state == "COMPLETE":
        file_name = attrs.get("fileName")
        file_size = attrs.get("fileSize")
        checksum = attrs.get("sourceFileChecksum")
        if (
            not isinstance(file_name, str)
            or not file_name.lower().endswith((".png", ".jpg", ".jpeg"))
            or isinstance(file_size, bool)
            or not isinstance(file_size, int)
            or file_size <= 0
            or not isinstance(checksum, str)
            or not re.fullmatch(r"[0-9a-fA-F]{32}", checksum)
            or attrs.get("assetType") != "SCREENSHOT"
        ):
            raise RuntimeError(
                f"{product_id}: completed review screenshot metadata is incomplete or malformed"
            )
    return state, screenshot


def ensure_review_screenshot(
    client: Client,
    iap_id: str,
    product_id: str,
    current_response,
    local_screenshot: ReviewScreenshot | None,
) -> bool:
    state, current = review_screenshot_state(current_response, product_id)
    if state == "COMPLETE":
        attrs = current["attributes"]
        print(
            f"  review screenshot: complete ({attrs['fileName']}, {attrs['fileSize']} bytes)"
        )
        return True
    if state != "MISSING":
        print(f"  review screenshot: incomplete remote upload ({state})")
        if APPLY:
            raise RuntimeError(
                f"{product_id}: refusing to replace or resume an incomplete review screenshot; "
                "resolve it in App Store Connect"
            )
        return False
    if not APPLY:
        print("  review screenshot: missing")
        return False
    if local_screenshot is None:
        raise RuntimeError(f"{product_id}: missing committed review screenshot artifact")

    reservation_response = client.call(
        "POST",
        "/v1/inAppPurchaseAppStoreReviewScreenshots",
        {
            "data": {
                "type": REVIEW_SCREENSHOT_TYPE,
                "attributes": {
                    "fileSize": len(local_screenshot.content),
                    "fileName": local_screenshot.file_name,
                },
                "relationships": {
                    "inAppPurchaseV2": {
                        "data": {"type": "inAppPurchases", "id": iap_id}
                    }
                },
            }
        },
    )
    if not isinstance(reservation_response, dict):
        raise RuntimeError(f"{product_id}: App Store returned a malformed screenshot reservation")
    reservation_state, reservation = review_screenshot_state(
        reservation_response, product_id
    )
    if reservation_state != "AWAITING_UPLOAD":
        raise RuntimeError(f"{product_id}: App Store returned a non-awaiting screenshot reservation")
    attrs = reservation.get("attributes")
    if not isinstance(attrs, dict):
        raise RuntimeError(f"{product_id}: App Store omitted screenshot reservation attributes")
    delivery = attrs.get("assetDeliveryState")
    if (
        attrs.get("fileName") != local_screenshot.file_name
        or attrs.get("fileSize") != len(local_screenshot.content)
        or attrs.get("assetType") != "SCREENSHOT"
        or not isinstance(delivery, dict)
    ):
        raise RuntimeError(f"{product_id}: App Store returned a mismatched screenshot reservation")
    operations = ordered_upload_operations(
        attrs.get("uploadOperations"), len(local_screenshot.content)
    )
    for operation in operations:
        client.upload(operation, local_screenshot.content)

    screenshot_id = reservation["id"]
    checksum = hashlib.md5(local_screenshot.content, usedforsecurity=False).hexdigest()
    client.call(
        "PATCH",
        f"/v1/inAppPurchaseAppStoreReviewScreenshots/{screenshot_id}",
        {
            "data": {
                "type": REVIEW_SCREENSHOT_TYPE,
                "id": screenshot_id,
                "attributes": {"uploaded": True, "sourceFileChecksum": checksum},
            }
        },
    )

    for attempt in range(REVIEW_SCREENSHOT_POLL_ATTEMPTS):
        response = client.call(
            "GET", f"/v1/inAppPurchaseAppStoreReviewScreenshots/{screenshot_id}"
        )
        uploaded_state, uploaded = review_screenshot_state(response, product_id)
        if uploaded_state == "COMPLETE":
            uploaded_attrs = uploaded["attributes"]
            if (
                uploaded.get("id") != screenshot_id
                or uploaded.get("type") != REVIEW_SCREENSHOT_TYPE
                or uploaded_attrs.get("fileName") != local_screenshot.file_name
                or uploaded_attrs.get("fileSize") != len(local_screenshot.content)
                or uploaded_attrs.get("sourceFileChecksum", "").lower() != checksum
            ):
                raise RuntimeError(
                    f"{product_id}: completed review screenshot does not match the upload"
                )
            print(
                "  review screenshot: uploaded and processed "
                f"(sha256={local_screenshot.sha256})"
            )
            return True
        if uploaded_state not in {"AWAITING_UPLOAD", "UPLOAD_COMPLETE"}:
            raise RuntimeError(
                f"{product_id}: screenshot entered unexpected state {uploaded_state}"
            )
        if attempt + 1 < REVIEW_SCREENSHOT_POLL_ATTEMPTS:
            time.sleep(REVIEW_SCREENSHOT_POLL_INTERVAL_SECONDS)
    raise RuntimeError(f"{product_id}: timed out waiting for screenshot processing")


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


def ensure_parent_metadata(client: Client, remote, product: Product) -> bool:
    attrs = remote.get("attributes", {})
    if attrs.get("productId") != product.product_id:
        raise RuntimeError(f"{product.product_id}: App Store product ID mismatch")
    if attrs.get("inAppPurchaseType") != "CONSUMABLE":
        raise RuntimeError(
            f"{product.product_id}: immutable App Store product type is not CONSUMABLE"
        )
    desired_note = f"Consumable credit of {product.gold_amount:,} Gold in Cubemeld."
    changes = {}
    if attrs.get("name") != product.reference_name:
        changes["name"] = product.reference_name
    if attrs.get("reviewNote") != desired_note:
        changes["reviewNote"] = desired_note
    if not changes:
        print("  metadata: exact")
        return True
    if not APPLY:
        print(f"  metadata: needs {', '.join(sorted(changes))}")
        return False
    client.call(
        "PATCH",
        f"/v2/inAppPurchases/{remote['id']}",
        {"data": {"type": "inAppPurchases", "id": remote["id"], "attributes": changes}},
    )
    print(f"  metadata: updated {', '.join(sorted(changes))}")
    return True


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


def ensure_localization(client: Client, iap_id: str, product: Product) -> bool:
    versions = client.list_all(f"/v2/inAppPurchases/{iap_id}/versions", {"limit": 50})
    versions.sort(key=lambda item: item.get("attributes", {}).get("version", 0), reverse=True)
    for version in versions:
        if any(
            localization_matches(loc, product)
            for loc in version_localizations(client, version["id"])
        ):
            print(f"  localization: en-US exact (version {version['attributes'].get('version')})")
            return True

    if not APPLY:
        print("  localization: missing or drifted")
        return False

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
    return True


def ensure_availability(client: Client, iap_id: str) -> bool:
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
        includes_new = (
            availability_data.get("attributes", {}).get("availableInNewTerritories") is True
        )
        available_ids = {
            territory["id"]
            for territory in client.list_all(
                f"/v1/inAppPurchaseAvailabilities/{availability_data['id']}/availableTerritories",
                {"limit": 200},
            )
        }
    if includes_new and available_ids == territory_ids:
        print(f"  availability: all {len(territory_ids)} territories plus future territories")
        return True
    if not APPLY:
        print(
            f"  availability: {len(available_ids)}/{len(territory_ids)} territories; "
            f"future={includes_new}"
        )
        return False

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
    return True


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
    if not current:
        return None
    if len(current) != 1:
        raise RuntimeError(f"{iap_id}: expected one current USA manual price, found {len(current)}")
    point_id = (
        current[0]
        .get("relationships", {})
        .get("inAppPurchasePricePoint", {})
        .get("data", {})
        .get("id")
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


def ensure_price(client: Client, iap_id: str, product: Product) -> bool:
    current = current_usa_price(client, iap_id)
    if current is not None:
        if current != product.price_usd:
            raise RuntimeError(
                f"{product.product_id}: existing USA price is {current}, "
                f"expected {product.price_usd}; "
                "refusing an unattended price change"
            )
        print(f"  price: USD {current} exact")
        return True
    if not APPLY:
        print(f"  price: missing (wanted USD {product.price_usd})")
        return False

    price_point_id = find_usa_price_point(client, iap_id, product.price_usd)
    # App Store Connect requires a literal JSON:API local identifier for an inline resource.
    manual_price_id = "${price1}"
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
    return True


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

    # Audit every screenshot relationship before any write. If apply mode would need an upload,
    # the committed artifact is also verified before any product metadata can be mutated.
    remote_details = {}
    screenshot_responses = {}
    needs_local_screenshot = False
    for product in products:
        summary = by_product_id.get(product.product_id)
        if summary is None:
            needs_local_screenshot = True
            continue
        remote = client.call("GET", f"/v2/inAppPurchases/{summary['id']}")["data"]
        remote_details[product.product_id] = remote
        screenshot_response = get_review_screenshot(client, remote["id"])
        screenshot_responses[product.product_id] = screenshot_response
        screenshot_state, _ = review_screenshot_state(screenshot_response, product.product_id)
        if screenshot_state == "MISSING":
            needs_local_screenshot = True
        elif APPLY and screenshot_state != "COMPLETE":
            raise RuntimeError(
                f"{product.product_id}: refusing all mutations while its review screenshot "
                f"is {screenshot_state}"
            )

    local_screenshot = None
    if APPLY and needs_local_screenshot:
        local_screenshot = load_review_screenshot()
        print(
            "Review screenshot artifact: committed RGB PNG "
            f"{local_screenshot.file_name} (sha256={local_screenshot.sha256})"
        )

    ready = True
    for product in products:
        print(f"\n{product.product_id}")
        remote = remote_details.get(product.product_id)
        if remote is None:
            if not APPLY:
                print("  product: missing")
                ready = False
                continue
            remote = create_product(client, app["id"], product)
            print(f"  product: created id={remote['id']}")
        else:
            print(f"  product: exists id={remote['id']} state={remote['attributes'].get('state')}")

        checks = [
            ensure_parent_metadata(client, remote, product),
            ensure_availability(client, remote["id"]),
            ensure_localization(client, remote["id"], product),
            ensure_price(client, remote["id"], product),
            ensure_review_screenshot(
                client,
                remote["id"],
                product.product_id,
                screenshot_responses.get(product.product_id),
                local_screenshot,
            ),
        ]
        product_ready = all(checks)
        if product_ready:
            refreshed = client.call("GET", f"/v2/inAppPurchases/{remote['id']}")["data"]
            state = refreshed.get("attributes", {}).get("state")
            print(f"  App Store state: {state}")
            if state == "MISSING_METADATA":
                print("  readiness: Apple still reports missing metadata")
                product_ready = False
        ready = ready and product_ready

    if not ready:
        mode = "Apply" if APPLY else "Audit"
        raise RuntimeError(
            f"{mode} completed, but the Gold catalog is not metadata-ready; "
            "review the field-level findings above"
        )
    print("\nGold catalog metadata is complete for all five products.")
    if APPLY:
        print("Apple says product-metadata changes can take up to one hour to appear in Sandbox.")
    else:
        print("Audit completed without changing App Store Connect.")
    print(
        "Submission is intentionally separate: Apple's first in-app purchase must be submitted "
        "with an app version in App Store Connect."
    )


if __name__ == "__main__":
    try:
        main()
    except (ApiError, RuntimeError, ValueError, KeyError) as error:
        print(f"::error::{error}", file=sys.stderr)
        raise SystemExit(1)
