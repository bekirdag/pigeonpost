"""Real GTK widgets; fake transport only. Not installed in release packages."""
import copy
import os
import subprocess
import sys
import threading
import time
import unittest

from pigeonpost.ui import Adw, GLib, Gtk, Window


def pump(until=lambda: False, timeout=2):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        while GLib.MainContext.default().pending():
            GLib.MainContext.default().iteration(False)
        if until():
            return
        time.sleep(0.01)


class FakeAPI:
    def __init__(self):
        self.calls = []
        self.rows = [
            {"message_id": "a", "peer_handle": "/alex/main", "body": "The Linux build is ready for a review.\nCan you check the new inbox layout?", "received_at": 1789320000, "read": True, "thread_id": "release"},
            {"message_id": "b", "peer_handle": "/alex/main", "body": "I’ll check the layout and attachments next.", "direction": "out", "sent_at": 1789320060, "thread_id": "release"},
            {"message_id": "c", "peer_handle": "/studio/reviewer", "body": "The test results are ready.", "received_at": 1789319000, "read": False},
        ]
    def identities(self):
        return [{"address": "/k/one", "handle": "/demo/main"}, {"address": "/k/two", "handle": "/demo/agent"}]
    def inbox(self, identity, wait=0):
        return copy.deepcopy(self.rows) if identity == "/k/one" else []
    def call(self, method, path, identity=None, data=None, **kwargs):
        self.calls.append((method, path, identity, data))
        if path == "/v1/me/handles":
            return {"handles": [
                {"namespace": "apple-name", "source": "apple", "active": True, "expires_at": 1999999999},
                {"namespace": "google-name", "source": "google", "active": False, "expires_at": 100},
                {"namespace": "web-name", "source": "entitlement", "active": True},
            ]}
        if path == "/v1/threads":
            return {"threads": [{"thread_id": "release", "peer": "/alex/main", "title": "Linux release"}, {"thread_id": "design", "peer": "/alex/main", "title": "Design notes"}]}
        if path == "/v1/contacts":
            return {"contacts": [{"peer": "/alex/main", "alias": "Alex", "autonomy": "review"}], "vocabulary": {"grantable": ["summarize", "answer_question"], "never_auto": ["run_shell"]}}
        if path == "/v1/archive": return {"archived": []}
        if path == "/v1/quota": return {"used_bytes": 1024 * 180, "limit_bytes": 1024 * 1024 * 100, "warn_at_bytes": 1024 * 1024 * 90}
        if path == "/v1/send":
            self.rows.append({"message_id": "sent", "peer_handle": data["to"], "direction": "out", "sent_at": 1789320200, "body": data["body"], "thread_id": data.get("thread_id")})
            return {"sent_copy_id": "sent"}
        return {}


class NativeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = Adw.Application(application_id="dev.pigeonpost.NativeTests")
        cls.app.register(None)

    def setUp(self):
        self.callback_errors = []
        self.previous_hook = sys.excepthook
        sys.excepthook = lambda kind, error, trace: self.callback_errors.append(error)
        self.api = FakeAPI()
        self.window = Window(self.app, api=self.api, restore=False)
        self.window.present()
        self.window.account_loaded(self.api.identities())
        pump(lambda: self.window.known_ids is not None)

    def tearDown(self):
        self.window.close()
        pump(timeout=0.05)
        sys.excepthook = self.previous_hook
        self.assertEqual(self.callback_errors, [], "GTK callbacks raised exceptions")

    def select(self, peer="/alex/main"):
        window = self.window
        row = window.peer_list.get_first_child()
        while row and row.peer != peer:
            row = row.get_next_sibling()
        self.assertIsNotNone(row)
        window.peer_list.select_row(row)
        pump(timeout=0.05)

    def test_widgets_display_real_messages_and_subjects(self):
        self.select()
        self.assertEqual(self.window.peer_title.get_text(), "Alex")
        self.assertIsNotNone(self.window.message_list.get_first_child())
        self.assertEqual(self.window.subject_list.get_first_child().subject, None)

    def test_drafts_are_isolated_by_mailbox_peer_and_subject(self):
        self.select()
        self.window.composer.get_buffer().set_text("Draft for Alex")
        self.select("/studio/reviewer")
        self.assertEqual(self.window.drafts[("/k/one", "/alex/main", None)], "Draft for Alex")
        self.assertFalse(self.window.send_button.get_sensitive())
        self.select()
        self.assertTrue(self.window.send_button.get_sensitive())

    def test_send_uses_selected_subject_and_clears_confirmed_draft(self):
        self.select()
        self.window.subject_list.select_row(self.window.subject_list.get_row_at_index(1))
        self.window.composer.get_buffer().set_text("Ready to test")
        self.window.send()
        pump(lambda: any(c[1] == "/v1/send" for c in self.api.calls) and not self.window.sending)
        sent = next(c[3] for c in self.api.calls if c[1] == "/v1/send")
        self.assertEqual(sent["thread_id"], "release")
        self.assertEqual(sent["from"], "/k/one")
        self.assertNotIn(self.window.draft_key(), self.window.drafts)

    def test_mailbox_switch_does_not_show_previous_messages(self):
        self.select()
        self.window.mailbox_picker.set_selected(1)
        self.assertIsNone(self.window.peer)
        self.assertEqual(self.window.messages, [])
        pump(lambda: self.window.known_ids is not None)
        self.assertEqual(self.window.identity, "/k/two")
        self.assertEqual(self.window.messages, [])

    def test_late_worker_callback_is_discarded(self):
        gate = threading.Event()
        result = []
        self.window._work(lambda: (gate.wait(1), "old")[1], result.append)
        self.window.generation += 1
        gate.set()
        pump(timeout=0.15)
        self.assertEqual(result, [])

    def test_signout_wipes_drafts_and_closes_account_dialogs(self):
        self.select()
        self.window.composer.get_buffer().set_text("Private draft")
        self.window.settings()
        self.window.clear_account()
        self.assertEqual(self.window.drafts, {})
        self.assertEqual(self.window.messages, [])
        self.assertEqual(self.window.dialogs, [])

    def test_native_account_pages_construct(self):
        self.select()
        for show in (self.window.settings, self.window.handles, self.window.contact_list, self.window.archives,
                     lambda: self.window.contact_editor("/alex/main")):
            show()
            pump(timeout=0.05)
            self.window.dialogs[-1].close()

    def test_handles_show_account_ownership_from_every_provider(self):
        self.window.handles()
        dialog = self.window.dialogs[-1]
        def texts(widget):
            found = [widget.get_text()] if isinstance(widget, Gtk.Label) else []
            child = widget.get_first_child()
            while child:
                found.extend(texts(child))
                child = child.get_next_sibling()
            return found
        pump(lambda: "/google-name" in texts(dialog))
        content = "\n".join(texts(dialog))
        for expected in ["/apple-name", "Active · App Store", "/google-name", "Expired · Google Play", "/web-name", "Active · Pigeonpost"]:
            self.assertIn(expected, content)
        self.assertIsNone(next(c for c in self.api.calls if c[1] == "/v1/me/handles")[2], "ownership never uses the selected mailbox")

    def test_settings_pages_back_and_purchase_separation(self):
        self.window.settings()
        navigation = self.window.dialogs[-1].settings_navigation
        def rows(widget):
            found = [widget] if isinstance(widget, Adw.ActionRow) else []
            child = widget.get_first_child()
            while child:
                found.extend(rows(child))
                child = child.get_next_sibling()
            return found
        root = navigation.stack.get_visible_child()
        self.assertEqual([row.get_title() for row in rows(root)],
                         ["Account", "Handles", "Inbox and storage", "Contacts and permissions", "Help and about"])
        self.assertFalse(navigation.back_button.get_visible())
        rows(root)[1].emit("activated")
        self.assertEqual(navigation.window.get_title(), "Handles")
        rows(navigation.stack.get_visible_child())[0].emit("activated")
        self.assertEqual(navigation.window.get_title(), "Get a handle")
        navigation.back_button.emit("clicked")
        self.assertEqual(navigation.window.get_title(), "Handles")
        navigation.back()
        self.assertIs(navigation.stack.get_visible_child(), root)
        for row in rows(root):
            row.emit("activated")
            self.assertTrue(navigation.back_button.get_visible())
            navigation.back()
        self.assertEqual(len(self.window.dialogs), 1, "Settings pages use one window")
        self.assertFalse(navigation.back_button.get_visible())

    def test_visual_evidence(self):
        if not os.environ.get("PIGEONPOST_SCREENSHOT_DIR"):
            self.skipTest("Screenshot capture requested only in GUI CI")
        directory = os.environ["PIGEONPOST_SCREENSHOT_DIR"]
        os.makedirs(directory, exist_ok=True)
        self.select()
        self.window.subject_list.select_row(self.window.subject_list.get_row_at_index(1))
        for theme in ("light", "dark"):
            Adw.StyleManager.get_default().set_color_scheme(Adw.ColorScheme.FORCE_LIGHT if theme == "light" else Adw.ColorScheme.FORCE_DARK)
            pump(timeout=0.35)
            subprocess.run(["import", "-window", "root", f"{directory}/inbox-{theme}.png"], check=True)
        self.window.settings()
        navigation = self.window.dialogs[-1].settings_navigation
        for title, show in (("settings", lambda: None), ("settings-account", lambda: self.window.account_settings(navigation)),
                            ("settings-handles", lambda: self.window.handles(navigation))):
            show()
            pump(timeout=0.35)
            subprocess.run(["import", "-window", "root", f"{directory}/{title}.png"], check=True)
            if len(navigation.pages) > 1:
                navigation.back()
        navigation.window.close()
        self.window.set_default_size(850, 620)
        pump(timeout=0.35)
        subprocess.run(["import", "-window", "root", f"{directory}/inbox-narrow.png"], check=True)


if __name__ == "__main__":
    unittest.main()
