"""Phase 6 live streams and Phase 7 Meeting notes in the real GTK window, plus the group composer
keeping focus after Send/Enter. Fake transport and synthetic data only. Run under Xvfb.

The brief opened here (notes_vector.json) was sealed by meeting-intel's own code."""
import copy
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

from pigeonpost import groupcrypto
from pigeonpost import meeting_notes as mn
from pigeonpost.groupstore import MemoryStorage
from pigeonpost.protected import MemoryProtectedStore
from pigeonpost.theme import Preferences
from pigeonpost.ui import Gtk, Window
from test_ui import pump
import test_ui_features as feature_tests
from test_ui_features import ALL, FeatureAPI, WindowCase, find_button, texts, walk

VECTOR = json.loads(Path(__file__).with_name("notes_vector.json").read_text())
GID = "grp_" + "a" * 26
OTHER = "grp_" + "c" * 26
SID = "str_" + "b" * 26
STREAMS = ALL | {"streaming"}
NOTES = ALL | {"meeting_summaries"}
gc = groupcrypto.load()


def group(gid=GID, role="member", policy="admins", name="Garden club"):
    return {"id": gid, "name": name, "join_policy": "private", "post_policy": "everyone", "stream_policy": policy,
            "activation_state": "active", "member_count": 3, "revision": 1, "created_by": "/k/alice", "last_seq": 0,
            "deleted_at": None, "my": {"role": role, "state": "active", "notify": "all", "muted_until": None}}


class StreamAPI(FeatureAPI):
    """Group list and member routes as plain data; `streams` answers GET /v1/groups/{id}/streams."""

    def __init__(self, features):
        super().__init__(features)
        self.streams = {}
        self.patches = []

    def scoped(self, method, path, identity, data=None, **_):
        self.calls.append((method, path, identity, data))
        if path.endswith("/streams"):
            gid = path.split("/")[3]
            return {"streams": [copy.deepcopy(self.streams[gid])] if gid in self.streams else []}
        if path.endswith("/members"):
            return {"members": []}
        if method == "PATCH":
            self.patches.append(data)
            return {"group": data}
        if method == "GET" and path.startswith("/v1/groups/"):
            return {"group": {"revision": 1}}
        raise AssertionError("unexpected group route " + path)


class StreamWindowTests(WindowCase):
    features = STREAMS

    def make_api(self):
        return StreamAPI(self.features)

    def setUp(self):
        super().setUp()
        w = self.window
        self.opened = []
        w.open_url = self.opened.append
        # The MLS device is not what this test is about: the group list is given directly.
        pump(lambda: w.group_state != "loading", timeout=10)
        w.group_state = "ready"
        w.groups = [group(), group(OTHER, name="Other")]

    def live(self, gid=GID, by="/k/alice", stage=()):
        self.api.streams[gid] = {"id": SID, "group_id": gid, "state": "live", "by": by, "title": "Sprint demo",
                                 "sources": ["camera"], "stage": list(stage)}

    def open(self, gid=GID):
        w = self.window
        w.open_group(gid)
        w.poll_group_stream()
        pump(lambda: any(c[1].endswith("/streams") for c in self.api.calls), timeout=3)
        pump(timeout=0.2)

    def test_live_banner_watch_and_stage_invite_open_the_inbox_at_the_group(self):
        w = self.window
        self.live(stage=[{"address": "/k/one", "state": "invited"}])
        w.name_of = lambda a: "You" if a == w.identity else "Alice"
        self.open()
        pump(lambda: w.group_live_banner.get_visible(), timeout=3)
        self.assertTrue(w.group_live_banner.get_visible())
        self.assertEqual(w.group_live_text.get_text(), "Alice is live: Sprint demo")
        self.assertEqual(w.group_live_watch.get_label(), "Watch in browser")
        self.assertIn("invited on stage", w.group_live_stage.get_text())
        self.assertFalse(w.group_go_live.get_visible(), "members may not go live under the admins policy")
        self.shot("group-live-banner")
        w.group_live_watch.emit("clicked")
        self.assertEqual(self.opened, ["https://inbox.pigeonpost.dev/#group=" + GID])
        # The stream ends: the banner goes on the next poll.
        del self.api.streams[GID]
        w.poll_group_stream()
        pump(lambda: not w.group_live_banner.get_visible(), timeout=3)
        self.assertFalse(w.group_live_banner.get_visible())

    def test_go_live_follows_the_policy_and_role(self):
        w = self.window
        w.groups = [group(role="admin")]
        self.open()
        self.assertTrue(w.group_go_live.get_visible())
        self.assertFalse(w.group_live_banner.get_visible())
        w.group_go_live.emit("clicked")
        self.assertEqual(self.opened, ["https://inbox.pigeonpost.dev/#group=" + GID])
        w.groups = [group(role="member", policy="everyone")]
        w.render_group_view()
        self.assertTrue(w.group_go_live.get_visible(), "everyone may go live when the group allows it")
        w.groups = [group(role="restricted", policy="everyone")]
        w.render_group_view()
        self.assertFalse(w.group_go_live.get_visible())
        # While the group is live nobody else is offered Go live.
        w.groups = [group(role="admin")]
        self.live(by="/k/one")
        w.poll_group_stream()
        pump(lambda: w.group_live_banner.get_visible(), timeout=3)
        self.assertFalse(w.group_go_live.get_visible())
        self.assertEqual(w.group_live_text.get_text(), "You are live: Sprint demo")
        self.assertEqual(w.group_live_watch.get_label(), "Open in browser")

    def test_an_answer_for_another_group_or_mailbox_is_dropped(self):
        w = self.window
        self.live()
        w.open_group(GID)
        w.poll_group_stream()
        w.group_open = OTHER  # moved on before the answer arrived
        pump(timeout=0.5)
        self.assertIsNone(w.group_stream)
        w.group_open = GID
        w.poll_group_stream()
        w.identity = "/k/two"
        pump(timeout=0.5)
        self.assertIsNone(w.group_stream, "an answer for another mailbox is not shown")

    def test_who_can_go_live_setting_for_admins(self):
        w = self.window
        w.groups = [group(role="owner")]
        self.open()
        info = w.group_info()
        self.assertIn("Who can go live", texts(info.group_content))
        info.group_go_live_policy.set_selected(1)
        find_button(info.group_content, "Save settings").emit("clicked")
        pump(lambda: self.api.patches, timeout=3)
        self.assertEqual(self.api.patches[-1]["stream_policy"], "everyone")
        info.close()


