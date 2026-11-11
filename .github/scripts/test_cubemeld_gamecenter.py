#!/usr/bin/env python3
"""Deterministic tests for the Cubemeld Game Center manifest loader."""

import hashlib
import importlib.util
import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib
from pathlib import Path

os.environ.setdefault("KEY", "test-key")
os.environ.setdefault("KEY_ID", "test-key-id")
os.environ.setdefault("ISSUER_ID", "test-issuer-id")
os.environ["APPLY"] = "false"

SCRIPT_PATH_IAP = Path(__file__).with_name("cubemeld_iap.py")
SPEC_IAP = importlib.util.spec_from_file_location("cubemeld_iap", SCRIPT_PATH_IAP)
IAP = importlib.util.module_from_spec(SPEC_IAP)
assert SPEC_IAP.loader is not None
sys.modules[SPEC_IAP.name] = IAP
SPEC_IAP.loader.exec_module(IAP)

SCRIPT_PATH_GC = Path(__file__).with_name("cubemeld_gamecenter.py")
SPEC_GC = importlib.util.spec_from_file_location("cubemeld_gamecenter", SCRIPT_PATH_GC)
GC = importlib.util.module_from_spec(SPEC_GC)
assert SPEC_GC.loader is not None
sys.modules[SPEC_GC.name] = GC
SPEC_GC.loader.exec_module(GC)


def make_png(w: int, h: int) -> bytes:
    def chunk(kind: bytes, payload: bytes) -> bytes:
        checksum = zlib.crc32(kind + payload) & 0xFFFFFFFF
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", checksum)

    ihdr_data = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    row = b"\x00" + (b"\x19\x35\x52" * w)
    raw_data = row * h
    compressed_data = zlib.compress(raw_data)

    png_bytes = b"\x89PNG\r\n\x1a\n"
    png_bytes += chunk(b"IHDR", ihdr_data)
    png_bytes += chunk(b"IDAT", compressed_data)
    png_bytes += chunk(b"IEND", b"")
    return png_bytes


def write_manifest(tmpdir: str, image_size=(512, 512), **overrides) -> str:
    tmp_path = Path(tmpdir)
    images_dir = tmp_path / "images"
    images_dir.mkdir(parents=True, exist_ok=True)

    png_content = make_png(*image_size)
    image_file = images_dir / "a.png"
    image_file.write_bytes(png_content)
    sha256_hex = hashlib.sha256(png_content).hexdigest()

    manifest_data = {
        "bundleId": IAP.EXPECTED_BUNDLE_ID,
        "achievements": [
            {
                "vendorIdentifier": "cubemeld.achievement.firstWin",
                "referenceName": "First Win",
                "points": 10,
                "showBeforeEarned": True,
                "image": {"path": "images/a.png", "sha256": sha256_hex},
                "localizations": {
                    "en-US": {
                        "name": "First Win",
                        "beforeEarnedDescription": "Win any Match.",
                        "afterEarnedDescription": "You won."
                    }
                }
            }
        ],
        "leaderboards": [
            {
                "vendorIdentifier": "cubemeld.match.wins",
                "referenceName": "Match wins",
                "defaultFormatter": "INTEGER",
                "submissionType": "BEST_SCORE",
                "scoreSortType": "DESC",
                "localizations": {
                    "en-US": {"name": "Match wins"}
                }
            }
        ]
    }

    for key, value in overrides.items():
        if key == "achievements" or key == "leaderboards":
            manifest_data[key] = value
        else:
            manifest_data[key] = value

    manifest_file = tmp_path / "achievements.json"
    manifest_file.write_text(json.dumps(manifest_data), encoding="utf-8")
    return str(manifest_file)


