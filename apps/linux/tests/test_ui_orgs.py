"""Organization rooms and machine approval in the real GTK window, with a fake postbox. Run under Xvfb."""
import copy
import json
import sys
import tempfile
import time
import unittest
from pathlib import Path

from pigeonpost import pairing
from pigeonpost.api import APIError
from pigeonpost.protected import MemoryProtectedStore
from pigeonpost.theme import Preferences
from pigeonpost.ui import Adw, Window
from test_ui import FakeAPI, pump
from test_ui_features import find_button, texts

ORG_MB = "/k/" + "o" * 26
AGENT = "/k/" + "g" * 26
NOW = int(time.time())


def catalog():
    return {"orgs": [{"id": "org_1", "name": "Acme", "namespace": "acme", "status": "active", "my_member_id": "mem_1",
                      "my_roles": [], "settings": {"rooms_creation": "everyone"},
                      "my_mailbox": {"address": ORG_MB, "handle": "/acme/john"}}],
            "operated": [{"address": AGENT, "handle": "/acme/agent1", "org_id": "org_1", "capabilities": ["read_inbox"]}]}


def pairing_preview(**overrides):
    raw = {"pairing_id": "pair_1", "org": {"id": "org_1", "name": "Acme"}, "handle": "/acme/agent1", "address": None,
           "creates_agent": True, "label": "Build box", "confirmation": "123 456", "capabilities": ["mailbox"], "ip_country": "TR",
           "requested_at": NOW - 5, "expires_at": NOW + 600, "state": "pending", "can_approve": True, "reason": None}
    raw.update(overrides)
    return raw


class OrgAPI(FakeAPI):
    def __init__(self):
        super().__init__()
        self.rooms = [{"id": "grp_gen", "name": "General", "description": "Everyone", "member_count": 4, "join_policy": "public", "my_state": "active"},
                      {"id": "grp_des", "name": "Design", "description": None, "member_count": 2, "join_policy": "public", "my_state": None},
                      {"id": "grp_brd", "name": "Board", "description": None, "member_count": 2, "join_policy": "private", "my_state": None}]
        self.scoped_calls = []
        self.create_failures = []
        self.rooms_error = None
        self.resolves = [pairing_preview()]

    def identities(self):
        return [{"address": "/k/one", "handle": "/demo/main"}, {"address": ORG_MB, "handle": "/acme/john"}]

    def capabilities(self):
        return frozenset({"orgs"})

    def call(self, method, path, identity=None, data=None, **kwargs):
        if path == "/v1/orgs":
            self.calls.append((method, path, identity, data))
            return copy.deepcopy(catalog())
        if path.startswith("/v1/device-pairings"):
            self.calls.append((method, path, identity, data))
            if path.endswith("/resolve"):
                result = self.resolves.pop(0) if len(self.resolves) > 1 else self.resolves[0]
                return copy.deepcopy(result)
            if path.endswith("/deny"):
                return {"denied": True}
            if path.endswith("/approve"):
                return {"approved": True, "address": "/k/" + "n" * 26, "handle": "/acme/agent1"}
        return super().call(method, path, identity, data, **kwargs)

    def scoped(self, method, path, identity, data=None, **kwargs):
        self.scoped_calls.append((method, path, identity, data, kwargs.get("raw")))
        if path.startswith("/v1/rooms?"):
            if self.rooms_error:
                raise self.rooms_error
            return {"rooms": copy.deepcopy(self.rooms)}
        if path == "/v1/groups/resolve":
            room = next(r for r in self.rooms if r["id"] == data["group_id"])
            return {"id": room["id"], "name": room["name"], "member_count": room["member_count"],
                    "join_policy": room["join_policy"], "my_state": room["my_state"], "can_join": room["join_policy"] == "public"}
        if path.endswith("/join"):
            return {"state": "keying"}
        if path == "/v1/groups" and method == "POST":
            if self.create_failures:
                raise self.create_failures.pop(0)
            body = json.loads(kwargs["raw"])
            return {"group": {"id": "grp_new", "name": body["name"], "org_id": body["org_id"], "created_by": identity,
                              "my": {"role": "owner", "state": "keying"}}}
        return {}


class OrgWindowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = Adw.Application(application_id="dev.pigeonpost.OrgTests")
        cls.app.register(None)

    def setUp(self):
        self.errors = []
        self.previous_hook = sys.excepthook
        sys.excepthook = lambda kind, error, trace: self.errors.append(error)
        self.api = OrgAPI()
        self.config = tempfile.TemporaryDirectory(prefix="pigeonpost-orgs-")
        self.protected = MemoryProtectedStore()
        self.window = w = Window(self.app, api=self.api, restore=False, calls=None, protected=self.protected,
                                 preferences=Preferences(Path(self.config.name) / "p.json"))
        self.confirms, self.opened, self.cycles = [], [], []
        w.confirm = lambda title, body, action, caption="Delete": (self.confirms.append(title), action())
        w.open_url = self.opened.append
        w.groups_on = lambda: True
        w.groups_cycle = lambda force=False: self.cycles.append(force)
        w.present()
        w.account_loaded(self.api.identities())
        pump(lambda: w.features == frozenset({"orgs"}) and w.org_state["catalog"] is not None, timeout=5)

    def tearDown(self):
        self.window.close()
        pump(timeout=0.05)
        sys.excepthook = self.previous_hook
        self.config.cleanup()
        self.assertEqual(self.errors, [], "GTK callbacks raised exceptions")

    def page_texts(self):
        return texts(self.window.orgs_content)

    def use_org_inbox(self):
        w = self.window
        w.show_page("orgs")
        pump(timeout=0.2)
        find_button(w.orgs_content, "Use /acme/john to see its rooms").emit("clicked")
        pump(lambda: w.org_state["rooms"] is not None and w.org_state["pending_for"] == ORG_MB, timeout=5)

    def test_catalog_directory_and_operated_agents(self):
        w = self.window
        self.assertTrue(w.orgs_button.get_visible())
        w.show_page("orgs")
        pump(timeout=0.2)
        shown = " ".join(self.page_texts())
        self.assertIn("Your inbox here: /acme/john", shown)
        self.assertIn("Agents you operate", shown)
        self.assertNotIn(AGENT, [m["address"] for m in w.mailboxes])  # an operated agent never becomes a mailbox
        self.assertIsNone(w.org_state["rooms"])  # the personal inbox does not browse rooms
        self.use_org_inbox()
        self.assertEqual(w.identity, ORG_MB)
        rooms_call = [c for c in self.api.scoped_calls if c[1].startswith("/v1/rooms?")][-1]
        self.assertEqual(rooms_call[2], ORG_MB)
        self.assertIn("org_id=org_1", rooms_call[1])
        self.assertIsNotNone(find_button(w.orgs_content, "Open"))
        self.assertIsNotNone(find_button(w.orgs_content, "Join…"))
        self.assertIn("Invite only", self.page_texts())

    def test_join_asks_server_then_person(self):
        w = self.window
        self.use_org_inbox()
        find_button(w.orgs_content, "Join…").emit("clicked")
        pump(lambda: any(c[1].endswith("/join") for c in self.api.scoped_calls), timeout=5)
        paths = [c[1] for c in self.api.scoped_calls]
        self.assertLess(paths.index("/v1/groups/resolve"), paths.index("/v1/groups/grp_des/join"))
        self.assertEqual(self.confirms, ["Join Design?"])
        pump(lambda: self.cycles, timeout=5)
        self.assertTrue(self.cycles[-1])

    def test_refusal_clears_cached_access(self):
        w = self.window
        self.api.rooms_error = APIError(403, "not_org_member")
        w.show_page("orgs")
        pump(timeout=0.2)
        find_button(w.orgs_content, "Use /acme/john to see its rooms").emit("clicked")
        pump(lambda: w.org_state["catalog"] is None, timeout=5)
        self.assertIn("Access could not be confirmed", " ".join(self.page_texts()))

    def test_room_creation_custody_and_exact_retry(self):
        w = self.window
        self.use_org_inbox()
        self.api.create_failures = [APIError(0, "network_error")]
        org = w.selected_org()
        dialog = w.new_org_room(org)
        dialog.new_room["name"].set_text("Launch")
        dialog.new_room["submit"]()
        pump(lambda: w.org_state["pending"] and not w.org_state["busy"], timeout=5)
        first = [c for c in self.api.scoped_calls if c[1] == "/v1/groups"][-1][4]
        self.assertIn("wasn’t confirmed", " ".join(self.page_texts()))
        self.assertIsNotNone(find_button(w.orgs_content, "Retry"))
        # Survives this window's state: the protected record holds the exact bytes.
        record = self.protected.read(w.mailbox_lease, ORG_MB)
        self.assertEqual(len(record["orgs"]["pending_rooms"]), 1)
        find_button(w.orgs_content, "Retry").emit("clicked")
        pump(lambda: not w.org_state["pending"] and not w.org_state["busy"], timeout=5)
        second = [c for c in self.api.scoped_calls if c[1] == "/v1/groups"][-1][4]
        self.assertEqual(first, second)
        self.assertEqual(len([c for c in self.api.scoped_calls if c[1] == "/v1/groups"]), 2)
        self.assertEqual(self.protected.read(w.mailbox_lease, ORG_MB)["orgs"]["pending_rooms"], [])
        self.assertTrue(self.cycles)

    def test_definite_refusal_forgets_attempt(self):
        w = self.window
        self.use_org_inbox()
        self.api.create_failures = [APIError(409, "idempotency_conflict")]
        dialog = w.new_org_room(w.selected_org())
        dialog.new_room["name"].set_text("Launch")
        dialog.new_room["submit"]()
        pump(lambda: not w.org_state["busy"] and w.org_state["pending_for"] == ORG_MB and self.api.scoped_calls[-1][1] != "/v1/groups", timeout=5)
        pump(timeout=0.3)
        self.assertEqual(self.protected.read(w.mailbox_lease, ORG_MB)["orgs"]["pending_rooms"], [])

    def test_default_client_offers_native_verification(self):
        self.assertTrue(self.window.strong_signin().available)

    def test_machine_approval_goes_to_browser_and_denies_natively(self):
        w = self.window

        class Unavailable:  # an OAuth client without the loopback flow keeps the browser path
            available = False
        w.pairing_strong = Unavailable()
        dialog = w.approve_device()
        entry = dialog.pairing["entry"]
        entry.set_text("https://inbox.pigeonpost.dev/pair.html#abcd-efgh")
        entry.emit("activate")
        pump(lambda: w.pairing_store.snapshot()["phase"] == pairing.REVIEW, timeout=5)
        pump(timeout=0.1)
        shown = " ".join(texts(dialog.pairing["content"]))
        for expected in ("/acme/agent1", "123 456", "Approving creates this agent", "Build box",
                         "from the network; not proof of anything", "Check that your terminal shows the same number",
                         "can’t do yet"):
            self.assertIn(expected, shown)
        self.assertIsNone(find_button(dialog.pairing["content"], "Hold to approve"))
        find_button(dialog.pairing["content"], "Approve in browser").emit("clicked")
        self.assertEqual(self.opened, ["https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH"])
        self.assertFalse(any("/approve" in c[1] for c in self.api.calls))
        self.api.resolves = [pairing_preview(state="denied", can_approve=False)]
        find_button(dialog.pairing["content"], "Deny").emit("clicked")
        pump(lambda: w.pairing_store.snapshot()["phase"] == pairing.DENIED, timeout=5)
        pump(timeout=0.1)
        self.assertIn("Denied. The machine was not signed in.", texts(dialog.pairing["content"]))
        dialog.close()
        pump(timeout=0.05)
        self.assertIsNone(w.pairing_store.snapshot()["code"])  # forgotten with the dialog

    def test_keyboard_approval_confirms_when_capable(self):
        w = self.window

        class Capable:
            available = True
        w.pairing_strong = Capable()
        dialog = w.approve_device()
        dialog.pairing["entry"].set_text("ABCD-EFGH")
        dialog.pairing["entry"].emit("activate")
        pump(lambda: w.pairing_store.snapshot()["phase"] == pairing.REVIEW, timeout=5)
        pump(timeout=0.1)
        hold = find_button(dialog.pairing["content"], "Hold to approve")
        self.assertIsNotNone(hold)
        hold.emit("clicked")  # keyboard/AT activation: confirmation first
        pump(lambda: w.pairing_store.snapshot()["phase"] == pairing.APPROVED, timeout=5)
        self.assertEqual(self.confirms, ["Approve this machine?"])
        self.assertEqual(len([c for c in self.api.calls if c[1].endswith("/approve")]), 1)

    def test_sign_out_retires_approvals(self):
        w = self.window
        w.approve_device()
        store = w.pairing_store
        w.clear_account()
        self.assertIsNone(w.pairing_window)
        self.assertFalse(store.current)
        self.assertIsNone(w.org_state["catalog"])
        self.assertFalse(w.orgs_button.get_visible())


if __name__ == "__main__":
    unittest.main()
