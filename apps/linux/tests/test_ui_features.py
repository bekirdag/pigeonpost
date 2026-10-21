"""Phase 1b and 3 in the real GTK window: call history, Filtered, privacy settings, blocking,
profile pictures, disappearing timers and end-to-end encrypted groups. The postbox is faked; the
group encryption is the real library against tests/fake_groups_postbox.py. Run under Xvfb."""
import copy
import os
import sys
import tempfile
import time
import unittest
import urllib.parse
from pathlib import Path

from pigeonpost import groupcrypto
from pigeonpost.features import call_detail, is_blocked, missed_call_lines, parse_invite_link, square_crop, wildcard_for
from pigeonpost.groups_core import GroupClient
from pigeonpost.groupstore import MemoryStorage
from pigeonpost.theme import Preferences
from pigeonpost.ui import Adw, Gtk, Window
from test_ui import FakeAPI, pump

gc = groupcrypto.load()
if gc is None and os.environ.get("PIGEONPOST_REQUIRE_GROUPCRYPTO") == "1":
    raise RuntimeError("group crypto library missing: " + str(groupcrypto.reason()))
if gc is not None:
    from fake_groups_postbox import FakePostbox

NOW = int(time.time())
ALL = frozenset({"message_filtering", "call_admission", "call_history", "avatars", "groups", "disappearing"})


def walk(widget):
    yield widget
    child = widget.get_first_child()
    while child:
        yield from walk(child)
        child = child.get_next_sibling()


def texts(widget):
    return [w.get_text() for w in walk(widget) if isinstance(w, Gtk.Label)]


def find_button(widget, text):
    for w in walk(widget):
        if isinstance(w, Gtk.Button) and (w.get_label() == text or w.get_tooltip_text() == text
                                           or any(isinstance(c, Gtk.Label) and c.get_text() == text for c in walk(w))):
            return w
    return None


