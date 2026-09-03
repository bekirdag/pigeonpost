#!/usr/bin/env python3
"""Deterministic tests for the Cubemeld App Store Connect IAP broker."""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest
import urllib.error
import urllib.request
import zlib
from pathlib import Path
from unittest import mock

os.environ.setdefault("KEY", "test-key")
os.environ.setdefault("KEY_ID", "test-key-id")
os.environ.setdefault("ISSUER_ID", "test-issuer-id")
os.environ["APPLY"] = "false"

SCRIPT_PATH = Path(__file__).with_name("cubemeld_iap.py")
SPEC = importlib.util.spec_from_file_location("cubemeld_iap", SCRIPT_PATH)
IAP = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = IAP
SPEC.loader.exec_module(IAP)


def http_error(
    status: int,
    *,
    url: str = "https://api.appstoreconnect.apple.com/v1/apps?cursor=signed-secret",
    headers=None,
    detail: str = "sensitive response detail",
) -> urllib.error.HTTPError:
    payload = {
        "errors": [
            {
                "status": str(status),
                "code": "TEST_ERROR",
                "title": "Test API error",
                "detail": detail,
            }
        ]
    }
    return urllib.error.HTTPError(
        url,
        status,
        "error",
        headers or {},
        io.BytesIO(json.dumps(payload).encode()),
    )


def json_response(payload):
    response = mock.MagicMock()
    response.__enter__.return_value.read.return_value = json.dumps(payload).encode()
    return response


class ClientRetryTests(unittest.TestCase):
    @staticmethod
    def client(token: str = "original-token"):
        client = object.__new__(IAP.Client)
        client.token = token
        return client

    def test_get_recovers_from_bounded_5xx_and_429(self):
        client = self.client()
        with (
            mock.patch.object(
                urllib.request,
                "urlopen",
                side_effect=[
                    http_error(500),
                    http_error(429, headers={"Retry-After": "300"}),
                    json_response({"data": []}),
                ],
            ) as urlopen,
            mock.patch.object(IAP.time, "sleep") as sleep,
        ):
            self.assertEqual(client.call("GET", "/v1/apps", params={"limit": 1}), {"data": []})
        self.assertEqual(urlopen.call_count, 3)
        self.assertEqual(sleep.call_args_list, [mock.call(1), mock.call(30)])

    def test_get_remints_once_after_401(self):
        client = self.client()
        with (
            mock.patch.object(IAP, "mint_token", return_value="fresh-token") as mint,
            mock.patch.object(
                urllib.request,
                "urlopen",
                side_effect=[http_error(401), json_response({"data": [{"id": "app-1"}]})],
            ) as urlopen,
        ):
            result = client.call("GET", "/v1/apps")
        self.assertEqual(result, {"data": [{"id": "app-1"}]})
        mint.assert_called_once_with()
        requests = [item.args[0] for item in urlopen.call_args_list]
        self.assertEqual(requests[0].get_header("Authorization"), "Bearer original-token")
        self.assertEqual(requests[1].get_header("Authorization"), "Bearer fresh-token")

    def test_second_401_fails_without_another_remint(self):
        client = self.client()
        with (
            mock.patch.object(IAP, "mint_token", return_value="fresh-token") as mint,
            mock.patch.object(
                urllib.request,
                "urlopen",
                side_effect=[http_error(401), http_error(401)],
            ) as urlopen,
        ):
            with self.assertRaises(IAP.ApiError) as raised:
                client.call("GET", "/v1/apps")
        self.assertEqual(raised.exception.status, 401)
        self.assertEqual(urlopen.call_count, 2)
        mint.assert_called_once_with()

    def test_transient_retries_are_bounded_and_redacted(self):
        client = self.client(token="bearer-secret")
        failures = [
            http_error(
                503,
                detail="https://store-030.blobstore.apple.com/upload?Signature=body-secret",
            )
            for _ in range(4)
        ]
        with (
            mock.patch.object(urllib.request, "urlopen", side_effect=failures) as urlopen,
            mock.patch.object(IAP.time, "sleep") as sleep,
        ):
            with self.assertRaises(IAP.ApiError) as raised:
                client.call(
                    "GET",
                    "https://api.appstoreconnect.apple.com/v1/apps?cursor=query-secret",
                )
        message = str(raised.exception)
        self.assertEqual(urlopen.call_count, 4)
        self.assertEqual(sleep.call_args_list, [mock.call(1), mock.call(2), mock.call(4)])
        self.assertNotIn("query-secret", message)
        self.assertNotIn("body-secret", message)
        self.assertNotIn("bearer-secret", message)
        self.assertIn("response body omitted", message)

    def test_semantic_4xx_and_mutations_fail_without_retry(self):
        for method, status in (("GET", 422), ("POST", 500), ("PATCH", 401)):
            with self.subTest(method=method, status=status):
                client = self.client()
                with (
                    mock.patch.object(
                        urllib.request,
                        "urlopen",
                        side_effect=http_error(status),
                    ) as urlopen,
                    mock.patch.object(IAP, "mint_token") as mint,
                    mock.patch.object(IAP.time, "sleep") as sleep,
                ):
                    with self.assertRaises(IAP.ApiError):
                        client.call(method, "/v1/inAppPurchases", body={"data": {}})
                urlopen.assert_called_once()
                mint.assert_not_called()
                sleep.assert_not_called()

    def test_allowed_404_returns_none_without_retry(self):
        client = self.client()
        with mock.patch.object(
            urllib.request, "urlopen", side_effect=http_error(404)
        ) as urlopen:
            self.assertIsNone(client.call("GET", "/v1/missing", allow_404=True))
        urlopen.assert_called_once()


