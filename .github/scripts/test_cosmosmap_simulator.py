"""Offline fixtures for the exact-runtime release gate (no Apple accounts needed)."""

import json
from pathlib import Path
import subprocess
import sys
import unittest

from cosmosmap_simulator import select_device


def device(udid, available=True, name="iPad Pro 13-inch (M5)"):
    return {"udid": udid, "isAvailable": available, "name": name}


def runtime(version):
    return f"com.apple.CoreSimulator.SimRuntime.iOS-{version}"


class SimulatorSelectionTests(unittest.TestCase):
    def setUp(self):
        self.data = {"devices": {
            runtime("26-2"): [device("ios262")],
            runtime("26-5"): [device("ios265")],
            runtime("26-10"): [device("unavailable", False)],
            "com.apple.CoreSimulator.SimRuntime.tvOS-27-0": [device("not-ios")],
        }}

    def test_native_pin_uses_262_despite_newer_installed_runtime(self):
        self.assertEqual(select_device(self.data, "26.2"), "ios262")

    def test_host_smoke_selects_latest_available_ios(self):
        self.assertEqual(select_device(self.data), "ios265")

    def test_missing_pinned_runtime_is_an_error_not_a_fallback(self):
        del self.data["devices"][runtime("26-2")]
        with self.assertRaisesRegex(ValueError, "iOS 26.2"):
            select_device(self.data, "26.2")

    def test_unavailable_pinned_ipad_is_an_error(self):
        self.data["devices"][runtime("26-2")][0]["isAvailable"] = False
        with self.assertRaises(ValueError):
            select_device(self.data, "26.2")

    def test_wrong_device_family_is_an_error(self):
        self.data["devices"][runtime("26-2")] = [device("phone", name="iPhone 17 Pro")]
        with self.assertRaises(ValueError):
            select_device(self.data, "26.2")

    def test_patch_version_zero_matches_exact_pin(self):
        self.data["devices"][runtime("26-2-0")] = self.data["devices"].pop(runtime("26-2"))
        self.assertEqual(select_device(self.data, "26.2"), "ios262")

    def test_nonzero_patch_does_not_silently_replace_exact_pin(self):
        self.data["devices"][runtime("26-2-1")] = self.data["devices"].pop(runtime("26-2"))
        with self.assertRaises(ValueError):
            select_device(self.data, "26.2")

    def test_numeric_order_not_lexicographic(self):
        self.data["devices"][runtime("26-10")] = [device("ios2610")]
        self.assertEqual(select_device(self.data), "ios2610")

    def test_cli_reports_missing_pin_with_nonzero_exit(self):
        result = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("cosmosmap_simulator.py")), "--ios-version", "25.0"],
            input=json.dumps(self.data), text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("::error::", result.stderr)


if __name__ == "__main__":
    unittest.main()