class FeatureAPI(FakeAPI):
    """FakeAPI plus the Phase 1b routes and, when a FakePostbox is attached, the group routes."""

    def __init__(self, features=ALL, postbox=None, me="/k/one"):
        super().__init__()
        self.features, self.postbox, self.me = features, postbox, me
        self.policy = {"unknown_calls": "ring", "unnamed_calls": "silence", "unnamed_messages": "show",
                       "unknown_messages": "show", "low_reputation": "filter", "group_members_known": False, "call_history_days": 180}
        self.contacts = [{"peer": "/alex/main", "alias": "Alex", "autonomy": "review"}]
        self.entries = [
            {"id": "clg_1", "direction": "incoming", "outcome": "missed", "kind": "video", "peer": "/k/alexkey", "peer_handle": "/alex/main",
             "participants": [], "started_at": NOW - 60, "seen": False},
            {"id": "clg_2", "direction": "outgoing", "outcome": "answered", "kind": "audio", "peer_handle": "/studio/reviewer",
             "participants": [], "started_at": NOW - 7200, "answered_at": NOW - 7190, "ended_at": NOW - 7000, "duration": 190, "seen": True},
        ]
        self.filtered_rows = [{"message_id": "f1", "from": "/k/strangerkey", "peer": "/k/strangerkey", "body": "Cheap watches",
                               "received_at": NOW - 30, "filtered": True}]
        self.avatar = None
        self.uploads = []

    def capabilities(self):
        return self.features

    def call(self, method, path, identity=None, data=None, **kwargs):
        query = kwargs.get("query") or {}
        if path.startswith("/v1/profile/avatar?"):
            self.calls.append((method, path, identity, data))
            self.uploads.append((path, kwargs.get("raw"), kwargs.get("headers")))
            self.avatar = {"id": "av_1", "version": 1, "visibility": "known", "hash": "h1", "urls": {"jpeg": "/v1/avatar-blobs/av_1/1/256.jpg"}}
            return {"avatar": self.avatar}
        if path == "/v1/contacts":
            self.calls.append((method, path, identity, data))
            if method == "PUT":
                self.contacts = [c for c in self.contacts if c["peer"] != data["peer"]] + [dict(data)]
                return dict(data)
            if method == "DELETE":
                self.contacts = [c for c in self.contacts if c["peer"] != query.get("peer")]
                return {"removed": True}
            rows = [c for c in self.contacts if c.get("admission") == query["admission"]] if query.get("admission") else self.contacts
            return {"contacts": copy.deepcopy(rows), "vocabulary": {"grantable": ["summarize"], "never_auto": []}}
        if path == "/v1/calls/log":
            self.calls.append((method, path, identity, query))
            if method == "DELETE":
                self.entries = []
                return {"deleted": 2}
            wanted = query.get("filter", "all")
            rows = [e for e in self.entries if (e["outcome"] == "filtered") == (wanted == "filtered")
                    and (wanted != "missed" or e["outcome"] == "missed")]
            return {"entries": copy.deepcopy(rows), "unseen_missed": len([e for e in self.entries if e["outcome"] == "missed" and not e["seen"]]),
                    "next_before": None}
        if path == "/v1/calls/log/seen":
            self.calls.append((method, path, identity, data))
            for e in self.entries:
                e["seen"] = True
            return {"updated": 1, "unseen_missed": 0}
        if path == "/v1/inbox" and query.get("filtered") == "1":
            return {"messages": copy.deepcopy(self.filtered_rows)}
        if path == "/v1/filtered/accept":
            self.calls.append((method, path, identity, data))
            self.filtered_rows = []
            return {"accepted": True, "moved": 1}
        if path == "/v1/policy":
            self.calls.append((method, path, identity, data))
            if method == "PUT":
                self.policy.update(data)
            return dict(self.policy)
        if path == "/v1/avatars":
            return {"avatars": {k: None for k in query.get("addresses", "").split(",")}}
        if path == "/v1/profile/avatar":
            self.calls.append((method, path, identity, data))
            if method == "DELETE":
                self.avatar = None
                return {"removed": True}
            return {"avatar": self.avatar}
        if path.startswith("/v1/avatar-blobs/"):
            return b""
        if path == "/v1/threads" and method == "GET":
            return {"threads": [{"thread_id": "release", "peer": "/alex/main", "title": "Linux release", "is_default": True,
                                 "ephemeral_secs": getattr(self, "timer", None)},
                                {"thread_id": "design", "peer": "/alex/main", "title": "Design notes"}]}
        if path.startswith("/v1/threads/") and method == "PATCH":
            self.calls.append((method, path, identity, data))
            self.timer = data["ephemeral_secs"]
            return {"ok": True, "ephemeral_secs": self.timer}
        return super().call(method, path, identity, data, **kwargs)

    def scoped(self, method, path, identity, data=None, *, device=None, raw=None, binary=False, **_):
        self.calls.append((method, path, identity, data))
        if not self.postbox:
            raise AssertionError("no groups postbox")
        me = self.me_address
        if path.startswith("/v1/groups?state="):
            return {"groups": self.postbox.list_groups(me)}
        return self.postbox.client_api(me)(method, path, body=data, device=device, raw=raw, binary=binary)


APP = None