def png_chunk(kind: bytes, payload: bytes) -> bytes:
    checksum = zlib.crc32(kind + payload) & 0xFFFFFFFF
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", checksum)


def rgb_png(width: int = 1320, height: int = 2868, color_type: int = 2) -> bytes:
    channels = 3 if color_type == 2 else 4
    row = b"\x00" + (b"\x19\x35\x52" + (b"\xff" if channels == 4 else b"")) * width
    header = struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", header)
        + png_chunk(b"IDAT", zlib.compress(row * height, 9))
        + png_chunk(b"IEND", b"")
    )


def complete_screenshot(
    screenshot_id: str = "screenshot-1",
    file_name: str = "cubemeld-gold.png",
    file_size: int = 6,
    checksum: str | None = None,
):
    checksum = checksum or hashlib.md5(b"abcdef", usedforsecurity=False).hexdigest()
    return {
        "data": {
            "type": IAP.REVIEW_SCREENSHOT_TYPE,
            "id": screenshot_id,
            "attributes": {
                "fileName": file_name,
                "fileSize": file_size,
                "sourceFileChecksum": checksum,
                "assetType": "SCREENSHOT",
                "assetDeliveryState": {"state": "COMPLETE", "errors": [], "warnings": []},
            },
        }
    }


class TemporaryScreenshotRepository:
    def __init__(self, screenshot: bytes | None = None):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.fastlane = self.root / "fastlane"
        self.fastlane.mkdir()
        self.screenshot_path = self.fastlane / "review" / "cubemeld-gold.png"
        self.screenshot_path.parent.mkdir()
        self.screenshot = screenshot or rgb_png()
        self.screenshot_path.write_bytes(self.screenshot)
        self.manifest_path = self.fastlane / "iap_review_screenshot.json"
        self.write_manifest(hashlib.sha256(self.screenshot).hexdigest())
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(["git", "-C", str(self.root), "add", "."], check=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(self.root),
                "-c",
                "user.name=Test",
                "-c",
                "user." + "e" + "mail=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
            check=True,
        )

    def write_manifest(self, checksum: str) -> None:
        self.manifest_path.write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "file": "review/cubemeld-gold.png",
                    "sha256": checksum,
                    "productIDs": sorted(IAP.EXPECTED_PRODUCTS),
                }
            ),
            encoding="utf-8",
        )

    def close(self):
        self.directory.cleanup()