class TestLoadManifest(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmpdir.cleanup)

    def test_valid_manifest_loads(self):
        path = write_manifest(self.tmpdir.name)
        achievements, leaderboards = GC.load_manifest(path)
        self.assertEqual(len(achievements), 1)
        self.assertEqual(len(leaderboards), 1)
        self.assertEqual(achievements[0]["_image"][0], "a.png")

    def test_apply_reads_committed_image_and_keeps_its_file_name(self):
        path = write_manifest(self.tmpdir.name)
        root = self.tmpdir.name
        identity = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_COMMITTER_NAME": "t",
                    "GIT_AUTHOR_EMAIL": "t@example.invalid", "GIT_COMMITTER_EMAIL": "t@example.invalid"}
        for args in (["init", "-q"], ["add", "."], ["commit", "-qm", "x"]):
            subprocess.run(["git", "-C", root, *args], check=True, env=identity)
        GC.APPLY = True
        try:
            achievements, _ = GC.load_manifest(path)
        finally:
            GC.APPLY = False
        self.assertEqual(achievements[0]["_image"][0], "a.png")

    def test_wrong_bundle_rejected(self):
        path = write_manifest(self.tmpdir.name, bundleId="com.wrong.bundle")
        with self.assertRaises(RuntimeError):
            GC.load_manifest(path)

    def test_points_0_rejected(self):
        ach = [{
            "vendorIdentifier": "cubemeld.achievement.firstWin",
            "referenceName": "First Win",
            "points": 0,
            "showBeforeEarned": True,
            "image": {"path": "images/a.png", "sha256": hashlib.sha256(make_png(512, 512)).hexdigest()},
            "localizations": {
                "en-US": {
                    "name": "First Win",
                    "beforeEarnedDescription": "Win any Match.",
                    "afterEarnedDescription": "You won."
                }
            }
        }]
        path = write_manifest(self.tmpdir.name, achievements=ach)
        with self.assertRaises(RuntimeError):
            GC.load_manifest(path)

    def test_points_101_rejected(self):
        ach = [{
            "vendorIdentifier": "cubemeld.achievement.firstWin",
            "referenceName": "First Win",
            "points": 101,
            "showBeforeEarned": True,
            "image": {"path": "images/a.png", "sha256": hashlib.sha256(make_png(512, 512)).hexdigest()},
            "localizations": {
                "en-US": {
                    "name": "First Win",
                    "beforeEarnedDescription": "Win any Match.",
                    "afterEarnedDescription": "You won."
                }
            }
        }]
        path = write_manifest(self.tmpdir.name, achievements=ach)
        with self.assertRaises(RuntimeError):
            GC.load_manifest(path)

    def test_total_over_1000_rejected(self):
        achs = []
        for i in range(11):
            achs.append({
                "vendorIdentifier": f"cubemeld.achievement.test{i}",
                "referenceName": f"Test {i}",
                "points": 100,
                "showBeforeEarned": True,
                "image": {"path": "images/a.png", "sha256": hashlib.sha256(make_png(512, 512)).hexdigest()},
                "localizations": {
                    "en-US": {
                        "name": f"Test {i}",
                        "beforeEarnedDescription": "Desc.",
                        "afterEarnedDescription": "Done."
                    }
                }
            })
        path = write_manifest(self.tmpdir.name, achievements=achs)
        with self.assertRaises(RuntimeError):
            GC.load_manifest(path)

    def test_name_over_30_chars_rejected(self):
        long_name = "A" * 31
        ach = [{
            "vendorIdentifier": "cubemeld.achievement.firstWin",
            "referenceName": "First Win",
            "points": 10,
            "showBeforeEarned": True,
            "image": {"path": "images/a.png", "sha256": hashlib.sha256(make_png(512, 512)).hexdigest()},
            "localizations": {
                "en-US": {
                    "name": long_name,
                    "beforeEarnedDescription": "Win any Match.",
                    "afterEarnedDescription": "You won."
                }
            }
        }]
        path = write_manifest(self.tmpdir.name, achievements=ach)
        with self.assertRaises(RuntimeError):
            GC.load_manifest(path)

    def test_missing_en_us_rejected(self):
        ach = [{
            "vendorIdentifier": "cubemeld.achievement.firstWin",
            "referenceName": "First Win",
            "points": 10,
            "showBeforeEarned": True,
            "image": {"path": "images/a.png", "sha256": hashlib.sha256(make_png(512, 512)).hexdigest()},
            "localizations": {
                "fr-FR": {
                    "name": "First Win",
                    "beforeEarnedDescription": "Win any Match.",
                    "afterEarnedDescription": "You won."
                }
            }
        }]
        path = write_manifest(self.tmpdir.name, achievements=ach)
        with self.assertRaises(RuntimeError):
            GC.load_manifest(path)

    def test_checksum_mismatch_rejected(self):
        ach = [{
            "vendorIdentifier": "cubemeld.achievement.firstWin",
            "referenceName": "First Win",
            "points": 10,
            "showBeforeEarned": True,
            "image": {"path": "images/a.png", "sha256": "deadbeef"},
            "localizations": {
                "en-US": {
                    "name": "First Win",
                    "beforeEarnedDescription": "Win any Match.",
                    "afterEarnedDescription": "You won."
                }
            }
        }]
        path = write_manifest(self.tmpdir.name, achievements=ach)
        with self.assertRaises(RuntimeError):
            GC.load_manifest(path)

    def test_300x300_image_rejected(self):
        path = write_manifest(self.tmpdir.name, image_size=(300, 300))
        with self.assertRaisesRegex(RuntimeError, "512x512 or 1024x1024"):
            GC.load_manifest(path)

    def test_bad_leaderboard_formatter_rejected(self):
        lb = [{
            "vendorIdentifier": "cubemeld.match.wins",
            "referenceName": "Match wins",
            "defaultFormatter": "FLOAT",
            "submissionType": "BEST_SCORE",
            "scoreSortType": "DESC",
            "localizations": {
                "en-US": {"name": "Match wins"}
            }
        }]
        path = write_manifest(self.tmpdir.name, leaderboards=lb)
        with self.assertRaises(RuntimeError):
            GC.load_manifest(path)

    def test_duplicate_achievement_id_rejected(self):
        ach = [
            {
                "vendorIdentifier": "cubemeld.achievement.firstWin",
                "referenceName": "First Win",
                "points": 10,
                "showBeforeEarned": True,
                "image": {"path": "images/a.png", "sha256": hashlib.sha256(make_png(512, 512)).hexdigest()},
                "localizations": {
                    "en-US": {
                        "name": "First Win",
                        "beforeEarnedDescription": "Win any Match.",
                        "afterEarnedDescription": "You won."
                    }
                }
            },
            {
                "vendorIdentifier": "cubemeld.achievement.firstWin",
                "referenceName": "First Win 2",
                "points": 10,
                "showBeforeEarned": True,
                "image": {"path": "images/a.png", "sha256": hashlib.sha256(make_png(512, 512)).hexdigest()},
                "localizations": {
                    "en-US": {
                        "name": "First Win 2",
                        "beforeEarnedDescription": "Win any Match.",
                        "afterEarnedDescription": "You won."
                    }
                }
            }
        ]
        path = write_manifest(self.tmpdir.name, achievements=ach)
        with self.assertRaises(RuntimeError):
            GC.load_manifest(path)

class TestUploadHost(unittest.TestCase):
    def op(self, url):
        return {"method": "PUT", "url": url, "offset": 0, "length": 4, "requestHeaders": []}

    def test_accepts_https_apple_hosts(self):
        method, url, _, _, _ = GC._validated(self.op("https://store-033.blobstore.apple.com/x?sig=1"), 4)
        self.assertEqual(method, "PUT")
        GC._validated(self.op("https://assets.itunes.apple.com/up?sig=1"), 4)

    def test_rejects_other_hosts_and_plain_http(self):
        for url in ("https://apple.com.evil.example/x?s=1", "http://a.apple.com/x?s=1",
                    "https://user@a.apple.com/x?s=1", "https://a.apple.com/x"):
            with self.assertRaises(RuntimeError):
                GC._validated(self.op(url), 4)

    def test_keeps_the_iap_range_checks(self):
        bad = dict(self.op("https://a.apple.com/x?s=1"), length=9)
        with self.assertRaises(RuntimeError):
            GC._validated(bad, 4)


if __name__ == "__main__":
    unittest.main()
