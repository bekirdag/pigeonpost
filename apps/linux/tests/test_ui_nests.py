"""Nests in the real GTK window, with a fake postbox: Products, a Nest's page, approvals. Run under Xvfb."""
import copy
import sys
import tempfile
import unittest
from pathlib import Path

from pigeonpost.api import APIError
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
        self.slots = 1
        self.decision_error = None
        self.detail = {"nest_id": NEST, "address": NEST_MB, "label": "Mira", "state": "running", "status": "working", "role": "owner"}
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
            return {"allowance": {"slots": self.slots, "used": 1, "funding": {"package": "nest.ai.starter.monthly", "source": "apple"}, "duplicates": []},
                    "nests": [{"nest_id": NEST, "address": NEST_MB, "label": "Mira", "state": "running", "status": "working", "funding": "package"}],
                    "setups": []}
        if path == f"/v1/nests/{NEST}" and method == "GET":
            return dict(self.detail)
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
            if self.decision_error:
                raise self.decision_error
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
        self.assertEqual(self.opened[-1], "https://pigeonpost.dev/account#products")

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

    def test_conversation_shows_status_cards_and_decides_with_the_shown_hash(self):
        w = self.make(NestAPI())
        pump(lambda: w.nest_state['snapshot'] is not None, timeout=5)
        w.peer = NEST_MB
        w.render_conversation_chrome()
        pump(lambda: w.nest_state['peer'] is not None and w.nest_state['peer']['cards'] is not None, timeout=5)
        w.render_conversation_chrome()
        self.assertEqual(w.nest_badge.get_text(), 'Working…')
        self.assertTrue(w.nest_file_button.get_visible())
        self.assertTrue(w.nest_panel_holder.get_visible())
        panel_texts = texts(w.nest_panel)
        self.assertIn('Send a message to /alex/main', ' '.join(panel_texts))
        approve_btn = find_button(w.nest_panel, 'Approve')
        self.assertIsNotNone(approve_btn)
        approve_btn.emit('clicked')
        pump(lambda: any(call[1].endswith('/approve') for call in self.api.calls), timeout=5)
        approve_calls = [call for call in self.api.calls if call[1].endswith('/approve')]
        self.assertTrue(approve_calls)
        last_approve_call = approve_calls[-1]
        self.assertEqual(last_approve_call[3], {'args_hash': HASH})

    def test_other_conversations_show_no_nest_chrome(self):
        w = self.make(NestAPI())
        pump(lambda: w.nest_state['snapshot'] is not None, timeout=5)
        w.peer = '/someone/else'
        w.render_conversation_chrome()
        self.assertFalse(w.nest_badge.get_visible())
        self.assertFalse(w.nest_manage_button.get_visible())
        self.assertFalse(w.nest_file_button.get_visible())
        self.assertFalse(w.nest_panel_holder.get_visible())

    def test_chrome_goes_away_when_the_flag_turns_off(self):
        w = self.make(NestAPI())
        pump(lambda: w.nest_state['snapshot'] is not None, timeout=5)
        w.peer = NEST_MB
        w.render_conversation_chrome()
        self.assertTrue(w.nest_manage_button.get_visible())
        w.apply_features(frozenset())
        pump(timeout=0.2)
        self.assertFalse(w.nest_manage_button.get_visible())
        self.assertFalse(w.products_button.get_visible())

    def test_a_nest_paused_for_payment_offers_no_resume_and_warns_of_deletion(self):
        api = NestAPI()
        api.detail.update(state="paused", status="needs_you", status_reason="payment", paused_reason="unfunded", delete_after=1893456000)
        w = self.make(api)
        pump(lambda: w.nest_state["snapshot"] is not None, timeout=5)
        w.open_nest(NEST)
        pump(lambda: w.nest_state["detail"] is not None, timeout=5)
        pump(timeout=0.2)
        self.assertIsNone(find_button(w.nest_content, "Resume"))
        self.assertIsNone(find_button(w.nest_content, "Pause"))
        self.assertIsNotNone(find_button(w.nest_content, "Export…"), "the export window is exactly when export matters")
        self.assertIn("Export it before then", " ".join(texts(w.nest_content)))

    def test_an_owner_pause_can_be_resumed_and_an_operator_sees_no_management(self):
        api = NestAPI()
        api.detail.update(state="paused", status="paused", paused_reason="owner")
        w = self.make(api)
        pump(lambda: w.nest_state["snapshot"] is not None, timeout=5)
        w.open_nest(NEST)
        pump(lambda: w.nest_state["detail"] is not None, timeout=5)
        pump(timeout=0.2)
        self.assertIsNotNone(find_button(w.nest_content, "Resume"))
        api.detail.update(role="operator", state="running", status="ready", paused_reason=None)
        w.open_nest(NEST)
        pump(lambda: w.nest_state["detail"] is not None, timeout=5)
        pump(timeout=0.2)
        for caption in ("Pause", "Rename…", "Export…", "Delete…"):
            self.assertIsNone(find_button(w.nest_content, caption), caption)
        self.assertIsNotNone(find_button(w.nest_content, "Approve"), "operators approve")

    def test_a_refused_approval_stays_on_the_page(self):
        api = NestAPI()
        api.decision_error = APIError(409, "approval_changed")
        w = self.make(api)
        pump(lambda: w.nest_state["snapshot"] is not None, timeout=5)
        w.open_nest(NEST)
        pump(lambda: w.nest_state["detail"] is not None, timeout=5)
        pump(timeout=0.2)
        find_button(w.nest_content, "Approve").emit("clicked")
        pump(lambda: any("/approve" in c[1] for c in api.calls), timeout=5)
        pump(lambda: w.nest_state["detail"] is not None and w.nest_state["message"], timeout=5)
        pump(timeout=0.3)
        self.assertIn("The action changed since you saw it", " ".join(texts(w.nest_content)))

    def test_an_unreadable_card_can_only_be_rejected(self):
        api = NestAPI()
        api.cards[0].update(tool="spend", call=None)
        w = self.make(api)
        pump(lambda: w.nest_state["snapshot"] is not None, timeout=5)
        w.open_nest(NEST)
        pump(lambda: w.nest_state["detail"] is not None, timeout=5)
        pump(timeout=0.2)
        self.assertIsNone(find_button(w.nest_content, "Approve"))
        self.assertIsNotNone(find_button(w.nest_content, "Reject"))

    def test_a_new_nest_is_offered_only_with_a_free_slot(self):
        api = NestAPI()
        w = self.make(api)
        pump(lambda: w.nest_state["snapshot"] is not None, timeout=5)
        w.show_page("products")
        pump(timeout=0.2)
        self.assertIsNone(find_button(w.products_content, "New Nest…"), "1 of 1 slots is in use")
        api.slots = 3
        w.nest_state["loading"] = False
        w.load_nests()
        pump(lambda: w.nest_state["snapshot"]["slots"] == 3, timeout=5)
        pump(timeout=0.2)
        self.assertIsNotNone(find_button(w.products_content, "New Nest…"))

    def test_an_organization_nest_known_by_its_handle_shows_status(self):
        api = NestAPI()
        w = self.make(api)
        pump(lambda: w.nest_state["snapshot"] is not None, timeout=5)
        w.nest_state["snapshot"]["nests"] = []
        w.nest_state["org_directory"] = {NEST_MB: {"nest_id": NEST, "label": "Mira", "role": "operator", "address": NEST_MB}}
        w.messages = [{"peer_handle": "/acme/hr", "peer": NEST_MB}]
        w.peer = "/acme/hr"
        w.render_conversation_chrome()
        pump(lambda: w.nest_state["peer"] is not None and w.nest_state["peer"]["detail"] is not None, timeout=5)
        pump(timeout=0.2)
        self.assertEqual(w.nest_badge.get_text(), "Working…")
        self.assertTrue(w.nest_badge.get_visible())

    def test_products_page_is_refused_without_the_flag(self):
        w = self.make(NestAPI(features=frozenset()))
        pump(timeout=0.3)
        w.show_page("products")
        self.assertEqual(w.page(), "conversation")


if __name__ == "__main__":
    unittest.main()