class ReviewScreenshotManifestTests(unittest.TestCase):
    def test_loads_exact_committed_rgb_png(self):
        repository = TemporaryScreenshotRepository()
        self.addCleanup(repository.close)
        with mock.patch.object(
            IAP, "REVIEW_SCREENSHOT_MANIFEST_PATH", str(repository.manifest_path)
        ):
            screenshot = IAP.load_review_screenshot()
        self.assertEqual(screenshot.content, repository.screenshot)
        self.assertEqual(screenshot.sha256, hashlib.sha256(repository.screenshot).hexdigest())

    def test_rejects_worktree_bytes_that_differ_from_head(self):
        repository = TemporaryScreenshotRepository()
        self.addCleanup(repository.close)
        repository.screenshot_path.write_bytes(repository.screenshot + b"drift")
        with mock.patch.object(
            IAP, "REVIEW_SCREENSHOT_MANIFEST_PATH", str(repository.manifest_path)
        ):
            with self.assertRaisesRegex(RuntimeError, "differs from HEAD"):
                IAP.load_review_screenshot()

    def test_rejects_alpha_channel_even_with_matching_manifest(self):
        repository = TemporaryScreenshotRepository(rgb_png(color_type=6))
        self.addCleanup(repository.close)
        with mock.patch.object(
            IAP, "REVIEW_SCREENSHOT_MANIFEST_PATH", str(repository.manifest_path)
        ):
            with self.assertRaisesRegex(RuntimeError, "without alpha"):
                IAP.load_review_screenshot()

    def test_rejects_manifest_with_the_wrong_product_set(self):
        repository = TemporaryScreenshotRepository()
        self.addCleanup(repository.close)
        manifest = json.loads(repository.manifest_path.read_text(encoding="utf-8"))
        manifest["productIDs"].pop()
        repository.manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        subprocess.run(
            ["git", "-C", str(repository.root), "add", "."],
            check=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(repository.root),
                "-c",
                "user.name=Test",
                "-c",
                "user." + "e" + "mail=test@example.invalid",
                "commit",
                "-qm",
                "wrong products",
            ],
            check=True,
        )
        with mock.patch.object(
            IAP, "REVIEW_SCREENSHOT_MANIFEST_PATH", str(repository.manifest_path)
        ):
            with self.assertRaisesRegex(RuntimeError, "exactly the five"):
                IAP.load_review_screenshot()


class UploadOperationTests(unittest.TestCase):
    def operation(self, offset=0, length=6, **changes):
        operation = {
            "method": "PUT",
            "url": "https://store-030.blobstore.apple.com/upload?Signature=secret",
            "offset": offset,
            "length": length,
            "requestHeaders": [{"name": "Content-Type", "value": "image/png"}],
        }
        operation.update(changes)
        return operation

    def test_orders_only_an_exact_nonoverlapping_file_cover(self):
        operations = [self.operation(3, 3), self.operation(0, 3)]
        ordered = IAP.ordered_upload_operations(operations, 6)
        self.assertEqual([item["offset"] for item in ordered], [0, 3])

    def test_rejects_a_gap_before_any_upload(self):
        with self.assertRaisesRegex(RuntimeError, "overlap or leave a byte gap"):
            IAP.ordered_upload_operations([self.operation(0, 2), self.operation(3, 3)], 6)

    def test_rejects_non_apple_upload_host_and_sensitive_header(self):
        with self.assertRaisesRegex(RuntimeError, "outside its HTTPS blob store"):
            IAP.validate_upload_operation(
                self.operation(url="https://example.invalid/upload"), 6
            )
        with self.assertRaisesRegex(RuntimeError, "unsafe screenshot upload header"):
            IAP.validate_upload_operation(
                self.operation(requestHeaders=[{"name": "Authorization", "value": "secret"}]),
                6,
            )

    def test_binary_upload_never_adds_app_store_bearer_token(self):
        client = object.__new__(IAP.Client)
        client.token = "must-not-leak"
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = b""
        with mock.patch.object(urllib.request, "urlopen", return_value=response) as urlopen:
            client.upload(self.operation(offset=1, length=3), b"abcdef")
        request = urlopen.call_args.args[0]
        self.assertEqual(request.data, b"bcd")
        self.assertNotIn("Authorization", dict(request.header_items()))

    def test_binary_upload_error_redacts_signed_url_and_untrusted_detail(self):
        client = object.__new__(IAP.Client)
        client.token = "must-not-leak"
        with mock.patch.object(
            urllib.request,
            "urlopen",
            side_effect=http_error(
                500,
                url="https://store-030.blobstore.apple.com/upload?Signature=query-secret",
                detail="signed body-secret",
            ),
        ):
            with self.assertRaises(IAP.ApiError) as raised:
                client.upload(self.operation(), b"abcdef")
        message = str(raised.exception)
        self.assertNotIn("query-secret", message)
        self.assertNotIn("body-secret", message)
        self.assertNotIn("must-not-leak", message)
        self.assertIn("response body omitted", message)