class NoStreamWindowTests(WindowCase):
    features = ALL

    def make_api(self):
        return StreamAPI(self.features)

    def test_without_the_flag_there_is_no_banner_and_no_request(self):
        w = self.window
        pump(lambda: w.group_state != "loading", timeout=10)
        w.group_state = "ready"
        w.groups = [group(role="owner")]
        w.open_group(GID)
        w.poll_group_stream()
        pump(timeout=0.4)
        self.assertFalse(w.group_go_live.get_visible() or w.group_live_banner.get_visible())
        self.assertFalse(any(c[1].endswith("/streams") for c in self.api.calls))
        info = w.group_info()
        self.assertNotIn("Who can go live", texts(info.group_content))
        info.close()


class NotesAPI(FeatureAPI):
    def __init__(self, features):
        super().__init__(features)
        other = copy.deepcopy(VECTOR["meeting"])
        other["id"] = "mtg_" + "o" * 26
        other["keys"] = [k for k in other["keys"] if k["key_id"] != "nkVectorKey01"]
        self.briefs = {VECTOR["meeting"]["id"]: copy.deepcopy(VECTOR["meeting"]), other["id"]: other}
        self.hold = None

    def call(self, method, path, identity=None, data=None, **kwargs):
        if path.startswith("/v1/meetings"):
            self.calls.append((method, path, identity, data))
            if self.hold is not None:
                self.hold.wait(5)
            if path == "/v1/meetings":
                return {"meetings": [{"id": mid, "kind": "room", "state": "ready", "created_at": 1793000000 + n, "expires_at": 1800000000,
                                      "host": "/alex/main", "as": "/k/one"} for n, mid in enumerate(self.briefs)]}
            if path.startswith("/v1/meetings/keys/"):
                return {"registered": True} if method == "PUT" else {"deleted": True}
            mid = path.rsplit("/", 1)[1]
            if method == "DELETE":
                self.briefs.pop(mid, None)
                return {"removed": True}
            return copy.deepcopy(self.briefs[mid])
        return super().call(method, path, identity, data, **kwargs)


