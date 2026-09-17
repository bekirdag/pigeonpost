"""Offline checks: provisioning must never accept open redirects or overwrite private backups."""
import importlib.util
import json
import os
import pathlib
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("chatgpt_provision", pathlib.Path(__file__).with_name("provision-chatgpt.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ProvisioningBoundaries(unittest.TestCase):
    def test_only_exact_production_callbacks(self):
        for value in ["https://chatgpt.com/connector/oauth/test", "https://chatgpt.com/connector_platform_oauth_redirect"]:
            self.assertEqual(module.callbacks(json.dumps([value])), [value])
        for value in ["https://chatgpt.com/*", "http://chatgpt.com/callback", "https://chatgpt.com.attacker.example/", "https://attacker.example/", "https://user:pass@chatgpt.com/callback", "https://chatgpt.com/callback#fragment", "http://127.0.0.1:9999/callback"]:
            with self.subTest(callback=value), self.assertRaises(ValueError):
                module.callbacks(json.dumps([value]))
        self.assertEqual(module.callbacks("[]"), [])

    def test_loopback_exception_requires_explicit_test_flag(self):
        value = "http://127.0.0.1:9999/callback"
        with patch.dict(os.environ, {"KC_ALLOW_LOOPBACK_TEST": "1"}):
            self.assertEqual(module.callbacks(json.dumps([value])), [value])
            with self.assertRaises(ValueError):
                module.callbacks('["http://attacker.example/callback"]')

    def test_private_files_are_exclusive_and_private(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "backup.json"
            module.private_write(path, "fixture")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                module.private_write(path, "overwritten")
            self.assertEqual(path.read_text(), "fixture")


if __name__ == "__main__":
    unittest.main()