class WindowCase(unittest.TestCase):
    features = ALL

    @classmethod
    def setUpClass(cls):
        global APP
        if APP is None:
            APP = Adw.Application(application_id="dev.pigeonpost.FeatureTests")
            APP.register(None)
        cls.app = APP

    def make_api(self):
        return FeatureAPI(self.features)

    def setUp(self):
        self.errors = []
        self.previous_hook = sys.excepthook
        sys.excepthook = lambda kind, error, trace: self.errors.append(error)
        self.api = self.make_api()
        self.config = tempfile.TemporaryDirectory(prefix="pigeonpost-prefs-")
        self.window = Window(self.app, api=self.api, restore=False, preferences=Preferences(Path(self.config.name) / "p.json"))
        self.window.group_storage_factory = lambda _gc: MemoryStorage()
        self.window.present()
        self.window.account_loaded(self.api.identities())
        pump(lambda: self.window.known_ids is not None and self.window.features == self.features, timeout=5)

    def tearDown(self):
        for dialog in list(self.window.dialogs):
            dialog.close()
        self.window.close()
        pump(timeout=0.05)
        sys.excepthook = self.previous_hook
        self.config.cleanup()
        self.assertEqual(self.errors, [], "GTK callbacks raised exceptions")

    def shot(self, name):
        """Screenshots of fixture data for review, only when CI asks for them."""
        directory = os.environ.get("PIGEONPOST_SCREENSHOT_DIR")
        if not directory:
            return
        import subprocess
        os.makedirs(directory, exist_ok=True)
        pump(timeout=0.4)
        subprocess.run(["import", "-window", "root", f"{directory}/{name}.png"], check=True)

    def select(self, peer="/alex/main"):
        row = self.window.peer_list.get_first_child()
        while row and getattr(row, "peer", None) != peer:
            row = row.get_next_sibling()
        self.assertIsNotNone(row, peer)
        self.window.peer_list.select_row(row)
        pump(timeout=0.05)


class OlderServerTests(WindowCase):
    features = frozenset()

    def test_everything_new_stays_hidden_against_an_older_postbox(self):
        w = self.window
        self.assertFalse(w.calls_button.get_visible())
        self.assertFalse(w.new_group_button.get_visible())
        self.assertNotIn("Filtered", " ".join(texts(w.peer_list)))
        self.assertIsNone(w.group_client())
        self.select()
        self.assertFalse(w.timer_badge.get_visible())


