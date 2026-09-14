"""GTK/libadwaita desktop. All HTTP and Secret Service calls run on workers."""

import datetime
import mimetypes
import threading
from pathlib import Path
from urllib.parse import quote

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, Gio, GLib, Gtk, Pango

from . import APP_ID, VERSION
from .api import APIError, MAX_FILE, Postbox
from .auth import Session
from .model import (contact_for, conversations, message_text, normalize, safe_filename,
                    size_text, subjects, target_thread, timestamp, valid_peer)
from .vault import Vault


def box(spacing=8, horizontal=False, margin=0):
    widget = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL if horizontal else Gtk.Orientation.VERTICAL, spacing=spacing)
    for edge in ("top", "bottom", "start", "end"):
        getattr(widget, "set_margin_" + edge)(margin)
    return widget


def label(text, style=None, wrap=False):
    widget = Gtk.Label(label=str(text), xalign=0)
    if style:
        widget.add_css_class(style)
    if wrap:
        widget.set_wrap(True)
        widget.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
    else:
        widget.set_ellipsize(Pango.EllipsizeMode.END)
    return widget


def button(text, callback, icon=None, style=None):
    widget = Gtk.Button(label=text) if not icon else Gtk.Button(icon_name=icon)
    widget.set_tooltip_text(text)
    widget.update_property([Gtk.AccessibleProperty.LABEL], [text])
    if style:
        widget.add_css_class(style)
    widget.connect("clicked", lambda _: callback())
    return widget


def clear(widget):
    while widget.get_first_child():
        widget.remove(widget.get_first_child())


def address_row(address):
    row = box(4, horizontal=True)
    text = label(address, "dim-label", wrap=True)
    text.set_selectable(True)
    text.set_hexpand(True)
    row.append(text)

    def copied():
        copy.get_clipboard().set(address)
        copy.set_icon_name("emblem-ok-symbolic")
        copy.set_tooltip_text("Address copied")
        copy.update_property([Gtk.AccessibleProperty.LABEL], ["Address copied"])
        if copy.reset_source:
            GLib.source_remove(copy.reset_source)
        copy.reset_source = GLib.timeout_add_seconds(2, reset)

    def reset():
        copy.set_icon_name("edit-copy-symbolic")
        copy.set_tooltip_text("Copy address " + address)
        copy.update_property([Gtk.AccessibleProperty.LABEL], ["Copy address " + address])
        copy.reset_source = None
        return False

    copy = button("Copy address " + address, copied, "edit-copy-symbolic", "flat")
    copy.reset_source = None
    copy.set_sensitive(bool(address))
    copy.set_valign(Gtk.Align.CENTER)
    row.append(copy)
    return row


def scroll(child):
    widget = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
    widget.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
    widget.set_child(child)
    return widget


def settings_row(title, detail, icon, callback):
    row = Adw.ActionRow(title=title, subtitle=detail, activatable=True)
    row.set_title_lines(0)
    row.set_subtitle_lines(0)
    row.add_prefix(Gtk.Image(icon_name=icon))
    row.add_suffix(Gtk.Image(icon_name="go-next-symbolic"))
    row.connect("activated", lambda _: callback())
    return row


def settings_group(content, *rows):
    group = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
    group.add_css_class("boxed-list")
    for row in rows:
        group.append(row)
    content.append(group)


class SettingsNavigation:
    """One native window with a back stack, compatible with libadwaita 1.2."""
    def __init__(self, owner, title="Settings"):
        self.window = Adw.Window(title=title, transient_for=owner, modal=True,
                                 default_width=560, default_height=600)
        self.window.settings_navigation = self
        self.pages = []
        root = box(0)
        header = Adw.HeaderBar()
        self.heading = Adw.WindowTitle(title=title)
        header.set_title_widget(self.heading)
        self.back_button = button("Back", self.back, "go-previous-symbolic")
        header.pack_start(self.back_button)
        root.append(header)
        self.stack = Gtk.Stack(vexpand=True, transition_type=Gtk.StackTransitionType.SLIDE_LEFT_RIGHT)
        root.append(self.stack)
        self.window.set_content(root)
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", self.key_pressed)
        self.window.add_controller(keys)
        owner.dialogs.append(self.window)
        self.window.connect("close-request", lambda w: owner.dialogs.remove(w) if w in owner.dialogs else None)
        self.content = self.push(title)

    def push(self, title):
        focus = self.window.get_focus()
        content = box(18, margin=24)
        view = scroll(content)
        self.pages.append((title, view, focus))
        self.stack.add_child(view)
        self.stack.set_visible_child(view)
        self.heading.set_title(title)
        self.window.set_title(title)
        self.back_button.set_visible(len(self.pages) > 1)
        GLib.idle_add(lambda: (content.child_focus(Gtk.DirectionType.TAB_FORWARD), False)[1])
        return content

    def back(self):
        if len(self.pages) < 2:
            return
        _, old, focus = self.pages.pop()
        title, view, _ = self.pages[-1]
        self.stack.set_visible_child(view)
        self.stack.remove(old)
        self.heading.set_title(title)
        self.window.set_title(title)
        self.back_button.set_visible(len(self.pages) > 1)
        if focus:
            focus.grab_focus()

    def key_pressed(self, _, key, _code, _state):
        if key != Gdk.KEY_Escape:
            return False
        if len(self.pages) > 1:
            self.back()
        else:
            self.window.close()
        return True


