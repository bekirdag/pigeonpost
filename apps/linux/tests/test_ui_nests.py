"""Nests in the real GTK window, with a fake postbox: Products, a Nest's page, approvals. Run under Xvfb."""
import copy
import sys
import tempfile
import unittest
from pathlib import Path

from pigeonpost.protected import MemoryProtectedStore
from pigeonpost.theme import Preferences
from pigeonpost.ui import Adw, Window
from test_ui import FakeAPI, pump
from test_ui_features import find_button, texts

NEST = "nst_0123abcd"
NEST_MB = "/k/" + "n" * 26
HASH = "b" * 64
CARD = "nap_" + "1" * 24


class NestAPI(FakeAPI):
    def __init__(self, features=frozenset({"nests"})):
        super().__init__()
        self.features = features
        self.cards = [{"approval_id": CARD, "tool": "send_message", "args_hash": HASH, "status": "pending",
                       "call": {"tool": "send_message", "args": {"to": "/alex/main", "body": "Draft ready"}}}]

    def capabilities(self):
        return self.features

    def call(self, method, path, identity=None, data=None, **kwargs):
        if path == "/v1/nest-packages":
            self.calls.append((method, path, identity, data))
            return {"catalog_version": "nests-2026-11", "packages": [
                {"code": "nest.ai.starter.monthly", "name": "Pigeonpost AI", "slots": 1, "storage_bytes": 5 << 30,
                 "active_hours": 10, "input_tokens": 1000000, "output_tokens": 100000, "model": "managed", "sellable": True},
                {"code": "nest.ai.pro.monthly", "name": "Pigeonpost AI Pro", "slots": 10, "storage_bytes": 100 << 30, "sellable": False}]}
        if path == "/v1/nests":
            self.calls.append((method, path, identity, data))
            return {"allowance": {"slots": 1, "used": 1, "funding": {"package": "nest.ai.starter.monthly", "source": "apple"}, "duplicates": []},
                    "nests": [{"nest_id": NEST, "address": NEST_MB, "label": "Mira", "state": "running", "status": "working", "funding": "package"}],
                    "setups": []}
        if path == f"/v1/nests/{NEST}" and method == "GET":
            return {"nest_id": NEST, "address": NEST_MB, "label": "Mira", "state": "running", "status": "working", "role": "owner"}
        if path == f"/v1/nests/{NEST}/files":
            return {"files": [{"file_id": "nfl_" + "2" * 24, "name": "plan.pdf", "bytes": 2048, "status": "partial",
                               "coverage": {"units_total": 4, "units_text": 3}, "warnings": ["page 4 looks scanned"]}],
                    "used_bytes": 2048, "quota_bytes": 5 << 30}
        if path == f"/v1/nests/{NEST}/approvals":
            return {"approvals": copy.deepcopy(self.cards)}
        if path == f"/v1/nests/{NEST}/usage":
            return {"input_tokens": {"used": 10, "limit": 1000000}, "output_tokens": {"used": 5, "limit": 100000}, "running_secs": 60, "files_bytes": 2048}
        if path.startswith(f"/v1/nests/{NEST}/approvals/"):
            self.calls.append((method, path, identity, data))
            self.cards[0]["status"] = "approved"
            return {"status": "approved"}
        return super().call(method, path, identity, data, **kwargs)


class NestWindowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = Adw.Application(application_id="dev.pigeonpost.NestTests")
        cls.app.register(None)

    def make(self, api):
        self.errors = []
        self.previous_hook = sys.excepthook
        sys.excepthook = lambda kind, error, trace: self.errors.append(error)
        self.api = api
        self.config = tempfile.TemporaryDirectory(prefix="pigeonpost-nests-")
        w = Window(self.app, api=api, restore=False, calls=None, protected=MemoryProtectedStore(),
                   preferences=Preferences(Path(self.config.name) / "p.json"))
        self.opened = []
        w.open_url = self.opened.append
        w.present()
        w.account_loaded(api.identities())
        self.window = w
        return w

    def tearDown(self):
        self.window.close()
        pump(timeout=0.05)
        sys.excepthook = self.previous_hook
        self.config.cleanup()
        self.assertEqual(self.errors, [], "GTK callbacks raised exceptions")

    def test_products_are_hidden_without_the_flag(self):
        w = self.make(NestAPI(features=frozenset()))
        pump(timeout=0.5)
        self.assertFalse(w.products_button.get_visible())
        self.assertFalse(any(c[1].startswith("/v1/nest") for c in self.api.calls))

    def test_products_tabs_packages_and_web_purchase(self):
        w = self.make(NestAPI())
        pump(lambda: w.nest_state["snapshot"] is not None, timeout=5)
        self.assertTrue(w.products_button.get_visible())
        w.show_page("products")
        pump(timeout=0.2)
        shown = " ".join(texts(w.products_content))
        self.assertIn("Pigeonpost AI", shown)
        self.assertNotIn("Pigeonpost AI Pro", shown, "unsellable packages are not offered")
        self.assertIn("1 of 1 Nest in use", shown)
        self.assertIn("Coming soon", " ".join(texts(w.products_services)))
        self.assertEqual(w.products_tabs.get_n_pages(), 3)
        find_button(w.products_content, "Buy, upgrade or manage on the website").emit("clicked")
        self.assertEqual(self.opened[-1], "https://pigeonpost.dev/account#nests")

    def test_nest_page_shows_files_usage_and_approves_with_the_shown_hash(self):
        w = self.make(NestAPI())
        pump(lambda: w.nest_state["snapshot"] is not None, timeout=5)
        w.open_nest(NEST)
        pump(lambda: w.nest_state["detail"] is not None, timeout=5)
        pump(timeout=0.2)
        shown = " ".join(texts(w.nest_content))
        self.assertIn("Working", shown)
        self.assertIn("plan.pdf — Read 3 of 4", shown)
        self.assertIn("Send a message to /alex/main", shown)
        find_button(w.nest_content, "Approve").emit("clicked")
        pump(lambda: any("/approve" in c[1] for c in self.api.calls), timeout=5)
        decision = next(c for c in self.api.calls if c[1].endswith("/approve"))
        self.assertEqual(decision[3], {"args_hash": HASH})


if __name__ == "__main__":
    unittest.main()