class Phase1bTests(WindowCase):
    def test_calls_page_badge_rows_seen_and_missed_line(self):
        w = self.window
        pump(lambda: w.call_state["loaded"], timeout=5)
        self.assertTrue(w.calls_button.get_visible())
        self.assertIn("1", w.calls_button_label.get_text())
        w.show_page("calls")
        pump(lambda: any(c[1] == "/v1/calls/log/seen" for c in self.api.calls), timeout=5)
        rows = [r for r in walk(w.calls_list) if isinstance(r, Adw.ActionRow)]
        self.assertEqual([r.get_title() for r in rows], ["Alex", "Studio Reviewer"])
        self.assertTrue(rows[0].has_css_class("pp-call-missed"))
        self.assertIn("Missed", rows[0].get_subtitle())
        self.assertIn("3:10", rows[1].get_subtitle())
        self.shot("calls")
        pump(lambda: w.call_state["unseen"] == 0, timeout=3)
        self.assertEqual(w.calls_button_label.get_text(), "Calls")
        # The missed call reads as a line in the conversation.
        rows[0].emit("activated")
        pump(timeout=0.1)
        self.assertEqual(w.page(), "conversation")
        self.assertEqual(w.peer, "/alex/main")
        self.assertTrue(any(t.startswith("Missed video call · ") for t in texts(w.message_list)))
        # Delete from history.
        w.show_page("calls")
        rows = [r for r in walk(w.calls_list) if isinstance(r, Adw.ActionRow)]
        rows[1].actions.activate_action("delete", None)
        pump(lambda: any(c[0] == "DELETE" and c[1].startswith("/v1/calls/log/") for c in self.api.calls), timeout=3)
        pump(lambda: len(w.call_state["entries"]) == 1, timeout=3)

    def test_filtered_row_page_and_accept(self):
        w = self.window
        pump(lambda: w.filtered, timeout=5)
        labels = texts(w.peer_list)
        self.assertIn("Filtered (1)", labels)
        last = w.peer_list.get_last_child()
        self.assertEqual(last.kind, "filtered")
        w.peer_list.select_row(last)
        pump(timeout=0.05)
        self.assertEqual(w.page(), "filtered")
        self.assertIn("Cheap watches", texts(w.filtered_list))
        self.shot("filtered")
        find_button(w.filtered_list, "Accept").emit("clicked")
        pump(lambda: any(c[1] == "/v1/filtered/accept" for c in self.api.calls), timeout=3)
        accept = next(c for c in self.api.calls if c[1] == "/v1/filtered/accept")
        self.assertEqual(accept[3], {"peer": "/k/strangerkey"})
        pump(lambda: not w.filtered, timeout=3)

    def test_privacy_page_saves_one_field_and_blocked_page_unblocks(self):
        w = self.window
        w.settings()
        navigation = w.dialogs[-1].settings_navigation
        titles = [r.get_title() for r in walk(navigation.stack.get_visible_child()) if isinstance(r, Adw.ActionRow)]
        self.assertEqual(titles[:4], ["Account", "Profile picture", "Privacy & filtering", "Blocked"])
        w.privacy_settings(navigation)
        pump(lambda: any(c[1] == "/v1/policy" for c in self.api.calls) and "Low-reputation senders" in texts(navigation.privacy_rows), timeout=3)
        pump(timeout=0.2)
        strip = next(x for x in walk(navigation.privacy_rows) if x.get_name() == "policy-unknown_calls")
        self.assertTrue(strip.toggles["ring"].get_active())
        strip.toggles["silence"].set_active(True)
        pump(lambda: self.api.policy["unknown_calls"] == "silence", timeout=3)
        put = [c for c in self.api.calls if c[0] == "PUT" and c[1] == "/v1/policy"]
        self.assertEqual(put[-1][3], {"unknown_calls": "silence"})
        self.shot("settings-privacy")
        history = next(x for x in walk(navigation.privacy_rows) if x.get_name() == "policy-call_history_days")
        history.toggles["30"].set_active(True)
        pump(lambda: self.api.policy["call_history_days"] == 30, timeout=3)

        self.api.contacts.append({"peer": "/spamco/*", "admission": "block"})
        w.blocked_settings(navigation)
        pump(lambda: "/spamco/*" in texts(navigation.blocked_list), timeout=3)
        self.assertIn("Everyone at /spamco", texts(navigation.blocked_list))
        find_button(navigation.blocked_list, "Unblock").emit("clicked")
        pump(lambda: not any(c["peer"] == "/spamco/*" for c in self.api.contacts), timeout=3)

    def test_block_from_a_message_with_report_banner_and_composer(self):
        w = self.window
        self.select()
        message = next(m for m in w.messages if m.get("direction") != "out")
        dialog = w.ask_block("/alex/main", "Alex", message_id=message["message_id"])
        report, wide = dialog.block_controls
        self.assertIsNotNone(wide, "a namespace block is offered for a handle")
        self.assertIn("won’t be told", dialog.get_body())
        report.set_active(True)
        dialog.response("block")
        pump(lambda: any(c[1] == "/v1/report-spam" for c in self.api.calls), timeout=3)
        put = next(c for c in self.api.calls if c[0] == "PUT" and c[1] == "/v1/contacts")
        self.assertEqual(put[3]["peer"], "/alex/main")
        self.assertEqual(put[3]["admission"], "block")
        self.assertEqual(next(c for c in self.api.calls if c[1] == "/v1/report-spam")[3], {"message_id": message["message_id"]})
        pump(lambda: w.blocked_banner.get_visible(), timeout=3)
        self.shot("blocked-conversation")
        self.assertFalse(w.composer.get_sensitive(), "the composer is off in a blocked conversation")
        find_button(w.blocked_banner, "Unblock").emit("clicked")
        pump(lambda: not w.blocked_banner.get_visible(), timeout=3)
        self.assertTrue(w.composer.get_sensitive())

    def test_wildcard_block_covers_the_namespace(self):
        w = self.window
        dialog = w.ask_block("/alex/main", "Alex")
        dialog.block_controls[1].set_active(True)
        dialog.response("block")
        pump(lambda: any(c[0] == "PUT" and c[1] == "/v1/contacts" for c in self.api.calls), timeout=3)
        put = next(c for c in self.api.calls if c[0] == "PUT" and c[1] == "/v1/contacts")
        self.assertEqual(put[3]["peer"], "/alex/*")
        self.assertFalse(any(c[1] == "/v1/report-spam" for c in self.api.calls), "report is off by default")

    def test_profile_picture_crop_uploads_a_square_jpeg(self):
        from gi.repository import GdkPixbuf
        from pigeonpost.window_privacy import CropDialog
        w = self.window
        pixbuf = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, True, 8, 1600, 900)
        pixbuf.fill(0x3366ccff)
        saved = []
        crop = CropDialog(w, pixbuf, saved.append)
        crop.scale.set_value(2)
        crop.offset = [1.0, -1.0]
        self.assertEqual(crop.rect(), (1150, 0, 450))
        crop.submit()
        self.assertEqual(saved[0]["side"], 450)
        crop2 = CropDialog(w, pixbuf, lambda _: None)
        self.shot("crop")
        crop2.dialog.close()
        self.assertTrue(saved[0]["bytes"].startswith(b"\xff\xd8"), "JPEG, with no metadata of the original")
        refreshed = []
        w.upload_avatar(saved[0], "everyone", True, lambda: refreshed.append(True))
        pump(lambda: refreshed, timeout=3)
        path, raw, headers = self.api.uploads[0]
        query = dict(urllib.parse.parse_qsl(path.split("?", 1)[1]))
        self.assertEqual(query, {"identity": "/k/one", "crop": "0,0,450,450", "visibility": "everyone", "all": "1"})
        self.assertEqual(headers["Content-Type"], "image/jpeg")
        self.assertEqual(raw, saved[0]["bytes"])

    def test_disappearing_timer_badge_dialog_and_local_expiry(self):
        w = self.window
        self.select()
        self.assertFalse(w.timer_badge.get_visible())
        dialog = w.timer_dialog("Disappearing messages", None, "note", lambda secs: w.set_direct_timer(w.direct_timer_thread(), secs))
        next(check for secs, check in dialog.timer_choices if secs == 86400).set_active(True)
        dialog.timer_save()
        pump(lambda: any(c[0] == "PATCH" for c in self.api.calls), timeout=3)
        patch = next(c for c in self.api.calls if c[0] == "PATCH")
        self.assertEqual((patch[1], patch[3]), ("/v1/threads/release", {"ephemeral_secs": 86400}))
        pump(lambda: w.timer_badge.get_visible(), timeout=3)
        self.assertEqual(w.timer_badge.get_text(), "⏱ 1 day")
        # A message past its expiry leaves by this device's clock, event or not.
        w.messages.append({"message_id": "gone", "peer_handle": "/alex/main", "body": "brief secret", "received_at": NOW - 5,
                           "expires_at": NOW - 1, "read": True, "thread_id": "release"})
        w.tick()
        self.assertNotIn("brief secret", [m.get("body") for m in w.messages])

    def test_pure_rules(self):
        self.assertEqual(wildcard_for("/acme/bob"), "/acme/*")
        self.assertIsNone(wildcard_for("/k/abc"))
        self.assertIsNone(wildcard_for("/github/bob"))
        self.assertTrue(is_blocked("/acme/bob", [{"peer": "/acme/*", "admission": "block"}]))
        self.assertFalse(is_blocked("/acme/bob", [{"peer": "/acme/*", "admission": "allow"}]))
        self.assertEqual(call_detail({"direction": "outgoing", "outcome": "answered", "kind": "video", "duration": 3725}),
                         "↗ Outgoing · Video · 1:02:05")
        self.assertEqual(len(missed_call_lines([{"id": 1, "direction": "incoming", "outcome": "missed", "peer": "/k/x", "started_at": 1}], {"/k/x"})), 1)
        self.assertEqual(square_crop(100, 50, 1, 0, 0), (25, 0, 50))
        self.assertEqual(parse_invite_link("https://inbox.pigeonpost.dev/g/grp_abc#" + "t" * 20), ("grp_abc", "t" * 20))
        self.assertIsNone(parse_invite_link("https://evil.example/g/grp_abc"))