class FakeUploadClient:
    def __init__(self, content: bytes):
        self.content = content
        self.calls = []
        self.uploads = []
        self.get_count = 0
        self.checksum = hashlib.md5(content, usedforsecurity=False).hexdigest()

    def call(self, method, path, body=None, params=None, allow_404=False):
        self.calls.append((method, path, body, params, allow_404))
        if method == "POST":
            return {
                "data": {
                    "type": IAP.REVIEW_SCREENSHOT_TYPE,
                    "id": "reserved-1",
                    "attributes": {
                        "fileName": "cubemeld-gold.png",
                        "fileSize": len(self.content),
                        "assetType": "SCREENSHOT",
                        "assetDeliveryState": {
                            "state": "AWAITING_UPLOAD",
                            "errors": [],
                            "warnings": [],
                        },
                        "uploadOperations": [
                            UploadOperationTests().operation(3, 3),
                            UploadOperationTests().operation(0, 3),
                        ],
                    },
                }
            }
        if method == "PATCH":
            return {}
        self.get_count += 1
        if self.get_count == 1:
            response = complete_screenshot(screenshot_id="reserved-1", checksum=self.checksum)
            response["data"]["attributes"]["assetDeliveryState"]["state"] = "UPLOAD_COMPLETE"
            return response
        return complete_screenshot(screenshot_id="reserved-1", checksum=self.checksum)

    def upload(self, operation, content):
        self.uploads.append((operation["offset"], content))


