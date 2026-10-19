"""Real GTK widgets; fake transport only. Not installed in release packages."""
import copy
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

from pigeonpost.theme import Preferences
from pigeonpost.ui import Adw, GLib, Gtk, Window

FIXTURES = Path(__file__).resolve().parents[3] / "tests/fixtures/attachments"


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


def sample_checklist_pdf():
    """A made-up one-page document for screenshots."""
    import io
    import cairo
    output = io.BytesIO()
    surface = cairo.PDFSurface(output, 595, 842)
    context = cairo.Context(surface)
    context.select_font_face("Sans", cairo.FONT_SLANT_NORMAL, cairo.FONT_WEIGHT_BOLD)
    context.set_font_size(26)
    context.move_to(60, 100)
    context.show_text("Linux 1.0.8 release checklist")
    context.select_font_face("Sans")
    context.set_font_size(15)
    for index, line in enumerate(["Unit and GTK tests pass on x86_64 and ARM64", "Debian package installs and starts",
                                   "Flatpak bundle installs and starts", "Screenshots in light and dark",
                                   "Release notes and checksums", "Website downloads updated"]):
        y = 170 + index * 46
        context.set_source_rgb(0.18, 0.42, 0.88)
        context.rectangle(60, y - 15, 18, 18)
        context.fill()
        context.set_source_rgb(0.1, 0.12, 0.15)
        context.move_to(94, y)
        context.show_text(line)
    surface.finish()
    return output.getvalue()