@unittest.skipIf(gc is None, "group crypto library not built (apps/linux/build-native.sh)")
class GroupWindowTests(WindowCase):
    def make_api(self):
        self.fake = FakePostbox(gc)
        alice = self.fake.mailbox()
        api = FeatureAPI(ALL, self.fake)
        api.me_address = alice
        api.identities = lambda: [{"address": alice, "handle": "/demo/main"}]

        def list_groups(me):
            out = []
            for g in self.fake.groups.values():
                mem = g["members"].get(me)
                if not mem or mem["state"] not in ("active", "invited", "keying"):
                    continue
                last = max([i["seq"] for i in g["items"]] or [0])
                out.append({"id": g["id"], "name": g["name"], "join_policy": "private", "post_policy": g["post_policy"],
                            "ephemeral_secs": g["ephemeral"] or None, "revision": g["revision"], "epoch": g["epoch"],
                            "activation_state": g["activation_state"], "member_count": len([m for m in g["members"].values() if m["state"] == "active"]),
                            "created_by": g["creator"], "last_seq": last, "last_at": NOW, "deleted_at": None,
                            "my": {"role": mem["role"], "state": mem["state"], "read_cursor": 0, "unread": 0, "notify": "all", "muted_until": None}})
            return out
        self.fake.list_groups = list_groups
        original = self.fake.handle

        def handle(me, method, path, body, device, raw):
            if path.startswith("/v1/groups/") and path.endswith("/members") and method == "GET":
                gid = urllib.parse.unquote(path.split("/")[3])
                return {"members": [{"address": a, "handle": None, "role": m["role"], "state": m["state"], "revision": 1}
                                    for a, m in self.fake.groups[gid]["members"].items()]}
            if path.startswith("/v1/mls/devices?mailbox="):
                who = urllib.parse.unquote(path.split("=", 1)[1])
                return {"devices": [{"device_id": d, "fingerprint": "ab" * 32, "fingerprint_digits": "12345 67890"}
                                    for d, v in self.fake.devices[who].items() if not v["revoked"]]}
            if path.endswith("/invitation") and method == "POST":
                gid = urllib.parse.unquote(path.split("/")[3])
                if body.get("accept"):
                    self.fake.accept(gid, me)
                else:
                    self.fake.groups[gid]["members"].pop(me, None)
                return {"ok": True}
            if path.endswith("/read") or path.endswith("/notifications"):
                return {"ok": True}
            return original(me, method, path, body, device, raw)
        self.fake.handle = handle
        return api

    def bob(self):
        address = self.fake.mailbox()
        client = GroupClient(gc, self.fake.client_api(address), address, MemoryStorage(), sleep=lambda s: None, jitter=lambda: 0)
        client.init()
        client.top_up_key_packages()
        return address, client

    def cycle(self, until=lambda: True, timeout=8):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if not self.window.group_busy:
                self.window.groups_cycle(force=True)
            pump(lambda: not self.window.group_busy, timeout=3)
            if until():
                return True
        return until()

    def test_create_join_chat_device_notice_safety_number_and_announcements(self):
        w = self.window
        pump(lambda: w.group_state == "ready", timeout=10)
        self.assertTrue(w.new_group_button.get_visible())
        B, bob = self.bob()
        dialog = w.new_group()
        form = dialog.new_group
        form["who"].set_text(B)
        form["add"]()
        self.assertEqual(form["chips"], [B])
        form["name"].set_text("Garden")
        form["submit"]()
        pump(lambda: w.group_open and w.page() == "group", timeout=10)
        gid = w.group_open
        self.assertEqual(self.fake.groups[gid]["activation_state"], "active")
        # The invitation is the server's job here: Bob accepts; Alice's loop adds his device.
        self.fake.invite(gid, B)
        self.fake.accept(gid, B)
        self.assertTrue(self.cycle(lambda: self.fake.groups[gid]["members"][B]["state"] == "active"))
        self.assertEqual(bob.process_welcomes(), [gid])
        bob.send(gid, {"text": "hello from bob"})
        self.assertTrue(self.cycle(lambda: "hello from bob" in texts(w.group_messages)))
        self.assertIn(f"{w.name_of(B)} joined", texts(w.group_messages))
        # Alice writes from the composer; Bob decrypts it.
        w.group_composer.get_buffer().set_text("hi bob")
        w.group_send()
        pump(lambda: not w.group_sending, timeout=10)
        bob.sync(gid)
        self.assertIn("hi bob", [x.get("text") for x in bob.history(gid)])
        stored = [i for i in self.fake.groups[gid]["items"] if i["type"] == "application"]
        self.assertTrue(all(b"hi bob" not in __import__("base64").b64decode(i["ciphertext"]) for i in stored))
        # Bob adds a second device: Alice's view says so, with its fingerprint.
        bob2 = GroupClient(gc, self.fake.client_api(B), B, MemoryStorage(), sleep=lambda s: None, jitter=lambda: 0)
        bob2.init()
        bob2.top_up_key_packages()
        self.assertTrue(self.cycle(lambda: any("added a new device" in t for t in texts(w.group_messages))))
        line = next(t for t in texts(w.group_messages) if "added a new device" in t)
        self.shot("group")
        self.assertEqual(len(line.split(" · ")[1].split()), 6, "fingerprint shown as six groups of five digits")
        # Group info: the safety number every member computes the same way.
        info = w.group_info()
        pump(lambda: B in [getattr(r, "member", {}).get("address") for r in walk(info.group_content)], timeout=5)
        bob.sync(gid)
        self.assertEqual(info.safety_label.get_text().replace(" ", ""), bob.safety_number(gid).replace(" ", ""))
        self.assertIn("This device: " + w.group_client().fingerprint(), texts(info.group_content))
        self.shot("group-info")
        info.close()
        # Announcement mode: a member's composer is replaced by the notice.
        self.fake.groups[gid]["members"][w.identity]["role"] = "member"
        self.fake.groups[gid]["post_policy"] = "admins"
        self.assertTrue(self.cycle(lambda: w.group_announce.get_visible()))
        self.assertEqual(w.group_announce.get_text(), "Only admins can send messages")
        self.assertFalse(w.group_composer_box.get_visible())

    def test_invitation_row_accepts(self):
        w = self.window
        pump(lambda: w.group_state == "ready", timeout=10)
        B, bob = self.bob()
        gid = bob.create_group({"name": "Book club"})["group"]["id"]
        self.fake.invite(gid, w.identity)
        self.assertTrue(self.cycle(lambda: "Invitations (1)" in texts(w.peer_list)))
        row = w.peer_list.get_first_child()
        self.assertEqual(row.kind, "invitations")
        w.peer_list.select_row(row)
        pump(lambda: any(getattr(r, "kind", None) == "invitation" for r in walk(w.peer_list)), timeout=3)
        join = find_button(w.peer_list, "Join")
        join.emit("clicked")
        pump(lambda: self.fake.groups[gid]["members"][w.identity]["state"] == "keying", timeout=3)
        self.assertEqual(self.fake.groups[gid]["members"][w.identity]["state"], "keying")
        bob.process_pending(gid, no_jitter=True)
        self.assertTrue(self.cycle(lambda: w.group_client().in_group(gid)))
        bob.send(gid, {"text": "welcome"})
        self.assertTrue(self.cycle(lambda: any("welcome" in t for t in texts(w.peer_list))))


if __name__ == "__main__":
    unittest.main()