@unittest.skipUnless(mn.crypto_available(), "python3-cryptography not installed")
class NotesWindowTests(WindowCase):
    features = NOTES

    def make_api(self):
        return NotesAPI(self.features)

    def setUp(self):
        self.errors = []
        self.previous_hook = sys.excepthook
        sys.excepthook = lambda kind, error, trace: self.errors.append(error)
        self.api = self.make_api()
        self.config = tempfile.TemporaryDirectory(prefix="pigeonpost-prefs-")
        self.store = MemoryProtectedStore()
        self.window = Window(self.app, api=self.api, restore=False, preferences=Preferences(Path(self.config.name) / "p.json"),
                             protected=self.store)
        self.window.session.owner = "owner-a"
        # Never the desktop keyring in tests: group devices use memory storage, as in WindowCase.
        self.window.group_storage_factory = lambda _gc: MemoryStorage()
        self.window.present()
        self.window.account_loaded(self.api.identities())
        pump(lambda: self.window.known_ids is not None and self.window.features == self.features, timeout=5)
        pump(lambda: self.window.notes_state["key_id"], timeout=5)

    def use_vector_key(self):
        self.store.update(self.window.lease, mn.KEY_SLOT, lambda record: record.update(notes_key=VECTOR["key"]))

    def labels(self):
        return [w for w in walk(self.window.notes_content) if isinstance(w, Gtk.Label)]

    def test_registers_this_computers_key_lists_opens_copies_and_deletes(self):
        w = self.window
        puts = [c for c in self.api.calls if c[0] == "PUT"]
        self.assertEqual(len(puts), 1)
        self.assertEqual(puts[0][1], "/v1/meetings/keys/" + w.notes_state["key_id"])
        self.assertEqual(puts[0][3]["label"], "Linux app")
        self.assertTrue(w.notes_button.get_visible())
        self.use_vector_key()
        w.notes_button.emit("clicked")
        pump(lambda: any(getattr(r, "meeting", None) for r in walk(w.notes_content)), timeout=3)
        rows = [r for r in walk(w.notes_content) if getattr(r, "meeting", None)]
        self.assertEqual(len(rows), 2)
        self.shot("meeting-notes-list")
        rows[0].emit("activated")
        pump(lambda: "Ship 1.0.11 on Thursday" in " ".join(texts(w.notes_content)), timeout=5)
        shown = " ".join(texts(w.notes_content))
        self.assertIn("Release review", shown)
        self.assertIn("Alex: Write the release notes (due Wednesday)", shown)
        agreed = next(l for l in self.labels() if "<b>Agreed</b>" in l.get_text())
        self.assertFalse(agreed.get_use_markup(), "transcript text is never markup")
        self.assertIsNotNone(find_button(w.notes_content, "Copy as text"))
        self.shot("meeting-notes-brief")
        w.notes_delete(VECTOR["meeting"]["id"])
        pump(lambda: ("DELETE", "/v1/meetings/" + VECTOR["meeting"]["id"], None, None) in self.api.calls, timeout=3)
        pump(lambda: len([r for r in walk(w.notes_content) if getattr(r, "meeting", None)]) == 1, timeout=3)

    def test_notes_for_other_devices_say_so(self):
        w = self.window
        self.use_vector_key()
        w.show_page("notes")
        w.notes_open("mtg_" + "o" * 26)
        pump(lambda: mn.NO_KEY in texts(w.notes_content), timeout=5)
        self.assertNotIn("Release review", " ".join(texts(w.notes_content)))

    def test_nothing_from_a_signed_out_login_is_shown(self):
        import threading
        w = self.window
        self.use_vector_key()
        w.show_page("notes")
        self.api.hold = threading.Event()
        w.notes_open(VECTOR["meeting"]["id"])
        pump(timeout=0.2)
        w.generation += 1
        w.clear_account()  # sign-out while the brief is still loading
        self.api.hold.set()
        pump(timeout=0.8)
        self.assertNotIn("Release review", " ".join(texts(w.notes_content)))
        self.assertFalse(w.notes_button.get_visible())
        self.assertIsNone(w.notes_state["key_id"])


@unittest.skipIf(gc is None, "group crypto library not built (apps/linux/build-native.sh)")
class GroupComposerFocusTests(WindowCase):
    make_api = feature_tests.GroupWindowTests.make_api
    bob = feature_tests.GroupWindowTests.bob
    cycle = feature_tests.GroupWindowTests.cycle

    def test_group_composer_keeps_focus_after_send_and_enter(self):
        from pigeonpost.ui import Gdk
        w = self.window
        pump(lambda: w.group_state == "ready", timeout=10)
        gid = w.group_client().create_group({"name": "Focus"})["group"]["id"]
        self.assertTrue(self.cycle(lambda: w.group_by_id(gid) is not None))
        w.open_group(gid)
        pump(lambda: w.group_composer_box.get_visible(), timeout=5)
        w.group_composer.get_buffer().set_text("First")
        w.group_send_button.grab_focus()
        w.group_send_button.emit("clicked")
        pump(lambda: not w.group_sending and w.get_focus() is w.group_composer, timeout=10)
        self.assertIs(w.get_focus(), w.group_composer, "Send gives the focus back to the message box")
        w.group_composer.get_buffer().set_text("Second")
        w.group_composer_keys(None, Gdk.KEY_Return, 0, Gdk.ModifierType(0))
        pump(lambda: not w.group_sending and w.get_focus() is w.group_composer, timeout=10)
        self.assertIs(w.get_focus(), w.group_composer, "Enter keeps the focus in the message box")
        texts_sent = [x.get("text") for x in w.group_client().history(gid) if x.get("kind") == "message"]
        self.assertEqual(texts_sent, ["First", "Second"])


if __name__ == "__main__":
    unittest.main()
