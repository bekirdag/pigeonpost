"""Message actions in the real GTK window (fake transport): action rows, ticks, reply and edit
modes, message info, unconfirmed-delivery review, read receipts and login binding."""
import copy
import json
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

from pigeonpost.api import APIError
from pigeonpost.binding import Lease
from pigeonpost.protected import MemoryProtectedStore
from pigeonpost.theme import Preferences
from pigeonpost.ui import Adw, Gdk, GLib, Gtk, Window
from test_ui import FakeAPI, pump
from test_ui_features import find_button, texts, walk

ACTIONS = frozenset({"message_edit", "message_reply", "receipts", "agent_status", "message_filtering"})
NOW = int(time.time())
REF_A = "mref_" + "a" * 26
REF_B = "mref_" + "b" * 26
REF_C = "mref_" + "c" * 26


class ActionsAPI(FakeAPI):
    def __init__(self):
        super().__init__()
        self.features = ACTIONS
        self.inbox_queries = []
        self.posted = []
        self.fail_next = None
        self.policy = {"read_receipts": True, "unknown_messages": "show"}
        self.rows = [
            {"message_id": "in1", "msg_ref": REF_A, "peer_handle": "/alex/main", "body": "Can you check the layout?",
             "received_at": NOW - 600, "read": True, "thread_id": "release"},
            {"message_id": "out1", "msg_ref": REF_B, "peer_handle": "/alex/main", "direction": "out", "body": "On it.",
             "sent_at": NOW - 300, "thread_id": "release", "receipt": {"state": "seen", "seen_at": NOW - 200},
             "reply_to": {"msg_ref": REF_A, "message_id": "in1", "direction": "in", "excerpt": "Can you check the layout?"},
             "edited_at": NOW - 250, "edit_count": 1},
            {"message_id": "out2", "msg_ref": REF_C, "peer_handle": "/alex/main", "direction": "out", "body": "Agent task",
             "sent_at": NOW - 100, "thread_id": "release", "receipt": {"state": "delivered"}, "agent_status": {"state": "working"}},
            {"message_id": "old", "peer_handle": "/alex/main", "direction": "out", "body": "Yesterday",
             "sent_at": NOW - 3 * 86400, "thread_id": "release", "receipt": {"state": "delivered"}},
        ]

    def capabilities(self):
        return self.features

    def inbox(self, identity, wait=0, actions=False):
        self.inbox_queries.append(actions)
        return copy.deepcopy(self.rows) if identity == "/k/one" else []

    def post_exact(self, path, data):
        body = json.loads(data)
        self.posted.append((path, body, bytes(data)))
        if self.fail_next is not None:
            error, self.fail_next = self.fail_next, None
            raise error
        self.rows.append({"message_id": "m" + str(len(self.posted)), "msg_ref": body["msg_ref"], "peer_handle": body["to"],
                          "direction": "out", "body": body["body"], "sent_at": NOW, "thread_id": body.get("thread_id"),
                          "receipt": {"state": "sent"}})
        return {"message_id": "x", "sent_copy_id": "m" + str(len(self.posted)), "msg_ref": body["msg_ref"]}

    def call(self, method, path, identity=None, data=None, **kwargs):
        if path == "/v1/policy":
            self.calls.append((method, path, identity, data))
            if method == "PUT":
                self.policy.update(data)
            return dict(self.policy)
        if path.startswith("/v1/messages/") and path.endswith("/info"):
            self.calls.append((method, path, identity, data))
            return {"message_id": "out1", "msg_ref": REF_B, "direction": "out", "sent_at": NOW - 300,
                    "versions": [{"message_id": "out1", "body": "On it", "at": NOW - 300},
                                 {"message_id": "e1", "body": "On it.", "at": NOW - 250}],
                    "receipt": {"state": "seen", "delivered_at": NOW - 280, "seen_at": NOW - 200}}
        if path == "/v1/calls":
            return {"entries": [], "unseen": 0, "next": None}
        if path in ("/v1/groups", "/v1/filtered"):
            return {"groups": [], "invitations": [], "messages": []}
        return super().call(method, path, identity, data, **kwargs)


class MessageActionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = Adw.Application(application_id="dev.pigeonpost.MessageActionTests")
        cls.app.register(None)

    def setUp(self):
        self.errors = []
        self.hook = sys.excepthook
        sys.excepthook = lambda kind, error, trace: self.errors.append(error)
        self.api = ActionsAPI()
        self.config = tempfile.TemporaryDirectory(prefix="pp-prefs-")
        self.store = MemoryProtectedStore()
        self.window = Window(self.app, api=self.api, restore=False, preferences=Preferences(Path(self.config.name) / "p.json"),
                             protected=self.store)
        self.window.present()
        self.window.account_loaded(self.api.identities())
        pump(lambda: self.window.known_ids is not None and self.window.features)
        self.window.refresh()
        pump(lambda: not self.window.polling, timeout=1)
        self.select()

    def tearDown(self):
        self.window.close()
        pump(timeout=0.05)
        sys.excepthook = self.hook
        self.config.cleanup()
        self.assertEqual(self.errors, [], "GTK callbacks raised exceptions")

    def select(self, peer="/alex/main"):
        row = self.window.peer_list.get_first_child()
        while row and getattr(row, "peer", None) != peer:
            row = row.get_next_sibling()
        self.window.peer_list.select_row(row)
        pump(timeout=0.05)

    def holder(self, message_id):
        return next(w for w in walk(self.window.message_list) if getattr(w, "message", {}).get("message_id") == message_id)

    def buffer_text(self):
        buffer = self.window.composer.get_buffer()
        return buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)

    def test_reads_folded_view_and_renders_actions_ticks_and_quotes(self):
        self.assertIn(True, self.api.inbox_queries, "actions=1&fold=1 once message actions are on")
        received, sent, working, old = (self.holder(m) for m in ("in1", "out1", "out2", "old"))
        self.assertIsNotNone(find_button(received, "Reply"))
        self.assertIsNotNone(find_button(received, "Copy message"))
        self.assertIsNone(find_button(received, "Edit message"))
        self.assertIsNotNone(find_button(sent, "Edit message"))
        self.assertIsNone(find_button(old, "Edit message"), "edit only within 24 hours")
        actions = next(w for w in walk(sent) if w.has_css_class("pp-actions"))
        self.assertIsNot(actions.get_parent(), next(w for w in walk(sent) if w.has_css_class("pp-bubble")),
                         "the action row is below and outside the bubble")
        ticks = {w.tick_state: w.get_tooltip_text() for w in walk(self.window.message_list) if hasattr(w, "tick_state")}
        self.assertTrue(ticks["seen"].startswith("Seen "))
        self.assertEqual(ticks["working"], "Agent working on this")
        self.assertEqual(ticks["delivered"], "Delivered")
        self.assertFalse(any(hasattr(w, "tick_state") for w in walk(received)), "no ticks on received messages")
        self.assertIn("edited", texts(sent))
        self.assertIn("Can you check the layout?", texts(next(w for w in walk(sent) if w.has_css_class("pp-quote"))))
        body = next(w for w in walk(received) if isinstance(w, Gtk.Label) and w.has_css_class("pp-body"))
        model = body.get_extra_menu()
        labels = [model.get_item_attribute_value(i, "label").unpack() for i in range(model.get_n_items())]
        self.assertEqual(labels, ["Reply", "Copy message", "Message info", "Delete message", "Report spam", "Block sender"])

    def test_quote_jumps_to_parent_and_highlights_it(self):
        quote = next(w for w in walk(self.holder("out1")) if isinstance(w, Gtk.Button) and w.has_css_class("flat")
                     and any(c.has_css_class("pp-quote") for c in walk(w)))
        quote.emit("clicked")
        pump(timeout=0.05)
        self.assertTrue(self.holder("in1").has_css_class("pp-highlight"))

    def test_reply_mode_sends_reference_and_quote(self):
        find_button(self.holder("in1"), "Reply").emit("clicked")
        pump(timeout=0.05)
        self.assertTrue(self.window.mode_holder.get_visible())
        self.assertIn("Replying to Alex", texts(self.window.mode_holder))
        self.window.composer.get_buffer().set_text("Looks good")
        self.window.send()
        pump(lambda: self.api.posted and not self.window.sending)
        path, body, _ = self.api.posted[0]
        self.assertEqual(body["reply_to"], REF_A)
        self.assertEqual(body["quote"], "Can you check the layout?")
        self.assertEqual(body["thread_id"], "release")
        pump(lambda: not self.window.mode_holder.get_visible())
        self.assertEqual(self.buffer_text(), "")

    def test_edit_mode_restores_draft_on_escape_and_saves_edit(self):
        self.window.composer.get_buffer().set_text("My unsent draft")
        find_button(self.holder("out1"), "Edit message").emit("clicked")
        pump(timeout=0.05)
        self.assertEqual(self.buffer_text(), "On it.")
        self.assertIn("Editing message", texts(self.window.mode_holder))
        self.assertFalse(self.window.attach_button.get_sensitive(), "edits are text only")
        self.assertTrue(self.window.composer_keys(None, Gdk.KEY_Escape, 0, 0))
        self.assertEqual(self.buffer_text(), "My unsent draft")
        find_button(self.holder("out1"), "Edit message").emit("clicked")
        self.window.composer.get_buffer().set_text("On it — done.")
        self.window.send()
        pump(lambda: self.api.posted and not self.window.sending)
        body = self.api.posted[0][1]
        self.assertEqual(body["edit_of"], REF_B)
        self.assertNotIn("thread_id", body)
        pump(lambda: self.buffer_text() == "My unsent draft")
        self.assertEqual(self.buffer_text(), "My unsent draft", "the saved draft comes back after the edit")

    def test_message_info_lists_versions_and_receipts(self):
        window = self.window.message_info(self.api.rows[1])
        pump(lambda: hasattr(window, "info"))
        shown = texts(window.get_content())
        self.assertIn("Versions", shown)
        self.assertIn("On it", shown)
        self.assertTrue(any(t.startswith("Seen ") for t in shown))
        window.close()

    def test_uncertain_send_is_kept_for_explicit_retry_and_blocks_the_conversation(self):
        self.api.fail_next = APIError(0, "network_error")
        self.window.composer.get_buffer().set_text("Maybe delivered")
        self.window.send()
        pump(lambda: self.window.pending.get("/k/one"))
        self.assertEqual(self.buffer_text(), "Maybe delivered", "the draft stays")
        pending = [w for w in walk(self.window.message_list) if hasattr(w, "attempt")]
        self.assertEqual(len(pending), 1)
        self.assertTrue(self.window.recovery_button.get_visible())
        for dialog in list(self.window.dialogs):
            dialog.close()
        # Another send here waits for the review instead of racing it.
        self.window.composer.get_buffer().set_text("Maybe delivered")
        self.window.send()
        pump(lambda: not self.window.sending)
        self.assertEqual(len(self.api.posted), 1)
        find_button(pending[0], "Retry sending").emit("clicked")
        pump(lambda: not self.window.pending.get("/k/one"))
        self.assertEqual(self.api.posted[1][2], self.api.posted[0][2], "the retry posts the exact saved bytes")
        pump(lambda: self.buffer_text() == "")
        self.assertEqual(self.buffer_text(), "", "a confirmed retry clears the matching draft")
        self.assertFalse(self.window.recovery_button.get_visible())

    def test_saved_work_survives_a_new_window_for_the_same_login(self):
        self.api.fail_next = APIError(503, "unavailable")
        self.window.composer.get_buffer().set_text("Kept across restart")
        self.window.send()
        pump(lambda: self.window.pending.get("/k/one"))
        pump(lambda: self.store.rows, timeout=2)
        second = Window(self.app, api=self.api, restore=False, protected=self.store)
        second.memory_work = self.window.memory_work
        second.account_loaded(self.api.identities())
        pump(lambda: second.pending.get("/k/one"))
        self.assertEqual([a.body for a in second.pending["/k/one"]], ["Kept across restart"])
        dialog = second.review_deliveries()
        self.assertIn("Kept across restart", texts(dialog.get_content()))
        dialog.close()
        second.close()

    def test_results_of_a_retired_login_are_dropped(self):
        gate = threading.Event()
        original = self.api.post_exact

        def slow(path, data):
            gate.wait(2)
            return original(path, data)
        self.api.post_exact = slow
        self.window.composer.get_buffer().set_text("From the old login")
        self.window.send()
        pump(timeout=0.1)
        lease = self.window.mailbox_lease
        self.window.clear_account()  # sign-out: the lease is retired before the answer arrives
        self.assertFalse(lease.current)
        gate.set()
        pump(timeout=0.3)
        self.assertEqual(self.window.drafts, {})
        self.assertEqual(self.window.pending, {})

    def test_read_receipts_setting(self):
        from pigeonpost.widgets import SettingsNavigation
        navigation = SettingsNavigation(self.window)
        self.window.privacy_settings(navigation)
        pump(lambda: any(isinstance(w, Gtk.Switch) and w.get_sensitive() for w in walk(navigation.privacy_rows)))
        switch = next(w for w in walk(navigation.privacy_rows) if isinstance(w, Gtk.Switch))
        self.assertTrue(switch.get_active())
        switch.set_active(False)
        pump(lambda: ("PUT", "/v1/policy", "/k/one", {"read_receipts": False}) in self.api.calls)
        navigation.window.close()

    def test_ack_includes_edits(self):
        self.api.rows[0]["read"] = False
        self.window.messages[0]["read"] = False
        self.window.acking = False
        self.window.ack_visible()
        pump(lambda: any(c[1] == "/v1/ack" for c in self.api.calls))
        ack = next(c for c in self.api.calls if c[1] == "/v1/ack")
        self.assertEqual(ack[3], {"message_id": "in1", "with_edits": True})


if __name__ == "__main__":
    unittest.main()