class Window(Adw.ApplicationWindow):
    def __init__(self, app, session=None, api=None, restore=True):
        super().__init__(application=app, title="Pigeonpost", default_width=1180, default_height=780)
        self.set_size_request(800, 540)
        self.session = session or Session(Vault())
        self.api = api or Postbox(self.session)
        self.generation, self.revision, self.closed = 0, 0, False
        self.cancel = threading.Event()
        self.mailboxes, self.messages, self.threads, self.contacts, self.archived = [], [], [], [], []
        self.vocabulary, self.quota = {}, {}
        self.identity = self.peer = self.subject = None
        self.drafts, self.files = {}, {}
        self.polling = self.sending = self.rendering = self.acking = False
        self.known_ids, self.dialogs = None, []
        self.message_limit = 200
        self.pool = threading.BoundedSemaphore(8)
        self.overlay = Adw.ToastOverlay()
        self.root = box(0)
        self.header = Adw.HeaderBar()
        self.title = Adw.WindowTitle(title="Pigeonpost", subtitle="Private messaging")
        self.header.set_title_widget(self.title)
        self.root.append(self.header)
        self.stack = Gtk.Stack(transition_type=Gtk.StackTransitionType.CROSSFADE, vexpand=True)
        self.root.append(self.stack)
        self.overlay.set_child(self.root)
        self.set_content(self.overlay)
        self.account_button = button("Settings and account", self.settings, "emblem-system-symbolic")
        self.header.pack_end(self.account_button)
        self.refresh_button = button("Refresh inbox", lambda: self.refresh(), "view-refresh-symbolic")
        self.header.pack_end(self.refresh_button)
        self.signin_view()
        self.inbox_view()
        self.show_signin("Restoring your session…" if restore else "Sign in to your Pigeonpost account.")
        self.connect("close-request", self.on_close)
        self.connect("notify::is-active", lambda *_: self.ack_visible())
        controller = Gtk.EventControllerKey()
        controller.connect("key-pressed", self.shortcuts)
        self.add_controller(controller)
        GLib.timeout_add_seconds(4, self.tick)
        if restore:
            self._work(self.session.restore, lambda ok: self.load_account() if ok else self.show_signin(),
                       lambda e: self.show_signin(self.error_text(e)))

    def _work(self, operation, done=None, failed=None):
        generation = self.generation
        def deliver(value, error=False):
            if self.closed or generation != self.generation:
                return False
            if error:
                (failed or self.error)(value)
            elif done:
                done(value)
            return False
        def run():
            with self.pool:
                if self.closed or generation != self.generation:
                    return
                try:
                    value = operation()
                except Exception as exc:
                    GLib.idle_add(deliver, exc, True)
                else:
                    GLib.idle_add(deliver, value)
        threading.Thread(target=run, daemon=True).start()

    @staticmethod
    def error_text(error):
        if isinstance(error, APIError):
            return str(error)
        if isinstance(error, GLib.Error):
            return "Unlock your desktop keyring and try again. Pigeonpost needs Secret Service to remember your account."
        if isinstance(error, OSError):
            return "The selected file could not be read or saved. Check its permissions and available space."
        return "This action could not be completed. Please try again."

    def error(self, error):
        self.toast(self.error_text(error))
        if isinstance(error, APIError) and error.code == "invalid_grant":
            self.generation += 1
            self.clear_account()
            self.show_signin(str(error))

    def toast(self, text):
        self.overlay.add_toast(Adw.Toast.new(text))

    def open_url(self, url):
        try:
            Gio.AppInfo.launch_default_for_uri(url, self.get_display().get_app_launch_context())
        except GLib.Error:
            self.toast("Could not open your browser. Copy the link and open it manually.")

    def signin_view(self):
        outer = box(18, margin=36)
        outer.set_valign(Gtk.Align.CENTER)
        outer.set_halign(Gtk.Align.CENTER)
        outer.set_size_request(460, -1)
        image = Gtk.Image.new_from_icon_name(APP_ID)
        image.set_pixel_size(96)
        outer.append(image)
        title = label("Your people. Your agents.", "title-1")
        title.set_halign(Gtk.Align.CENTER)
        outer.append(title)
        self.login_status = label("", wrap=True)
        self.login_status.set_max_width_chars(58)
        outer.append(self.login_status)
        self.login_button = button("Sign in with your browser", self.sign_in, style="suggested-action")
        outer.append(self.login_button)
        self.code_label = label("", "title-2")
        self.code_label.set_selectable(True)
        outer.append(self.code_label)
        self.browser_button = button("Open sign-in page", lambda: self.open_url(self.verification_url))
        outer.append(self.browser_button)
        self.cancel_button = button("Cancel sign-in", self.cancel_login)
        outer.append(self.cancel_button)
        outer.append(button("Privacy policy", lambda: self.open_url("https://pigeonpost.dev/privacy")))
        self.stack.add_named(outer, "signin")

    def show_signin(self, text="Sign in to your Pigeonpost account. Your browser handles the password."):
        self.stack.set_visible_child_name("signin")
        self.account_button.set_sensitive(False)
        self.refresh_button.set_sensitive(False)
        self.login_status.set_text(text)
        self.login_button.set_sensitive(True)
        self.code_label.set_visible(False)
        self.browser_button.set_visible(False)
        self.cancel_button.set_visible(False)
        self.title.set_subtitle("Private messaging")

    def sign_in(self):
        self.cancel = threading.Event()
        cancel = self.cancel
        self.login_button.set_sensitive(False)
        self.login_status.set_text("Preparing a secure sign-in…")
        self.cancel_button.set_visible(True)
        def begun(device):
            self.verification_url = device.get("verification_uri_complete", device["verification_uri"])
            self.code_label.set_text(device["user_code"])
            self.code_label.set_visible(True)
            self.browser_button.set_visible(True)
            self.login_status.set_text("Approve Pigeonpost Desktop (Linux) in your browser. Check that the code matches.")
            self.open_url(self.verification_url)
            self._work(lambda: self.session.complete(device, cancel),
                       lambda ok: self.load_account() if ok else self.show_signin(),
                       lambda e: self.show_signin(self.error_text(e)))
        self._work(self.session.begin, begun, lambda e: self.show_signin(self.error_text(e)))

    def cancel_login(self):
        self.cancel.set()
        self.generation += 1
        self.show_signin("Sign-in cancelled.")

    def load_account(self):
        self.login_status.set_text("Opening your mailboxes…")
        self.login_button.set_sensitive(False)
        self._work(self.api.identities, self.account_loaded, lambda e: self.show_signin(self.error_text(e)))

    def account_loaded(self, rows):
        self.mailboxes = rows
        self.account_button.set_sensitive(True)
        self.refresh_button.set_sensitive(True)
        self.stack.set_visible_child_name("inbox")
        self.rendering = True
        self.mailbox_model.splice(0, self.mailbox_model.get_n_items(),
                                 [r.get("handle") or r.get("label") or r["address"] for r in rows])
        self.rendering = False
        self.create_button.set_visible(not rows)
        self.mailbox_picker.set_visible(bool(rows))
        self.post_address.set_visible(bool(rows))
        if rows:
            self.mailbox_picker.set_selected(0)
            self.select_mailbox()
        else:
            self.empty_thread("Create your inbox to start messaging.")

    def create_inbox(self):
        self.create_button.set_sensitive(False)
        def create():
            rows = self.api.identities()
            if not rows:
                self.api.call("POST", "/v1/identities", data={})
            return self.api.identities()
        def failed(error):
            self.create_button.set_sensitive(True)
            self.error(error)
        self._work(create, self.account_loaded, failed)

    def inbox_view(self):
        sidebar = box(10, margin=12)
        sidebar.set_size_request(210, -1)
        self.mailbox_model = Gtk.StringList.new([])
        self.mailbox_picker = Gtk.DropDown(model=self.mailbox_model)
        self.mailbox_picker.set_tooltip_text("Active mailbox")
        self.mailbox_picker.connect("notify::selected", lambda *_: self.select_mailbox())
        sidebar.append(self.mailbox_picker)
        self.post_address = box(0)
        sidebar.append(self.post_address)
        self.create_button = button("Create my inbox", self.create_inbox, style="suggested-action")
        sidebar.append(self.create_button)
        self.peer_search = Gtk.SearchEntry(placeholder_text="Search conversations")
        self.peer_search.connect("search-changed", lambda _: self.render_peers())
        sidebar.append(self.peer_search)
        sidebar.append(button("New conversation", self.new_conversation, "list-add-symbolic"))
        self.peer_list = Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
        self.peer_list.add_css_class("navigation-sidebar")
        self.peer_list.connect("row-selected", self.peer_selected)
        sidebar.append(scroll(self.peer_list))
        self.connection_status = label("", "dim-label", wrap=True)
        sidebar.append(self.connection_status)
        sidebar.append(button("Handles", self.handles))
        subpanel = box(10, margin=12)
        subpanel.set_size_request(165, -1)
        subhead = box(horizontal=True)
        heading = label("Subjects", "heading")
        heading.set_hexpand(True)
        subhead.append(heading)
        subhead.append(button("New subject", self.new_subject, "list-add-symbolic"))
        subpanel.append(subhead)
        self.subject_list = Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
        self.subject_list.add_css_class("navigation-sidebar")
        self.subject_list.connect("row-selected", self.subject_selected)
        subpanel.append(scroll(self.subject_list))
        self.subject_delete = button("Delete subject", self.delete_subject, "user-trash-symbolic")
        subpanel.append(self.subject_delete)
        detail = box(0)
        detail.set_size_request(350, -1)
        toolbar = box(horizontal=True, margin=12)
        self.peer_title = label("Your inbox", "title-3")
        self.peer_title.set_hexpand(True)
        toolbar.append(self.peer_title)
        toolbar.append(button("Contact details", lambda: self.contact_editor(self.peer), "avatar-default-symbolic"))
        toolbar.append(button("Archive conversation", self.archive_peer, "folder-download-symbolic"))
        detail.append(toolbar)
        self.message_search = Gtk.SearchEntry(placeholder_text="Search this conversation")
        self.message_search.set_margin_start(12)
        self.message_search.set_margin_end(12)
        self.message_search.connect("search-changed", lambda _: self.render_messages())
        detail.append(self.message_search)
        self.message_list = box(12, margin=16)
        self.message_scroll = scroll(self.message_list)
        detail.append(self.message_scroll)
        self.file_chips = box(horizontal=True, margin=8)
        detail.append(self.file_chips)
        composer = box(horizontal=True, margin=12)
        self.attach_button = button("Attach a file", self.pick_file, "mail-attachment-symbolic")
        composer.append(self.attach_button)
        self.composer = Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR, accepts_tab=False)
        self.composer.set_top_margin(8)
        self.composer.set_bottom_margin(8)
        self.composer.set_left_margin(10)
        self.composer.set_right_margin(10)
        self.composer.update_property([Gtk.AccessibleProperty.LABEL], ["Write a message. Enter sends; Shift Enter adds a line."])
        self.composer.get_buffer().connect("changed", self.draft_changed)
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", self.composer_keys)
        self.composer.add_controller(keys)
        compose_scroll = Gtk.ScrolledWindow(hexpand=True, min_content_height=64, max_content_height=130)
        compose_scroll.set_child(self.composer)
        composer.append(compose_scroll)
        self.send_button = button("Send message", self.send, "mail-send-symbolic", "suggested-action")
        composer.append(self.send_button)
        detail.append(composer)
        drop = Gtk.DropTarget.new(Gio.File, Gdk.DragAction.COPY)
        drop.connect("drop", lambda _, file, x, y: self.stage_file(file))
        detail.add_controller(drop)
        right = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL, position=200)
        right.set_start_child(subpanel)
        right.set_end_child(detail)
        right.set_resize_start_child(False)
        right.set_shrink_start_child(False)
        right.set_shrink_end_child(False)
        panes = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL, position=270)
        panes.set_start_child(sidebar)
        panes.set_end_child(right)
        panes.set_resize_start_child(False)
        panes.set_shrink_start_child(False)
        panes.set_shrink_end_child(False)
        self.stack.add_named(panes, "inbox")

    def select_mailbox(self):
        if self.rendering or not self.mailboxes:
            return
        index = self.mailbox_picker.get_selected()
        if index >= len(self.mailboxes):
            return
        row = self.mailboxes[index]
        clear(self.post_address)
        self.post_address.append(address_row(row.get("handle") or row["address"]))
        if row["address"] == self.identity:
            return
        self.generation += 1
        for dialog in list(self.dialogs):
            dialog.close()
        self.identity, self.peer, self.subject = row["address"], None, None
        self.messages, self.contacts, self.threads, self.archived = [], [], [], []
        self.quota, self.vocabulary, self.known_ids = {}, {}, None
        self.polling = self.sending = self.acking = False
        self.title.set_subtitle(row.get("handle") or row.get("label") or row["address"])
        self.restore_draft()
        self.render_all()
        self.refresh()

    def tick(self):
        if self.closed:
            return False
        if self.identity and not self.polling:
            self.refresh(wait=25)
        return True

    def refresh(self, wait=0):
        if not self.identity or self.polling:
            return
        identity, revision = self.identity, self.revision
        self.polling = True
        if self.known_ids is None:
            self.connection_status.set_text("Loading inbox…")
        def read():
            messages = self.api.inbox(identity, wait)
            threads = self.api.call("GET", "/v1/threads", identity).get("threads", [])
            contacts = self.api.call("GET", "/v1/contacts", identity)
            archived = self.api.call("GET", "/v1/archive", identity).get("archived", [])
            quota = self.api.call("GET", "/v1/quota", identity)
            return messages, threads, contacts, archived, quota
        def loaded(result):
            self.polling = False
            if revision != self.revision:
                return  # A mutation completed after this snapshot began; fetch again next tick.
            messages, threads, contacts, archived, quota = result
            ids = {m["message_id"] for m in messages}
            if self.known_ids is not None:
                new = [m for m in messages if m["message_id"] not in self.known_ids and m.get("direction") != "out" and not m.get("read")]
                if new and not self.is_active():
                    notification = Gio.Notification.new("New Pigeonpost message")
                    notification.set_body("Open Pigeonpost to read your messages.")
                    self.get_application().send_notification("new-mail", notification)
            changed = (messages, threads, contacts.get("contacts", []), archived) != (self.messages, self.threads, self.contacts, self.archived)
            self.messages, self.threads = messages, threads
            self.contacts, self.vocabulary = contacts.get("contacts", []), contacts.get("vocabulary", {})
            self.archived, self.quota, self.known_ids = archived, quota, ids
            self.connection_status.set_text("Connected")
            if changed:
                self.render_all()
            self.ack_visible()
        def failed(error):
            self.polling = False
            self.connection_status.set_text("Offline — retrying…" if getattr(error, "status", 0) != 401 else "Sign-in required")
            if self.known_ids is None or getattr(error, "code", "") == "invalid_grant":
                self.error(error)
        self._work(read, loaded, failed)

    def render_all(self):
        self.render_peers()
        self.render_subjects()
        self.render_messages()

    def render_peers(self):
        self.rendering = True
        clear(self.peer_list)
        search = self.peer_search.get_text().casefold()
        rows = conversations(self.messages, self.contacts)
        if self.peer:
            rows.setdefault(self.peer, [])
        for peer, messages in list(rows.items())[:1000]:
            contact = contact_for(peer, self.contacts)
            name = contact.get("alias") or peer
            if peer in self.archived or search not in (name + " " + peer).casefold():
                continue
            row = Gtk.ListBoxRow()
            row.peer = peer
            content = box(4, margin=8)
            unread = sum(m.get("direction") != "out" and not m.get("read") for m in messages)
            content.append(label(name + (f"  • {unread}" if unread else ""), "heading"))
            content.append(label(message_text(messages[-1]).replace("\n", " ")[:120] if messages else "Start a conversation", "dim-label"))
            row.set_child(content)
            self.peer_list.append(row)
            if peer == self.peer:
                self.peer_list.select_row(row)
        self.rendering = False

    def peer_selected(self, _, row):
        if self.rendering or not row:
            return
        self.peer, self.subject, self.message_limit = row.peer, None, 200
        self.message_search.set_text("")
        self.render_subjects()
        self.restore_draft()
        self.render_messages()
        self.ack_visible()

    def render_subjects(self):
        self.rendering = True
        clear(self.subject_list)
        rows = [{"thread_id": None, "title": "All messages"}] + subjects(self.peer, self.messages, self.threads) if self.peer else []
        for item in rows:
            row = Gtk.ListBoxRow()
            row.subject = item["thread_id"]
            row.set_child(label(item.get("title") or ("General" if item.get("is_default") else "Untitled")))
            row.get_child().set_margin_top(12)
            row.get_child().set_margin_bottom(12)
            self.subject_list.append(row)
            if row.subject == self.subject:
                self.subject_list.select_row(row)
        self.rendering = False
        self.subject_delete.set_sensitive(bool(self.subject))

    def subject_selected(self, _, row):
        if self.rendering or not row:
            return
        self.subject, self.message_limit = row.subject, 200
        self.subject_delete.set_sensitive(bool(self.subject))
        self.restore_draft()
        self.render_messages()
        self.ack_visible()

    def shown_messages(self):
        rows = conversations(self.messages).get(self.peer, [])
        return [m for m in rows if self.subject is None or (m.get("thread_id") or "") == self.subject]

    def empty_thread(self, text):
        clear(self.message_list)
        self.message_list.append(label(text, "dim-label", wrap=True))
        self.update_send()

    def render_messages(self):
        if not self.peer:
            self.peer_title.set_text("Your inbox")
            self.empty_thread("Choose a conversation, or start a new one.")
            return
        self.peer_title.set_text(contact_for(self.peer, self.contacts).get("alias") or self.peer)
        adj = self.message_scroll.get_vadjustment()
        bottom = adj.get_value() + adj.get_page_size() >= adj.get_upper() - 80
        clear(self.message_list)
        query = self.message_search.get_text().casefold()
        rows = [m for m in self.shown_messages() if query in message_text(m).casefold()]
        if len(rows) > self.message_limit:
            self.message_list.append(button(f"Load earlier messages ({len(rows) - self.message_limit})", self.load_earlier))
        if not rows:
            self.message_list.append(label("No matching messages." if query else "Say hello to start this conversation.", "dim-label", wrap=True))
        for message in rows[-self.message_limit:]:
            outgoing = message.get("direction") == "out"
            bubble = box(6, margin=4)
            bubble.add_css_class("card")
            inner = box(6, margin=12)
            who = "You" if outgoing else self.peer
            when = timestamp(message)
            try:
                suffix = datetime.datetime.fromtimestamp(when / 1000 if when > 10**11 else when).strftime("%d %b · %H:%M") if when else ""
            except (ValueError, OSError, OverflowError):
                suffix = ""
            inner.append(label(who + "   " + suffix, "dim-label"))
            if message.get("verb"):
                state = "Review required" if message.get("autonomy") != "auto" else "Automatic handling allowed"
                inner.append(label(state + " · " + str(message["verb"]), "heading", wrap=True))
            text = label(message_text(message), wrap=True)
            text.set_selectable(True)
            inner.append(text)
            for attachment in message.get("attachments") or []:
                inner.append(button("Save " + safe_filename(attachment.get("filename", "attachment")) + " · " + size_text(attachment.get("bytes", 0)),
                                    lambda a=attachment: self.save_attachment(a)))
            actions = box(horizontal=True)
            actions.append(button("Copy message", lambda m=message: self.get_clipboard().set(message_text(m)), "edit-copy-symbolic"))
            actions.append(button("Delete message", lambda m=message: self.delete_message(m), "user-trash-symbolic"))
            if not outgoing:
                actions.append(button("Report spam", lambda m=message: self.report_message(m), "dialog-warning-symbolic"))
            inner.append(actions)
            bubble.append(inner)
            bubble.set_margin_start(36 if outgoing else 0)
            bubble.set_margin_end(0 if outgoing else 36)
            self.message_list.append(bubble)
        self.update_send()
        if bottom and not query:
            GLib.idle_add(lambda: adj.set_value(max(0, adj.get_upper() - adj.get_page_size())))

    def load_earlier(self):
        self.message_limit += 200
        self.render_messages()

    def ack_visible(self):
        if not self.identity or not self.peer or not self.is_active():
            return
        # Only actual displayed messages, not hidden subjects/search results or unloaded history.
        query = self.message_search.get_text().casefold()
        rows = [m for m in self.shown_messages() if query in message_text(m).casefold()][-self.message_limit:]
        ids = [m["message_id"] for m in rows if m.get("direction") != "out" and not m.get("read")]
        identity = self.identity
        if not ids or getattr(self, "acking", False):
            return
        self.acking = True
        def ack():
            succeeded = []
            for mid in ids:
                try:
                    self.api.call("POST", "/v1/ack", identity, data={"message_id": mid})
                    succeeded.append(mid)
                except APIError:
                    break
            return succeeded
        def done(acked):
            self.acking = False
            for m in self.messages:
                if m["message_id"] in acked:
                    m["read"] = True
            self.render_peers()
        self._work(ack, done, lambda _: setattr(self, "acking", False))

    def draft_key(self):
        return self.identity, self.peer, self.subject

    def draft_changed(self, buffer):
        if not self.rendering:
            self.drafts[self.draft_key()] = buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)
        self.update_send()

    def restore_draft(self):
        self.rendering = True
        self.composer.get_buffer().set_text(self.drafts.get(self.draft_key(), ""))
        self.rendering = False
        clear(self.file_chips)
        for item in self.files.get(self.draft_key(), []):
            self.file_chips.append(button("Remove " + item["name"], lambda f=item: self.remove_file(f)))
        self.update_send()

    def update_send(self):
        enabled = bool(self.peer and self.identity and not self.sending)
        self.composer.set_sensitive(enabled)
        self.attach_button.set_sensitive(enabled)
        self.send_button.set_sensitive(enabled and bool(self.drafts.get(self.draft_key(), "").strip() or self.files.get(self.draft_key())))

    def composer_keys(self, _, key, code, state):
        if key in (Gdk.KEY_Return, Gdk.KEY_KP_Enter) and not state & Gdk.ModifierType.SHIFT_MASK:
            self.send()
            return True
        return False

    def shortcuts(self, _, key, code, state):
        if state & Gdk.ModifierType.CONTROL_MASK:
            if key == Gdk.KEY_q:
                self.close()
                return True
            if key == Gdk.KEY_f:
                self.message_search.grab_focus()
                return True
            if key == Gdk.KEY_n:
                self.new_conversation()
                return True
            if key == Gdk.KEY_r:
                self.refresh()
                return True
        return False

    def send(self):
        if not self.send_button.get_sensitive():
            return
        key = self.draft_key()
        identity, peer, selected = key
        body = self.drafts.get(key, "").strip()
        files = list(self.files.get(key, []))
        thread = target_thread(selected, subjects(peer, self.messages, self.threads))
        self.sending = True
        self.update_send()
        def send():
            attachments = []
            for file in files:
                if not file.get("uploaded"):
                    file["uploaded"] = self.api.upload(identity, file["name"], file["media_type"], file["content"])
                attachments.append(file["uploaded"]["id"])
            payload = {"from": identity, "to": peer, "body": body}
            if thread:
                payload["thread_id"] = thread
            if attachments:
                payload["attachments"] = attachments
            return self.api.call("POST", "/v1/send", data=payload)
        def sent(result):
            self.sending = False
            self.revision += 1
            self.drafts.pop(key, None)
            self.files.pop(key, None)
            self.restore_draft()
            self.toast("Message sent")
            # Read immediately; the long-poll worker may still be waiting on another update.
            self.reload_messages()
        def failed(error):
            self.sending = False
            self.update_send()
            message = self.error_text(error)
            if getattr(error, "status", 0) == 0 or getattr(error, "status", 0) >= 500:
                message += " Your draft is saved here. Check the conversation before retrying; delivery may have succeeded."
            self.notice("Message could not be confirmed", message)
        self._work(send, sent, failed)

    def sent_snapshot(self, messages):
        self.messages = messages
        self.render_all()

    def reload_messages(self):
        identity, revision = self.identity, self.revision
        self._work(lambda: self.api.inbox(identity),
                   lambda rows: self.sent_snapshot(rows) if revision == self.revision else None)

    def pick_file(self):
        if not self.peer:
            return
        chooser = Gtk.FileChooserNative.new("Attach a file", self, Gtk.FileChooserAction.OPEN, "Attach", "Cancel")
        chooser.set_select_multiple(True)
        def chosen(dialog, response):
            if response == Gtk.ResponseType.ACCEPT:
                files = dialog.get_files()
                for index in range(files.get_n_items()):
                    self.stage_file(files.get_item(index))
            dialog.destroy()
        chooser.connect("response", chosen)
        chooser.show()

    def stage_file(self, file):
        if not self.peer or self.sending:
            return False
        key = self.draft_key()
        if len(self.files.get(key, [])) >= 5:
            self.toast("Attach up to five files per message.")
            return False
        def read():
            path = file.get_path()
            if not path or not Path(path).is_file():
                raise OSError("Not a local file")
            with open(path, "rb") as stream:
                content = stream.read(MAX_FILE + 1)
            if len(content) > MAX_FILE:
                raise APIError(0, "file_too_large", "Choose a file smaller than 25 MiB.")
            return {"name": Path(path).name, "content": content,
                    "media_type": mimetypes.guess_type(path)[0] or "application/octet-stream"}
        def staged(item):
            if len(self.files.get(key, [])) < 5:
                self.files.setdefault(key, []).append(item)
            if key == self.draft_key():
                self.restore_draft()
        self._work(read, staged)
        return True

    def remove_file(self, item):
        if not self.sending:
            self.files[self.draft_key()].remove(item)
            self.restore_draft()

    def save_attachment(self, attachment):
        identity = self.identity
        chooser = Gtk.FileChooserNative.new("Save attachment", self, Gtk.FileChooserAction.SAVE, "Save", "Cancel")
        chooser.set_current_name(safe_filename(attachment.get("filename", "attachment")))
        def chosen(dialog, response):
            file = dialog.get_file() if response == Gtk.ResponseType.ACCEPT else None
            dialog.destroy()
            if not file:
                return
            def save():
                data = self.api.download(identity, attachment["id"])
                # GIO atomically replaces only the explicit user-selected path, including portals.
                file.replace_contents(data, None, False, Gio.FileCreateFlags.REPLACE_DESTINATION, None)
            self._work(save, lambda _: self.toast("Attachment saved"))
        chooser.connect("response", chosen)
        chooser.show()

    def dialog(self, title, width=510):
        window = Adw.Window(title=title, transient_for=self, modal=True, default_width=width, default_height=440)
        root = box(0)
        header = Adw.HeaderBar()
        header.set_title_widget(Adw.WindowTitle(title=title))
        root.append(header)
        content = box(14, margin=24)
        root.append(scroll(content))
        window.set_content(root)
        self.dialogs.append(window)
        window.connect("close-request", lambda w: self.dialogs.remove(w) if w in self.dialogs else None)
        return window, content

    def notice(self, title, body):
        dialog = Adw.MessageDialog.new(self, title, body)
        dialog.add_response("close", "Close")
        dialog.present()

    def confirm(self, title, body, action, caption="Delete"):
        dialog = Adw.MessageDialog.new(self, title, body)
        dialog.add_response("cancel", "Cancel")
        dialog.add_response("confirm", caption)
        dialog.set_response_appearance("confirm", Adw.ResponseAppearance.DESTRUCTIVE)
        dialog.set_default_response("cancel")
        dialog.set_close_response("cancel")
        generation = self.generation
        dialog.connect("response", lambda _, response: action() if response == "confirm" and generation == self.generation else None)
        dialog.present()

    def form_dialog(self, title, placeholder, submitted, initial=""):
        window, content = self.dialog(title)
        entry = Gtk.Entry(placeholder_text=placeholder, text=initial)
        content.append(entry)
        error_label = label("", "error", wrap=True)
        content.append(error_label)
        def submit():
            value = entry.get_text().strip()
            try:
                submitted(value)
            except ValueError as error:
                error_label.set_text(str(error))
                return
            window.close()
        content.append(button("Continue", submit, style="suggested-action"))
        entry.connect("activate", lambda _: submit())
        window.present()
        entry.grab_focus()

    def new_conversation(self):
        if not self.identity:
            return
        def start(peer):
            if not valid_peer(peer):
                raise ValueError("Use a mailbox address such as /name/main or /k/your-address.")
            self.peer, self.subject = normalize(peer, self.messages), None
            self.render_all()
            self.restore_draft()
            self.composer.grab_focus()
        self.form_dialog("New conversation", "/name/main", start)

    def new_subject(self):
        if not self.peer:
            self.toast("Choose a conversation first.")
            return
        identity, peer = self.identity, self.peer
        def create(title):
            if not title or len(title) > 160:
                raise ValueError("Enter a subject of 1–160 characters.")
            def done(result):
                self.revision += 1
                self.threads.append({"peer": peer, "thread_id": result["thread_id"], "title": title})
                if self.peer == peer:
                    self.subject = result["thread_id"]
                    self.render_subjects()
                    self.restore_draft()
                    self.render_messages()
            self._work(lambda: self.api.call("POST", "/v1/threads", identity, data={"peer": peer, "title": title}), done)
        self.form_dialog("New subject", "Subject", create)

    def mutate(self, method, path, data=None):
        identity = self.identity
        def done(_):
            self.revision += 1
            self.reload_messages()
            self.toast("Saved")
        self._work(lambda: self.api.call(method, path, identity, data=data), done)

    def delete_subject(self):
        subject, identity, peer = self.subject, self.identity, self.peer
        if not subject:
            return
        def done(_):
            self.revision += 1
            self.messages = [m for m in self.messages if m.get("thread_id") != subject]
            self.threads = [t for t in self.threads if t.get("thread_id") != subject]
            if self.peer == peer and self.subject == subject:
                self.subject = None
            self.restore_draft()
            self.render_all()
        self.confirm("Delete this subject?", "This removes its messages from your mailbox. Other participants keep their copies.",
                     lambda: self._work(lambda: self.api.call("DELETE", "/v1/threads/" + quote(subject, safe=""), identity), done))

    def delete_message(self, message):
        self.confirm("Delete this message?", "This removes your copy. Other participants keep their copies.",
                     lambda: self.mutate("POST", "/v1/messages/delete", {"message_id": message["message_id"]}))

    def report_message(self, message):
        self.confirm("Report this message as spam?", "Pigeonpost will receive a spam report for this message.",
                     lambda: self.mutate("POST", "/v1/report-spam", {"message_id": message["message_id"]}), "Report spam")

    def archive_peer(self, peer=None, archived=True):
        peer, identity = peer or self.peer, self.identity
        if not peer:
            return
        def done(_):
            self.revision += 1
            if archived and peer not in self.archived:
                self.archived.append(peer)
            elif not archived and peer in self.archived:
                self.archived.remove(peer)
            if self.peer == peer and archived:
                self.peer = self.subject = None
            self.restore_draft()
            self.render_all()
            self.toast("Conversation archived" if archived else "Conversation restored")
        self._work(lambda: self.api.call("PUT", "/v1/archive", identity, data={"peer": peer, "archived": archived}), done)

    def settings(self):
        navigation = SettingsNavigation(self)
        content = navigation.content
        settings_group(content, settings_row("Account", self.title.get_subtitle() or "Your profile and devices",
                       "avatar-default-symbolic", lambda: self.account_settings(navigation)))
        settings_group(content,
            settings_row("Handles", "Your names and subscriptions", "insert-link-symbolic", lambda: self.handles(navigation)),
            settings_row("Inbox and storage", "Storage and archived conversations", "mail-unread-symbolic", lambda: self.inbox_settings(navigation)),
            settings_row("Contacts and permissions", "Senders you know and trust", "system-users-symbolic", lambda: self.contact_list(navigation)))
        settings_group(content, settings_row("Help and about", "Support, privacy and app information",
                       "help-about-symbolic", lambda: self.help_settings(navigation)))
        navigation.window.present()

    def account_settings(self, navigation):
        content = navigation.push("Account")
        content.append(label("Current inbox", "heading"))
        current = next((row for row in self.mailboxes if row["address"] == self.identity), None)
        if current:
            content.append(address_row(current.get("handle") or current["address"]))
        settings_group(content, settings_row("Manage account", "Profile and account details", "avatar-default-symbolic",
                       lambda: self.open_url("https://pigeonpost.dev/account")))
        content.append(button("Sign out", self.sign_out))
        content.append(button("Delete account…", lambda: self.open_url("https://pigeonpost.dev/account#delete-account"), style="destructive-action"))

    def inbox_settings(self, navigation):
        content = navigation.push("Inbox and storage")
        content.append(label("Storage", "title-2"))
        if self.quota:
            used, limit = self.quota.get("used_bytes", 0), self.quota.get("limit_bytes", 0)
            content.append(label(f"{size_text(used)} of {size_text(limit)}", "heading"))
            content.append(Gtk.ProgressBar(fraction=min(1, used / limit) if limit else 0))
            if used >= self.quota.get("warn_at_bytes", float("inf")):
                content.append(label("Your mailbox is nearly full. Remove unneeded messages or manage your plan.", wrap=True))
        else:
            content.append(label("Storage information is unavailable.", wrap=True))
        settings_group(content, settings_row("Archived conversations", "Saved conversations, out of the way", "user-trash-symbolic", lambda: self.archives(navigation)))
        content.append(label("Notifications", "heading"))
        content.append(label("Keep Pigeonpost open to receive desktop notifications. Closing the app stops checking for messages.", wrap=True))

    def help_settings(self, navigation):
        content = navigation.push("Help and about")
        settings_group(content,
            settings_row("Contact support", "Get help with Pigeonpost", "help-browser-symbolic", lambda: self.open_url("https://pigeonpost.dev/app-support.html")),
            settings_row("Privacy policy", "How your information is handled", "changes-prevent-symbolic", lambda: self.open_url("https://pigeonpost.dev/app-privacy.html")),
            settings_row("Terms of service", "Using Pigeonpost", "text-x-generic-symbolic", lambda: self.open_url("https://pigeonpost.dev/app-terms.html")))
        content.append(label("Keyboard shortcuts", "heading"))
        content.append(label("Enter: send · Shift+Enter: new line\nCtrl+N: new conversation · Ctrl+F: find\nCtrl+R: refresh · Ctrl+Q: quit", wrap=True))
        content.append(label(f"Pigeonpost Desktop {VERSION}\nWodo Teknoloji A.Ş.", "dim-label", wrap=True))

    def archives(self, navigation=None):
        window, content = (navigation.window, navigation.push("Archived conversations")) if navigation else self.dialog("Archived conversations")
        content.append(label("Archiving hides a conversation and keeps its messages.", wrap=True))
        for peer in self.archived:
            content.append(button("Restore " + peer, lambda p=peer: (self.archive_peer(p, False), window.close())))
        if not self.archived:
            content.append(label("No archived conversations.", "dim-label"))
        window.present()

    def contact_list(self, navigation=None):
        window, content = (navigation.window, navigation.push("Contacts and permissions")) if navigation else self.dialog("Contacts and permissions")
        content.append(label("A contact name does not grant permission to act. Pigeonpost enforces the permissions below.", wrap=True))
        content.append(button("Add contact", lambda: self.contact_editor("")))
        for contact in self.contacts:
            content.append(button((contact.get("alias") or contact["peer"]) + " · " + contact.get("admission", "allow"),
                                  lambda c=contact: self.contact_editor(c["peer"])))
        window.present()

    def contact_editor(self, peer):
        if peer is None or not self.identity:
            return
        identity = self.identity
        original = next((c for c in self.contacts if c.get("peer") == peer), {})
        window, content = self.dialog("Contact permissions")
        address = Gtk.Entry(placeholder_text="/name/main or /name/*", text=peer)
        address.set_editable(not bool(original))
        alias = Gtk.Entry(placeholder_text="Display name (optional)", text=original.get("alias") or "")
        content.append(address)
        content.append(alias)
        block = Gtk.CheckButton(label="Block this sender", active=original.get("admission") == "block")
        auto = Gtk.CheckButton(label="Allow automatic handling of selected requests", active=original.get("autonomy") == "auto")
        content.append(block)
        content.append(auto)
        content.append(label("Unselected requests require review. High-risk actions always require approval.", wrap=True))
        never = set(self.vocabulary.get("never_auto", []))
        choices = []
        for verb in self.vocabulary.get("grantable", []):
            if not isinstance(verb, str) or verb in never:
                continue
            choice = Gtk.CheckButton(label=verb, active=verb in original.get("allowed_verbs", []))
            choice.set_sensitive(auto.get_active())
            auto.connect("toggled", lambda check, item=choice: item.set_sensitive(check.get_active()))
            choices.append((verb, choice))
            content.append(choice)
        error_label = label("", "error", wrap=True)
        content.append(error_label)
        def save(remove=False):
            target = address.get_text().strip()
            if not valid_peer(target, wildcard=True):
                error_label.set_text("Enter a mailbox address or a namespace wildcard such as /name/*.")
                return
            payload = {"peer": target, "remove": True} if remove else {
                "peer": target, "alias": alias.get_text().strip(), "admission": "block" if block.get_active() else "allow",
                "autonomy": "auto" if auto.get_active() else "review",
                "allowed_verbs": [v for v, check in choices if check.get_active()] if auto.get_active() else [],
            }
            def done(_):
                self.revision += 1
                self.contacts = [c for c in self.contacts if c["peer"] != target]
                if not remove:
                    self.contacts.append(payload)
                self.render_peers()
                window.close()
            self._work(lambda: self.api.call("PUT", "/v1/contacts", identity, data=payload), done,
                       lambda error: error_label.set_text(self.error_text(error)))
        content.append(button("Save contact", save, style="suggested-action"))
        if original:
            content.append(button("Remove contact", lambda: self.confirm("Remove this contact?", "The sender will use your mailbox's default policy.", lambda: save(True), "Remove"), style="destructive-action"))
        window.present()

    def handles(self, navigation=None):
        if navigation:
            window, content = navigation.window, navigation.push("Handles")
        else:
            navigation = SettingsNavigation(self, "Handles")
            window, content = navigation.window, navigation.content
        settings_group(content, settings_row("Get a handle", "Register a name or manage purchases", "list-add-symbolic", lambda: self.handle_registration(navigation, refresh_ownership)))
        content.append(label("Your handles", "title-2"))
        content.append(label("Names belong to your Pigeonpost account across mobile, desktop and web. Expired names need renewal through their original provider.", wrap=True))
        holdings = box()
        content.append(holdings)
        ownership_status = label("Loading account handles…", wrap=True)
        content.append(ownership_status)
        def owned_rows():
            result = self.api.call("GET", "/v1/me/handles", query={"include_inactive": "true"})
            rows = result.get("handles")
            if not isinstance(rows, list):
                raise APIError(0, "invalid_response")
            return rows
        def show_holdings(rows):
            clear(holdings)
            for row in rows:
                name = "/" + row["namespace"].lstrip("/")
                provider = {"apple": "App Store", "google": "Google Play"}.get(row.get("source"), "Pigeonpost")
                active = row.get("active") is True
                holdings.append(label(name, "heading", wrap=True))
                holdings.append(label(f"{'Active' if active else 'Expired'} · {provider}", "dim-label", wrap=True))
                if row.get("expires_at"):
                    date = GLib.DateTime.new_from_unix_local(row["expires_at"]).format("%x")
                    holdings.append(label(f"{'Paid through' if active else 'Expired on'} {date}", "dim-label"))
            ownership_status.set_text("" if rows else "No handles on this Pigeonpost account yet.")
        def refresh_ownership():
            ownership_status.set_text("Loading account handles…")
            self._work(owned_rows, show_holdings,
                       lambda error: ownership_status.set_text("Could not refresh your account handles. Your registrations are saved. Try Refresh again."))
        content.append(button("Refresh account handles", refresh_ownership))
        refresh_ownership()
        window.present()

    def handle_registration(self, navigation, refresh_ownership):
        window, content = navigation.window, navigation.push("Get a handle")
        content.append(label("A name for every inbox", "title-2"))
        content.append(label("Check a name, then register it securely on the Pigeonpost website. Your browser shows the price and payment confirmation.", wrap=True))
        entry = Gtk.Entry(placeholder_text="Choose a handle")
        content.append(entry)
        status = label("", wrap=True)
        content.append(status)
        def check():
            name = entry.get_text().strip().lstrip("/").lower()
            import re
            if not re.fullmatch(r"[a-z0-9][a-z0-9_-]{1,31}", name):
                status.set_text("Enter a handle using 2–32 lowercase letters, numbers, underscores or hyphens.")
                return
            status.set_text("Checking availability…")
            def checked(result):
                status.set_text(f"/{name} is available. Continue on the website to register it." if result.get("available") else f"/{name} is not available. Try another name.")
            self._work(lambda: self.api.call("GET", "/v1/handles/" + quote(name, safe="") + "/availability"), checked,
                       lambda error: status.set_text(self.error_text(error)))
        content.append(button("Check availability", check))
        entry.connect("activate", lambda _: check())
        content.append(button("Register or manage handles on the website", lambda: self.open_url("https://pigeonpost.dev/account"), style="suggested-action"))
        content.append(label("Your mailboxes", "heading"))
        owned = box()
        content.append(owned)
        def show(rows):
            clear(owned)
            for row in rows:
                owned.append(label(row.get("handle") or row.get("label") or row["address"], wrap=True))
            if not rows:
                owned.append(label("No mailboxes yet.", "dim-label"))
        show(self.mailboxes)
        def refreshed(rows):
            show(rows)
            refresh_ownership()
            # Refresh the selector without changing the active mailbox or clearing drafts.
            current = self.identity
            self.mailboxes = rows
            self.rendering = True
            self.mailbox_model.splice(0, self.mailbox_model.get_n_items(), [r.get("handle") or r.get("label") or r["address"] for r in rows])
            index = next((i for i, row in enumerate(rows) if row["address"] == current), 0)
            self.mailbox_picker.set_selected(index)
            self.rendering = False
            self.create_button.set_visible(not rows)
            self.mailbox_picker.set_visible(bool(rows))
            if rows:
                self.select_mailbox()
        content.append(button("Refresh after registration", lambda: self._work(self.api.identities, refreshed)))
        window.present()

    def clear_account(self):
        for dialog in list(self.dialogs):
            dialog.close()
        self.identity = self.peer = self.subject = None
        self.messages, self.contacts, self.threads, self.archived, self.mailboxes = [], [], [], [], []
        self.drafts, self.files, self.quota, self.vocabulary = {}, {}, {}, {}
        self.known_ids = None
        self.polling = self.sending = self.acking = False
        self.restore_draft()
        self.render_all()

    def sign_out(self):
        def signout():
            self.cancel.set()
            self.generation += 1
            self.clear_account()
            self.show_signin("Signing out…")
            self.login_button.set_sensitive(False)
            self._work(self.session.sign_out, lambda _: self.show_signin("You have signed out."),
                       lambda error: self.show_signin("Could not clear your desktop keyring. Unlock it and sign out again."))
        self.confirm("Sign out?", "Unsent drafts and staged attachments on this device will be discarded.", signout, "Sign out")

    def on_close(self, *_):
        self.closed = True
        self.cancel.set()
        self.generation += 1
        self.clear_account()
        return False


class Application(Adw.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID, flags=Gio.ApplicationFlags.DEFAULT_FLAGS)
        self.window = None

    def do_activate(self):
        if not self.window or self.window.closed:
            self.window = Window(self)
        self.window.present()


def main():
    import sys
    if "--version" in sys.argv:
        print("Pigeonpost Desktop " + VERSION)
        return 0
    return Application().run(sys.argv)
