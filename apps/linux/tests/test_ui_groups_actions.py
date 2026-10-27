"""Group message actions in the real GTK window (docs/message-actions.md §5–§6): the action row
outside the bubble, reply with a quoted parent and jump, inline edit with Esc restoring the draft,
versions in message info, ticks from member receipts, an unconfirmed send kept for an explicit
retry, and deletion of a whole version chain. Real MLS library against the in-memory postbox
(tests/fake_groups_postbox.py). Run under Xvfb."""
import os
import time
import unittest

from pigeonpost import groupcrypto
from pigeonpost.groups_core import GroupError
from pigeonpost.ui import Gdk, Gtk
from test_ui import pump
import test_ui_features as feature_tests
from test_ui_features import ALL, WindowCase, find_button, texts, walk

gc = groupcrypto.load()
if gc is None and os.environ.get("PIGEONPOST_REQUIRE_GROUPCRYPTO") == "1":
    raise RuntimeError("group crypto library missing: " + str(groupcrypto.reason()))

ACTIONS = ALL | {"message_edit", "message_reply", "receipts"}


@unittest.skipIf(gc is None, "group crypto library not built (apps/linux/build-native.sh)")
class GroupActionWindowTests(WindowCase):
    features = ACTIONS
    make_api_base = feature_tests.GroupWindowTests.make_api
    bob = feature_tests.GroupWindowTests.bob
    cycle = feature_tests.GroupWindowTests.cycle

    def make_api(self):
        api = self.make_api_base()
        api.features = ACTIONS
        return api

    def bubbles(self):
        return [w for w in walk(self.window.group_messages) if hasattr(w, "item") and hasattr(w, "actions")]

    def bubble(self, text):
        for w in self.bubbles():
            if w.item.get("text") == text:
                return w
        return None

    def row_buttons(self, bubble):
        row = bubble.action_row
        return [w.get_tooltip_text() for w in walk(row) if isinstance(w, Gtk.Button) and w.get_tooltip_text()] if row else []

    def setUpGroup(self):
        w = self.window
        pump(lambda: w.group_state == "ready", timeout=10)
        B, bob = self.bob()
        gid = w.group_client().create_group({"name": "Actions"})["group"]["id"]
        self.fake.invite(gid, B)
        self.fake.accept(gid, B)
        self.assertTrue(self.cycle(lambda: self.fake.groups[gid]["members"][B]["state"] == "active"))
        self.assertEqual(bob.process_welcomes(), [gid])
        w.open_group(gid)
        return B, bob, gid

    def test_reply_edit_info_ticks_retry_and_delete(self):
        w = self.window
        B, bob, gid = self.setUpGroup()
        bob.send(gid, {"text": "Which colour?"})
        self.assertTrue(self.cycle(lambda: self.bubble("Which colour?") is not None))
        received = self.bubble("Which colour?")
        self.assertEqual(self.row_buttons(received)[:2], ["Copy message", "Reply"], "received: copy, reply")
        self.assertNotIn("Edit message", self.row_buttons(received))
        self.assertEqual(received.action_row.get_halign(), Gtk.Align.START)

        # Reply: the mode bar, then the quoted parent on the sent bubble, which jumps back.
        find_button(received.action_row, "Reply").emit("clicked")
        self.assertIn("Replying to " + w.name_of(B), texts(w.group_mode_holder))
        w.group_composer.get_buffer().set_text("Blue")
        w.group_send()
        pump(lambda: not w.group_sending and self.bubble("Blue") is not None, timeout=10)
        self.assertEqual(texts(w.group_mode_holder), [])
        bob.sync(gid)
        parent = next(x["seq"] for x in bob.history(gid) if x.get("text") == "Which colour?")
        self.assertEqual(next(x for x in bob.history(gid) if x.get("text") == "Blue")["reply_to"], str(parent))
        mine = self.bubble("Blue")
        self.assertIn("Which colour?", texts(mine))
        self.assertEqual(mine.action_row.get_halign(), Gtk.Align.END)
        self.assertEqual(self.row_buttons(mine)[:2], ["Edit message", "Copy message"], "sent: edit, copy")
        quote = next(b for b in walk(mine) if isinstance(b, Gtk.Button) and (b.get_tooltip_text() or "") == "Show the original message")
        quote.emit("clicked")
        self.assertTrue(self.bubble("Which colour?").bubble.has_css_class("pp-highlight"), "the parent is highlighted")

        # Edit inline; Esc gives the ordinary draft back; saving folds the versions.
        w.group_composer.get_buffer().set_text("keep me")
        find_button(mine.action_row, "Edit message").emit("clicked")
        buffer = w.group_composer.get_buffer()
        self.assertEqual(buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False), "Blue")
        self.assertIn("Editing message", texts(w.group_mode_holder))
        w.group_composer_keys(None, Gdk.KEY_Escape, 0, 0)
        self.assertEqual(buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False), "keep me")
        self.assertIsNone(w.group_mode)
        find_button(self.bubble("Blue").action_row, "Edit message").emit("clicked")
        buffer.set_text("Dark blue")
        w.group_send()
        pump(lambda: not w.group_sending and self.bubble("Dark blue") is not None, timeout=10)
        self.assertIsNone(self.bubble("Blue"), "the old version is folded")
        self.assertIn("edited", texts(self.bubble("Dark blue")))
        self.assertEqual(buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False), "keep me")
        bob.sync(gid)
        self.assertIn("Dark blue", [x.get("text") for x in bob.history(gid) if not x.get("replaced_by")])
        self.shot("group-actions")

        # Message info lists the versions this device holds.
        info = self.bubble("Dark blue").actions["info"]()
        self.assertIn("Blue", texts(info.info_content))
        self.assertIn("Dark blue", texts(info.info_content))
        info.close()

        # Ticks: delivered once Bob listed it, seen (green) once he read it.
        latest = self.bubble("Dark blue").item["seq"]
        bob.sync(gid)
        w.load_group_receipts(gid, force=True)
        pump(lambda: self.tick_of("Dark blue") == "delivered", timeout=5)
        self.assertEqual(self.tick_of("Dark blue"), "delivered")
        self.fake.seen[(gid, B)] = latest
        w.load_group_receipts(gid, force=True)
        pump(lambda: self.tick_of("Dark blue") == "seen", timeout=5)
        self.assertEqual(self.tick_of("Dark blue"), "seen")

        # A lost answer: the bubble says so and offers Retry, which resends the same request once.
        original = self.fake.handle
        armed = [True]

        def lossy(me, method, path, body, device, raw):
            out = original(me, method, path, body, device, raw)
            if armed[0] and method == "POST" and path.endswith("/messages"):
                armed[0] = False
                raise GroupError(502, "http_502")
            return out
        self.fake.handle = lossy
        buffer.set_text("Did this arrive?")
        w.group_send()
        pump(lambda: not w.group_sending and self.bubble("Did this arrive?") is not None
             and "Not confirmed" in texts(self.bubble("Did this arrive?")), timeout=10)
        pending = self.bubble("Did this arrive?")
        self.assertIn("Retry sending", self.row_buttons(pending))
        self.assertEqual(buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False), "",
                         "the unconfirmed text stays in its bubble, not back in the composer")
        find_button(pending.action_row, "Retry sending").emit("clicked")
        pump(lambda: self.bubble("Did this arrive?") is not None and "Not confirmed" not in texts(self.bubble("Did this arrive?")), timeout=10)
        posts = [i for i in self.fake.groups[gid]["items"] if i["type"] == "application" and i.get("sender") == w.identity]
        self.assertEqual(len({i["client_message_id"] for i in posts}), len(posts), "no duplicate post")
        self.assertEqual(w.group_client().outbox(gid), [])

        # Delete any version: every version goes, here and (after reconciliation) for Bob.
        w.confirm = lambda title, body, action, caption="Delete": action()
        self.bubble("Dark blue").actions["delete"]()
        pump(lambda: self.bubble("Dark blue") is None, timeout=10)
        self.assertFalse(any(x.get("text") in ("Blue", "Dark blue") for x in w.group_client().history(gid)))
        bob.reconcile(gid)
        self.assertFalse(any(x.get("text") in ("Blue", "Dark blue") for x in bob.history(gid)))

    def tick_of(self, text):
        bubble = self.bubble(text)
        if bubble is None:
            return None
        return next((getattr(wd, "tick_state", None) for wd in walk(bubble) if hasattr(wd, "tick_state")), None)

    def test_actions_stay_hidden_without_the_features(self):
        w = self.window
        B, bob, gid = self.setUpGroup()
        bob.send(gid, {"text": "plain"})
        self.assertTrue(self.cycle(lambda: self.bubble("plain") is not None))
        w.features = ALL
        w.render_group_view()
        received = self.bubble("plain")
        self.assertEqual(self.row_buttons(received)[:1], ["Copy message"])
        self.assertNotIn("Reply", self.row_buttons(received))
        self.assertNotIn("reply", received.actions)
        w.group_composer.get_buffer().set_text("mine")
        w.group_send()
        pump(lambda: not w.group_sending and self.bubble("mine") is not None, timeout=10)
        self.assertNotIn("Edit message", self.row_buttons(self.bubble("mine")))
        self.assertEqual(self.tick_of("mine"), "sent", "a single tick without receipts")


if __name__ == "__main__":
    unittest.main()