class ReviewScreenshotApiTests(unittest.TestCase):
    def test_missing_screenshot_is_a_read_only_audit_failure(self):
        client = mock.MagicMock()
        with mock.patch.object(IAP, "APPLY", False):
            with contextlib.redirect_stdout(io.StringIO()):
                ready = IAP.ensure_review_screenshot(client, "iap-1", "product-1", None, None)
        self.assertFalse(ready)
        client.assert_not_called()

    def test_complete_screenshot_is_ready(self):
        with contextlib.redirect_stdout(io.StringIO()):
            ready = IAP.ensure_review_screenshot(
                mock.MagicMock(), "iap-1", "product-1", complete_screenshot(), None
            )
        self.assertTrue(ready)

    def test_incomplete_remote_upload_is_never_replaced(self):
        response = complete_screenshot()
        response["data"]["attributes"]["assetDeliveryState"]["state"] = "AWAITING_UPLOAD"
        client = mock.MagicMock()
        with mock.patch.object(IAP, "APPLY", True):
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, "refusing to replace or resume"):
                    IAP.ensure_review_screenshot(client, "iap-1", "product-1", response, None)
        client.assert_not_called()

    def test_apply_reserves_uploads_commits_and_waits_for_complete(self):
        content = b"abcdef"
        screenshot = IAP.ReviewScreenshot(
            path=Path("cubemeld-gold.png"),
            file_name="cubemeld-gold.png",
            content=content,
            sha256=hashlib.sha256(content).hexdigest(),
        )
        client = FakeUploadClient(content)
        with (
            mock.patch.object(IAP, "APPLY", True),
            mock.patch.object(IAP.time, "sleep"),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            ready = IAP.ensure_review_screenshot(
                client, "iap-1", "product-1", None, screenshot
            )
        self.assertTrue(ready)
        self.assertEqual([offset for offset, _ in client.uploads], [0, 3])
        post = next(call for call in client.calls if call[0] == "POST")
        self.assertEqual(
            post[2]["data"]["relationships"]["inAppPurchaseV2"]["data"],
            {"type": "inAppPurchases", "id": "iap-1"},
        )
        patch = next(call for call in client.calls if call[0] == "PATCH")
        self.assertEqual(
            patch[2]["data"]["attributes"],
            {"uploaded": True, "sourceFileChecksum": client.checksum},
        )

    def test_failed_asset_state_surfaces_only_the_error_code(self):
        response = complete_screenshot()
        response["data"]["attributes"]["assetDeliveryState"] = {
            "state": "FAILED",
            "errors": [{"code": "IMAGE_BAD", "description": "sensitive detail"}],
            "warnings": [],
        }
        with self.assertRaisesRegex(RuntimeError, r"processing failed \(IMAGE_BAD\)") as raised:
            IAP.review_screenshot_state(response, "product-1")
        self.assertNotIn("sensitive detail", str(raised.exception))


class MainAuditTests(unittest.TestCase):
    def test_audit_exits_nonzero_when_review_screenshot_is_missing(self):
        product = IAP.Product(
            product_id="product-1",
            reference_name="Gold",
            gold_amount=100,
            price_usd=IAP.Decimal("0.99"),
            display_name="100 Gold",
            description="100 Gold",
        )
        remote = {
            "type": "inAppPurchases",
            "id": "iap-1",
            "attributes": {
                "productId": "product-1",
                "inAppPurchaseType": "CONSUMABLE",
                "state": "MISSING_METADATA",
            },
        }

        class AuditClient:
            def list_all(self, path, params=None):
                if path == "/v1/apps":
                    return [{"id": "app-1", "attributes": {"name": "Cubemeld"}}]
                if path.endswith("/inAppPurchasesV2"):
                    return [remote]
                raise AssertionError(path)

            def call(self, method, path, body=None, params=None, allow_404=False):
                self.assert_get(method)
                if path.endswith("/appStoreReviewScreenshot"):
                    if not allow_404 or params != {
                        "fields[inAppPurchaseAppStoreReviewScreenshots]": (
                            "fileSize,fileName,sourceFileChecksum,assetType,assetDeliveryState"
                        )
                    }:
                        raise AssertionError("screenshot audit omitted required fields")
                    return None
                if path == "/v2/inAppPurchases/iap-1":
                    return {"data": remote}
                raise AssertionError(path)

            @staticmethod
            def assert_get(method):
                if method != "GET":
                    raise AssertionError("audit attempted a mutation")

        with (
            mock.patch.object(IAP, "APPLY", False),
            mock.patch.object(IAP, "load_catalog", return_value=[product]),
            mock.patch.object(IAP, "Client", return_value=AuditClient()),
            mock.patch.object(IAP, "ensure_parent_metadata", return_value=True),
            mock.patch.object(IAP, "ensure_availability", return_value=True),
            mock.patch.object(IAP, "ensure_localization", return_value=True),
            mock.patch.object(IAP, "ensure_price", return_value=True),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            with self.assertRaisesRegex(RuntimeError, "not metadata-ready"):
                IAP.main()

    def test_apply_requires_committed_screenshot_before_first_mutation(self):
        product = IAP.Product(
            product_id="product-1",
            reference_name="Gold",
            gold_amount=100,
            price_usd=IAP.Decimal("0.99"),
            display_name="100 Gold",
            description="100 Gold",
        )

        class PreflightClient:
            def __init__(self):
                self.mutations = []

            def list_all(self, path, params=None):
                if path == "/v1/apps":
                    return [{"id": "app-1", "attributes": {"name": "Cubemeld"}}]
                if path.endswith("/inAppPurchasesV2"):
                    return []
                raise AssertionError(path)

            def call(self, method, path, body=None, params=None, allow_404=False):
                if method != "GET":
                    self.mutations.append((method, path))
                raise AssertionError("preflight should not call the API")

        client = PreflightClient()
        with (
            mock.patch.object(IAP, "APPLY", True),
            mock.patch.object(IAP, "load_catalog", return_value=[product]),
            mock.patch.object(IAP, "Client", return_value=client),
            mock.patch.object(
                IAP,
                "load_review_screenshot",
                side_effect=RuntimeError("missing committed review screenshot"),
            ),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            with self.assertRaisesRegex(RuntimeError, "missing committed review screenshot"):
                IAP.main()
        self.assertEqual(client.mutations, [])


if __name__ == "__main__":
    unittest.main()