class NativeTests(unittest.TestCase):
    def test_native_previews_render_image_video_and_pdf_pixels(self):
        from pigeonpost.previews import ThumbnailCache, supports
        cache = ThumbnailCache()
        folder = Path(__file__).resolve().parents[3] / "tests/fixtures/attachments"
        for name, mime, channel in [("preview.png", "image/png", 0), ("preview.mp4", "video/mp4", 2), ("preview.pdf", "application/pdf", 1)]:
            pixbuf = cache.load(name, name, mime, lambda: (folder / name).read_bytes())
            self.assertIsNotNone(pixbuf, name)
            self.assertLessEqual(pixbuf.get_width(), 440)
            self.assertLessEqual(pixbuf.get_height(), 300)
            offset = (pixbuf.get_height() // 2) * pixbuf.get_rowstride() + (pixbuf.get_width() // 2) * pixbuf.get_n_channels()
            rgb = pixbuf.get_pixels()[offset:offset + 3]
            self.assertGreater(rgb[channel], min(rgb) + 80, name)
            self.assertIs(cache.load(name, name, mime, lambda: self.fail("Cached preview was downloaded twice")), pixbuf)
        self.assertFalse(supports("image.png", "text/html"))
        self.assertFalse(supports("image.svg", "image/svg+xml"))

    def test_staging_prevents_partial_send_and_preserves_caption(self):
        self.select()
        self.window.composer.get_buffer().set_text("A caption")
        key = self.window.draft_key()
        self.window.stage_started(key)
        self.assertFalse(self.window.send_button.get_sensitive())
        self.window.send()
        self.assertFalse(any(call[1] == "/v1/send" for call in self.api.calls))
        self.window.stage_finished(key)
        self.assertTrue(self.window.send_button.get_sensitive())
        self.assertEqual(self.window.drafts[key], "A caption")

    @classmethod
    def setUpClass(cls):
        cls.app = Adw.Application(application_id="dev.pigeonpost.NativeTests")
        cls.app.register(None)

    def setUp(self):
        self.callback_errors = []
        self.previous_hook = sys.excepthook
        sys.excepthook = lambda kind, error, trace: self.callback_errors.append(error)
        self.api = FakeAPI()
        self.config = tempfile.TemporaryDirectory(prefix="pigeonpost-prefs-")
        self.preferences = Preferences(Path(self.config.name) / "preferences.json")
        self.window = Window(self.app, api=self.api, restore=False, preferences=self.preferences)
        self.window.present()
        self.window.account_loaded(self.api.identities())
        pump(lambda: self.window.known_ids is not None)

    def tearDown(self):
        self.window.close()
        pump(timeout=0.05)
        sys.excepthook = self.previous_hook
        Adw.StyleManager.get_default().set_color_scheme(Adw.ColorScheme.DEFAULT)
        self.config.cleanup()
        self.assertEqual(self.callback_errors, [], "GTK callbacks raised exceptions")

    def select(self, peer="/alex/main"):
        window = self.window
        row = window.peer_list.get_first_child()
        while row and row.peer != peer:
            row = row.get_next_sibling()
        self.assertIsNotNone(row)
        window.peer_list.select_row(row)
        pump(timeout=0.05)

    def test_subject_unread_counts_and_notification_target(self):
        self.api.rows += [
            {"message_id": "new-release", "peer_handle": "/alex/main", "body": "new", "thread_id": "release", "read": False},
            {"message_id": "new-design", "peer_handle": "/alex/main", "body": "new", "thread_id": "design", "read": False},
        ]
        self.window.messages = copy.deepcopy(self.api.rows)
        self.window.peer, self.window.subject = "/alex/main", "release"
        self.window.render_all()
        design = self.window.subject_list.get_first_child()
        while design and design.subject != "design":
            design = design.get_next_sibling()
        self.assertIn("1 unread", design.get_child().get_text())
        self.assertIn("3 unread", self.window.get_title())
        self.window.notification_target = "/k/one", "new-design"
        self.window.apply_notification_target()
        self.assertEqual((self.window.peer, self.window.subject), ("/alex/main", "design"))
        self.assertIsNone(self.window.notification_target)
        self.window.clear_account()
        self.assertEqual(self.window.get_title(), "Pigeonpost")

    def test_widgets_display_real_messages_and_subjects(self):
        self.select()
        self.assertEqual(self.window.peer_title.get_text(), "Alex")
        self.assertIsNotNone(self.window.message_list.get_first_child())
        self.assertEqual(self.window.subject_list.get_first_child().subject, None)

    def test_new_conversation_supplies_prefix_and_keeps_the_full_destination(self):
        self.assertEqual(self.window.mailbox_model.get_string(0), "/demo")
        self.window.new_conversation()
        content = self.window.dialogs[-1].get_content()
        def entries(widget):
            if isinstance(widget, Gtk.Entry):
                yield widget
            child = widget.get_first_child()
            while child:
                yield from entries(child)
                child = child.get_next_sibling()
        entry = next(entries(content))
        self.assertEqual(entry.get_text(), "/")
        entry.set_text("bekir/main")
        pump(timeout=0.05)
        self.assertEqual(entry.get_text(), "/bekir/main")
        entry.set_text("/alp")
        pump(timeout=0.05)
        self.assertEqual(entry.get_text(), "/alp")
        entry.emit("activate")
        pump(timeout=0.05)
        self.assertEqual(self.window.peer, "/alp")

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

    def test_copy_full_address_and_labelled_unnamed_mailbox(self):
        def copy_row(row, expected):
            row.get_last_child().emit("clicked")
            copied = []
            clipboard = self.window.get_clipboard()
            clipboard.read_text_async(None, lambda board, result: copied.append(board.read_text_finish(result)))
            pump(lambda: bool(copied))
            self.assertEqual(copied, [expected])

        copy_row(self.window.post_address.get_first_child(), "/demo/main")
        self.assertEqual(self.window.identity, "/k/one")
        self.window.mailboxes[0].pop("handle")
        self.window.mailboxes[0]["label"] = "My friendly label"
        self.window.select_mailbox()
        copy_row(self.window.post_address.get_first_child(), "/k/one")
        self.window.settings()
        navigation = self.window.dialogs[-1].settings_navigation
        self.window.account_settings(navigation)
        content = navigation.stack.get_visible_child().get_child()
        if isinstance(content, Gtk.Viewport):
            content = content.get_child()
        row = content.get_first_child().get_next_sibling()
        copy_row(row, "/k/one")
        self.assertEqual(self.window.identity, "/k/one")

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

    def clipboard_text(self):
        copied = []
        self.window.get_clipboard().read_text_async(None, lambda board, result: copied.append(board.read_text_finish(result)))
        pump(lambda: bool(copied))
        return copied[0] if copied else None

    def test_theme_choice_applies_and_persists(self):
        manager = Adw.StyleManager.get_default()
        self.assertEqual(self.preferences.appearance, "system")
        self.window.settings()
        buttons = self.window.appearance_buttons
        self.assertTrue(buttons["system"].get_active())
        buttons["dark"].set_active(True)
        pump(timeout=0.05)
        self.assertEqual(manager.get_color_scheme(), Adw.ColorScheme.FORCE_DARK)
        self.assertTrue(manager.get_dark())
        self.assertFalse(buttons["system"].get_active())
        self.assertEqual(Preferences(self.preferences.path).appearance, "dark")
        buttons["light"].set_active(True)
        pump(timeout=0.05)
        self.assertFalse(manager.get_dark())
        self.assertEqual(Preferences(self.preferences.path).appearance, "light")
        # A new window (the next launch) starts with the saved choice.
        manager.set_color_scheme(Adw.ColorScheme.DEFAULT)
        again = Window(self.app, api=self.api, restore=False, preferences=Preferences(self.preferences.path))
        self.assertEqual(manager.get_color_scheme(), Adw.ColorScheme.FORCE_LIGHT)
        again.close()
        buttons["system"].set_active(True)
        self.assertEqual(manager.get_color_scheme(), Adw.ColorScheme.DEFAULT)
        self.assertEqual(Preferences(self.preferences.path).appearance, "system")
        Path(self.preferences.path).write_text("not json")
        self.assertEqual(Preferences(self.preferences.path).appearance, "system")

    def test_composer_keeps_focus_after_send_and_enter(self):
        self.select()
        buffer = self.window.composer.get_buffer()
        buffer.set_text("First")
        self.window.send_button.grab_focus()
        self.window.send_button.emit("clicked")
        self.assertFalse(self.window.composer.get_sensitive(), "the box is locked while sending")
        pump(lambda: not self.window.sending and self.window.get_focus() is self.window.composer)
        self.assertIs(self.window.get_focus(), self.window.composer)
        buffer.set_text("Second")
        from gi.repository import Gdk
        self.assertTrue(self.window.composer_keys(None, Gdk.KEY_Return, 0, 0))
        pump(lambda: not self.window.sending and len([c for c in self.api.calls if c[1] == "/v1/send"]) == 2)
        pump(lambda: self.window.get_focus() is self.window.composer)
        self.assertIs(self.window.get_focus(), self.window.composer)
        self.assertEqual(buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False), "")

    def bodies(self, widget=None):
        widget = widget or self.window.message_list
        found = [widget] if isinstance(widget, Gtk.Label) and widget.has_css_class("pp-body") else []
        child = widget.get_first_child()
        while child:
            found.extend(self.bodies(child))
            child = child.get_next_sibling()
        return found

    def test_partial_text_selection_copies_only_the_selection_and_keeps_message_menu(self):
        self.select()
        body = self.bodies()[0]
        text = body.get_text()
        self.assertTrue(body.get_selectable())
        self.assertFalse(body.get_use_markup())
        body.select_region(4, 9)
        self.assertEqual(body.get_selection_bounds()[1:], (4, 9))
        body.emit("copy-clipboard")
        self.assertEqual(self.clipboard_text(), text[4:9])
        menu = body.get_extra_menu()
        labels = [menu.get_item_attribute_value(i, "label").unpack() for i in range(menu.get_n_items())]
        self.assertEqual(labels, ["Copy message", "Delete message", "Report spam"])
        self.assertTrue(body.activate_action("message.copy", None))
        self.assertEqual(self.clipboard_text(), text)
        mine = [b for b in self.bodies() if b.get_parent().get_parent().has_css_class("pp-mine")]
        self.assertTrue(mine)
        self.assertEqual(mine[0].get_extra_menu().get_n_items(), 2, "no spam report on your own message")

    def test_viewer_gating_never_renders_markup(self):
        from pigeonpost.viewer import viewer_kind, decode_image
        cases = {("a.png", "image/png"): "image", ("a.jpg", "application/octet-stream"): "image",
                 ("a.pdf", "application/pdf"): "pdf", ("a.txt", "text/plain"): "text", ("a.csv", ""): "text",
                 ("a.json", "application/json"): "text", ("a.mp4", "video/mp4"): "video", ("a.mp3", "audio/mpeg"): "audio",
                 ("a.html", "text/html"): None, ("a.html", "text/plain"): None, ("a.svg", "image/svg+xml"): None,
                 ("a.png", "image/svg+xml"): None, ("a.svg", "image/png"): None, ("a.xhtml", ""): None,
                 ("a.xml", "text/xml"): None, ("a.zip", "application/zip"): None, ("a", "application/octet-stream"): None}
        for (name, mime), expected in cases.items():
            self.assertEqual(viewer_kind(name, mime), expected, (name, mime))
        svg = b'<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>'
        with self.assertRaises(Exception):
            decode_image(svg, "image/png")  # declared PNG, actually SVG: never sniffed into a renderer
        reads = []
        viewer = self.window.open_attachment({"id": "x", "filename": "page.html", "media_type": "text/html"})
        self.assertIn("cannot be shown", viewer.status.get_text())
        self.assertFalse(viewer.save_button.get_sensitive())
        viewer.window.close()

    def test_viewer_opens_pictures_pdf_text_and_video_in_app(self):
        payloads = {"p": (FIXTURES / "preview.png").read_bytes(), "d": (FIXTURES / "preview.pdf").read_bytes(),
                    "t": "Plain <b>text</b> stays plain\nline two".encode(), "v": (FIXTURES / "preview.mp4").read_bytes()}
        self.api.download = lambda identity, attachment: payloads[attachment]
        saved = []
        self.window.save_attachment = lambda attachment, data=None: saved.append((attachment["id"], data))
        for key, name, mime, widget in [("p", "photo.png", "image/png", Gtk.Picture), ("d", "doc.pdf", "application/pdf", Gtk.ScrolledWindow),
                                        ("t", "notes.txt", "text/plain", Gtk.ScrolledWindow), ("v", "clip.mp4", "video/mp4", Gtk.Video)]:
            viewer = self.window.open_attachment({"id": key, "filename": name, "media_type": mime})
            self.assertIn(viewer.window, self.window.dialogs)
            pump(lambda: viewer.data is not None and viewer.status.get_parent() is None, timeout=8)
            shown = viewer.body.get_last_child()
            self.assertIsInstance(shown, widget, name)
            if key == "t":
                buffer = viewer.text_view.get_buffer()
                self.assertEqual(buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False), payloads["t"].decode())
                self.assertFalse(viewer.text_view.get_editable())
            if key == "v":
                folder = viewer.folder
                self.assertTrue(Path(folder).is_dir())
            viewer.save_button.emit("clicked")
            self.assertEqual(saved[-1], (key, payloads[key]))
            viewer.window.close()
            pump(timeout=0.05)
            self.assertNotIn(viewer.window, self.window.dialogs)
            if key == "v":
                self.assertFalse(Path(folder).exists(), "the temporary copy is removed when the viewer closes")

    def test_calls_open_the_web_inbox_with_the_address_copied(self):
        opened = []
        self.window.open_url = opened.append
        self.assertFalse(self.window.voice_call_button.get_sensitive())
        self.select()
        self.assertTrue(self.window.voice_call_button.get_sensitive())
        self.window.video_call_button.emit("clicked")
        self.assertEqual(opened, ["https://inbox.pigeonpost.dev/"])
        self.assertEqual(self.clipboard_text(), "/alex/main")

    def test_visual_evidence(self):
        if not os.environ.get("PIGEONPOST_SCREENSHOT_DIR"):
            self.skipTest("Screenshot capture requested only in GUI CI")
        directory = os.environ["PIGEONPOST_SCREENSHOT_DIR"]
        os.makedirs(directory, exist_ok=True)
        # Fixture conversation only; never real mail.
        icon = (Path(__file__).resolve().parents[1] / "data/dev.pigeonpost.Desktop.png").read_bytes()
        checklist = sample_checklist_pdf()
        self.api.download = lambda identity, attachment: checklist if attachment == "spec" else icon
        self.api.rows += [
            {"message_id": "d", "peer_handle": "/alex/main", "body": "Here is the release checklist and the new icon.", "received_at": 1789320100, "read": True, "thread_id": "release",
             "attachments": [{"id": "spec", "filename": "release-checklist.pdf", "media_type": "application/pdf", "bytes": 48213},
                             {"id": "icon", "filename": "icon.png", "media_type": "image/png", "bytes": 12034}]},
            {"message_id": "e", "peer_handle": "/alex/main", "body": '{"verb": "summarize", "args": {"task": "Summarize the open review comments on the Linux release."}}',
             "verb": "summarize", "autonomy": "review", "received_at": 1789320150, "read": True, "thread_id": "release"},
        ]
        self.window.messages = copy.deepcopy(self.api.rows)
        self.window.render_all()
        self.select()
        self.window.subject_list.select_row(self.window.subject_list.get_row_at_index(1))
        self.window.composer.get_buffer().set_text("Looks good — shipping it this afternoon.")
        for theme in ("light", "dark"):
            Adw.StyleManager.get_default().set_color_scheme(Adw.ColorScheme.FORCE_LIGHT if theme == "light" else Adw.ColorScheme.FORCE_DARK)
            pump(timeout=0.35)
            subprocess.run(["import", "-window", "root", f"{directory}/inbox-{theme}.png"], check=True)
        for theme in ("dark", "light"):
            Adw.StyleManager.get_default().set_color_scheme(Adw.ColorScheme.FORCE_LIGHT if theme == "light" else Adw.ColorScheme.FORCE_DARK)
            viewer = self.window.open_attachment(self.api.rows[-2]["attachments"][0])
            pump(lambda: viewer.data is not None and viewer.status.get_parent() is None, timeout=8)
            pump(timeout=0.5)
            subprocess.run(["import", "-window", "root", f"{directory}/viewer-{theme}.png"], check=True)
            viewer.window.close()
        Adw.StyleManager.get_default().set_color_scheme(Adw.ColorScheme.FORCE_DARK)
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
