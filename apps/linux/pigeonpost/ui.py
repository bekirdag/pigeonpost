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
from .calls import CallAlerts, valid_room
from .model import (contact_for, conversation_address_input, conversations, display_name, message_text, normalize, safe_filename,
                    size_text, subjects, target_thread, timestamp, valid_peer, unread_count)
from .vault import Vault
from .previews import ThumbnailCache, supports as preview_supported
from .theme import CHOICES, Design, Preferences, apply_appearance
from .viewer import AttachmentViewer
from .widgets import (SettingsNavigation, address_row, box, button, clear, label, scroll, settings_group,  # noqa: F401
                      settings_row)
from .avatars import AvatarCache
from .features import AVATARS, CALL_HISTORY, DISAPPEARING, GROUPS, MESSAGE_FILTERING, live_messages, missed_call_lines, timer_label
from .binding import Lease, Retired
from .protected import open_store
from .window_calendar import CalendarMixin
from .window_calls import CallsMixin
from .window_groups import GroupsMixin
from .window_messages import MessagesMixin
from .window_notes import NotesMixin
from .window_orgs import OrgsMixin
from .window_pairing import PairingMixin
from .window_privacy import PrivacyMixin

WEB_INBOX = "https://inbox.pigeonpost.dev/"


class CallNotifier:
    """Incoming calls as urgent desktop notifications with Answer and Decline buttons."""

    def __init__(self, app):
        self.app = app

    def show(self, room, title, body):
        notification = Gio.Notification.new(title)
        notification.set_body(body)  # plain text, already stripped of markup characters
        notification.set_priority(Gio.NotificationPriority.URGENT)
        if hasattr(notification, "set_category"):
            notification.set_category("call.incoming")
        target = GLib.Variant("s", room)
        notification.add_button_with_target("Answer", "app.call-answer", target)
        notification.add_button_with_target("Decline", "app.call-decline", target)
        self.app.send_notification("call-" + room, notification)

    def withdraw(self, room):
        self.app.withdraw_notification("call-" + room)


def on_main(fn, *args):
    GLib.idle_add(lambda: (fn(*args), False)[1])


def main_later(seconds, fn):
    GLib.timeout_add_seconds(seconds, lambda: (fn(), False)[1])


class Window(MessagesMixin, NotesMixin, PairingMixin, OrgsMixin, CalendarMixin, PrivacyMixin, CallsMixin, GroupsMixin, Adw.ApplicationWindow):
    def __init__(self, app, session=None, api=None, restore=True, preferences=None, calls=None, protected=None):
        super().__init__(application=app, title="Pigeonpost", default_width=1180, default_height=780)
        self.set_size_request(800, 540)
        self.preferences = preferences or getattr(app, "preferences", None) or Preferences()
        apply_appearance(self.preferences.appearance)
        if not getattr(app, "design", None):
            app.design = Design(self.get_display())
        self.session = session or Session(Vault())
        self.api = api or Postbox(self.session)
        # Injected APIs (tests) get no live calls stream unless one is passed explicitly.
        self.calls = calls if calls is not None or api is not None else CallAlerts(
            self.session, CallNotifier(app), self.open_url, dispatch=on_main, later=main_later, name_for=self.caller_name)
        self.generation, self.revision, self.closed = 0, 0, False
        # The accepted login and the selected mailbox (binding.Lease). Protected work keeps its own
        # lease and is published only while that login is still current, whatever is selected.
        self.lease = self.mailbox_lease = None
        self.protected = protected if protected is not None else open_store()
        self.cancel = threading.Event()
        self.mailboxes, self.messages, self.threads, self.contacts, self.archived = [], [], [], [], []
        self.vocabulary, self.quota = {}, {}
        self.identity = self.peer = self.subject = None
        # Phase 1b and 3: what the postbox enables (GET /v1/capabilities), and the state behind it.
        self.features = frozenset()
        self.capabilities_at = 0
        self.side_data_at = 0
        self.filtered = []
        self.own_avatar_texture = None
        self.avatars = AvatarCache(self)
        self.reset_calls()
        self.reset_groups()
        self.reset_calendar()
        self.reset_orgs()
        self.reset_pairing()
        self.reset_messages()
        self.reset_notes(keep_login=False)
        self.drafts, self.files = {}, {}
        self.previews = ThumbnailCache()
        self.polling = self.sending = self.rendering = self.acking = False
        self.staging = {}
        self.known_ids, self.dialogs = None, []
        self.notification_target = None
        self.message_limit = 200
        self.pool = threading.BoundedSemaphore(8)
        self.overlay = Adw.ToastOverlay()
        self.root = box(0)
        self.root.add_css_class("pp-root")
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

    def api_for(self, lease):
        """The postbox for work that must stay with `lease`'s login (injected test APIs as is)."""
        bound = getattr(self.api, "bound", None)
        return bound(lease) if lease is not None and callable(bound) else self.api

    def bound_work(self, lease, operation, done=None, failed=None):
        """Like _work, but tied to a login rather than to the current selection: the result is
        delivered while `lease` is still current, even after the person moved to another mailbox
        or conversation, and dropped (never shown in a newer login) once it is retired."""
        def deliver(value, error=False):
            if self.closed or lease is None or not lease.current:
                return False
            if error:
                if not isinstance(value, Retired):
                    (failed or self.error)(value)
            elif done:
                done(value)
            return False

        def run():
            with self.pool:
                if self.closed or lease is None or not lease.current:
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
        if isinstance(error, Retired):
            return  # work of an earlier sign-in: nothing to show in this one
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
        if self.lease is None or not self.lease.current:
            if self.lease is not None:
                self.lease.retire()
            self.lease = Lease(self.session)
        self.mailboxes = rows
        self.account_button.set_sensitive(True)
        self.refresh_button.set_sensitive(True)
        self.stack.set_visible_child_name("inbox")
        self.rendering = True
        self.mailbox_model.splice(0, self.mailbox_model.get_n_items(),
                                 [display_name(r["handle"]) if r.get("handle") else r.get("label") or display_name(r["address"]) for r in rows])
        self.rendering = False
        self.create_button.set_visible(not rows)
        if self.calls and rows:
            self.calls.start()
        self.mailbox_picker.set_visible(bool(rows))
        self.post_address.set_visible(bool(rows))
        if rows:
            index = next((i for i, row in enumerate(rows) if self.notification_target and row["address"] == self.notification_target[0]), 0)
            self.mailbox_picker.set_selected(index)
            self.select_mailbox()
            self.load_capabilities()
        else:
            self.empty_thread("Create your inbox to start messaging.")

    def load_capabilities(self):
        """Every Phase 1b and 3 feature stays invisible until the postbox says it is on, so this app
        degrades to 1.0.8 behaviour against an older server."""
        self.capabilities_at = GLib.get_monotonic_time()
        def read():
            if hasattr(self.api, "capabilities"):
                return self.api.capabilities()
            features = self.api.call("GET", "/v1/capabilities").get("features")
            return frozenset(str(f) for f in features) if isinstance(features, list) else frozenset()
        self._work(read, self.apply_features, lambda _: None)

    def apply_features(self, features):
        changed = frozenset(features) != self.features
        self.features = frozenset(features)
        if changed or not self.avatars.enabled == (AVATARS in self.features):
            self.avatars.reset(AVATARS in self.features)
        self.sync_side_nav()
        self.start_groups()
        self.load_calls(quiet=True)
        self.load_filtered()
        self.calendar_features_changed()
        self.orgs_features_changed()
        self.notes_features_changed()
        if changed:
            self.render_all()

    def refresh_side_data(self):
        """Call history, the Filtered list and expiry: slower than the inbox poll."""
        self.side_data_at = GLib.get_monotonic_time()
        self.load_calls(quiet=True)
        self.load_filtered()

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
        sidebar_ground = box(0)
        sidebar_ground.add_css_class("pp-sidebar")
        sidebar_ground.append(sidebar)
        self.mailbox_model = Gtk.StringList.new([])
        self.mailbox_picker = Gtk.DropDown(model=self.mailbox_model)
        self.mailbox_picker.add_css_class("pp-mailbox")
        self.mailbox_picker.set_tooltip_text("Active mailbox")
        self.mailbox_picker.connect("notify::selected", lambda *_: self.select_mailbox())
        sidebar.append(self.mailbox_picker)
        self.post_address = box(0)
        sidebar.append(self.post_address)
        self.create_button = button("Create my inbox", self.create_inbox, style="suggested-action")
        sidebar.append(self.create_button)
        # Calls (SDS §12.1): under the mailbox switcher, with the unseen missed-call count.
        self.calls_button = Gtk.Button()
        calls_content = box(8, horizontal=True)
        calls_content.append(Gtk.Image(icon_name="call-start-symbolic"))
        self.calls_button_label = label("Calls")
        calls_content.append(self.calls_button_label)
        self.calls_button.set_child(calls_content)
        self.calls_button.add_css_class("flat")
        self.calls_button.set_tooltip_text("Call history")
        self.calls_button.connect("clicked", lambda _: self.show_page("calls"))
        self.calls_button.set_visible(False)
        sidebar.append(self.calls_button)
        self.calendar_button = self.side_button("Calendar", "x-office-calendar-symbolic", "Calendar", lambda: self.show_page("calendar"))
        sidebar.append(self.calendar_button)
        self.orgs_button = self.side_button("Organizations", "system-users-symbolic", "Organization rooms", lambda: self.show_page("orgs"))
        sidebar.append(self.orgs_button)
        self.notes_button = self.side_button("Meeting notes", "x-office-document-symbolic", "Meeting notes", lambda: self.show_page("notes"))
        sidebar.append(self.notes_button)
        self.peer_search = Gtk.SearchEntry(placeholder_text="Search conversations")
        self.peer_search.connect("search-changed", lambda _: self.render_peers())
        sidebar.append(self.peer_search)
        compose_row = box(6, horizontal=True)
        compose_row.set_homogeneous(True)
        new_conversation = Gtk.Button()
        conversation_content = box(6, horizontal=True)
        conversation_content.set_halign(Gtk.Align.CENTER)
        conversation_content.append(Gtk.Image(icon_name="list-add-symbolic"))
        conversation_content.append(label("New conversation"))
        new_conversation.set_child(conversation_content)
        new_conversation.add_css_class("pp-pill-button")
        new_conversation.set_tooltip_text("New conversation")
        new_conversation.update_property([Gtk.AccessibleProperty.LABEL], ["New conversation"])
        new_conversation.connect("clicked", lambda _: self.new_conversation())
        compose_row.append(new_conversation)
        # New group (SDS §4.2): a labelled button beside compose, shown only while groups are on.
        self.new_group_button = Gtk.Button()
        group_content = box(6, horizontal=True)
        group_content.set_halign(Gtk.Align.CENTER)
        group_content.append(Gtk.Image(icon_name="system-users-symbolic"))
        group_content.append(label("New group"))
        self.new_group_button.set_child(group_content)
        self.new_group_button.add_css_class("pp-pill-button")
        self.new_group_button.set_tooltip_text("New group")
        self.new_group_button.update_property([Gtk.AccessibleProperty.LABEL], ["New group"])
        self.new_group_button.connect("clicked", lambda _: self.new_group())
        self.new_group_button.set_visible(False)
        compose_row.append(self.new_group_button)
        sidebar.append(compose_row)
        self.peer_list = Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
        self.peer_list.add_css_class("navigation-sidebar")
        self.peer_list.add_css_class("pp-list")
        self.peer_list.connect("row-selected", self.peer_selected)
        sidebar.append(scroll(self.peer_list))
        self.connection_status = label("", "pp-status", wrap=True)
        sidebar.append(self.connection_status)
        self.recovery_button = self.side_button("Unconfirmed", "dialog-warning-symbolic", "Review unconfirmed messages",
                                                lambda: self.review_deliveries())
        sidebar.append(self.recovery_button)
        sidebar.append(button("Handles", self.handles, style="pp-pill-button"))
        subpanel = box(10, margin=12)
        subpanel.set_size_request(165, -1)
        subpanel_ground = box(0)
        subpanel_ground.add_css_class("pp-column")
        subpanel_ground.append(subpanel)
        subhead = box(horizontal=True)
        heading = label("Subjects", "pp-overline")
        heading.set_hexpand(True)
        subhead.append(heading)
        subhead.append(button("New subject", self.new_subject, "list-add-symbolic", "flat"))
        subpanel.append(subhead)
        self.subject_list = Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
        self.subject_list.add_css_class("navigation-sidebar")
        self.subject_list.add_css_class("pp-list")
        self.subject_list.connect("row-selected", self.subject_selected)
        subpanel.append(scroll(self.subject_list))
        self.subject_delete = button("Delete subject", self.delete_subject, "user-trash-symbolic", "flat")
        subpanel.append(self.subject_delete)
        detail = box(0)
        detail.set_size_request(350, -1)
        detail.add_css_class("pp-wash")
        toolbar = box(4, horizontal=True, margin=12)
        toolbar.add_css_class("pp-toolbar")
        self.peer_avatar = box(0)
        toolbar.append(self.peer_avatar)
        self.peer_title = label("Your inbox", "pp-title")
        self.peer_title.set_hexpand(True)
        toolbar.append(self.peer_title)
        self.timer_badge = label("", "pp-meta")
        self.timer_badge.set_visible(False)
        toolbar.append(self.timer_badge)
        # Calls run in the web inbox: no Linux WebKitGTK build ships WebRTC (see README).
        self.voice_call_button = button("Voice call", lambda: self.call(False), "call-start-symbolic", "flat")
        self.video_call_button = button("Video call", lambda: self.call(True), "camera-video-symbolic", "flat")
        toolbar.append(self.voice_call_button)
        toolbar.append(self.video_call_button)
        toolbar.append(button("Contact details", lambda: self.contact_editor(self.peer), "avatar-default-symbolic", "flat"))
        toolbar.append(button("Archive conversation", self.archive_peer, "folder-download-symbolic", "flat"))
        # Conversation menu: Block (SDS §15.2) and Disappearing messages (§17.1).
        conversation_actions = Gio.SimpleActionGroup()
        for name, handler in (("block", lambda: self.ask_block(self.peer, self.peer_title.get_text())),
                              ("timer", self.choose_direct_timer)):
            action = Gio.SimpleAction.new(name, None)
            action.connect("activate", lambda *_, run=handler: run())
            conversation_actions.add_action(action)
        self.insert_action_group("conversation", conversation_actions)
        self.conversation_actions = conversation_actions
        self.conversation_menu = Gtk.MenuButton(icon_name="view-more-symbolic")
        self.conversation_menu.add_css_class("flat")
        self.conversation_menu.set_tooltip_text("Conversation menu")
        self.conversation_menu.update_property([Gtk.AccessibleProperty.LABEL], ["Conversation menu"])
        toolbar.append(self.conversation_menu)
        detail.append(toolbar)
        self.message_search = Gtk.SearchEntry(placeholder_text="Search this conversation")
        self.message_search.set_margin_start(16)
        self.message_search.set_margin_end(16)
        self.message_search.set_margin_top(10)
        self.message_search.connect("search-changed", lambda _: self.render_messages())
        detail.append(self.message_search)
        self.message_list = box(14, margin=20)
        self.message_scroll = scroll(self.message_list)
        detail.append(self.message_scroll)
        self.file_chips = box(horizontal=True, margin=8)
        detail.append(self.file_chips)
        self.blocked_banner = box(8, horizontal=True, margin=12)
        self.blocked_banner.add_css_class("pp-banner")
        banner_text = label("You blocked this person", wrap=True)
        banner_text.set_hexpand(True)
        self.blocked_banner.append(banner_text)
        self.blocked_banner.append(button("Unblock", lambda: self.unblock_peer()))
        self.blocked_banner.set_visible(False)
        detail.append(self.blocked_banner)
        self.mode_holder = box(0)
        self.mode_holder.set_visible(False)
        detail.append(self.mode_holder)
        composer = box(6, horizontal=True, margin=16)
        composer.set_margin_top(4)
        composer.add_css_class("pp-composer")
        self.attach_button = button("Attach a file", self.pick_file, "mail-attachment-symbolic", "flat")
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
        self.send_button.set_valign(Gtk.Align.END)
        self.attach_button.set_valign(Gtk.Align.END)
        composer.append(self.send_button)
        detail.append(composer)
        drop = Gtk.DropTarget.new(Gio.File, Gdk.DragAction.COPY)
        drop.connect("drop", lambda _, file, x, y: self.stage_file(file))
        detail.add_controller(drop)
        right = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL, position=200)
        right.set_start_child(subpanel_ground)
        right.set_end_child(detail)
        right.set_resize_start_child(False)
        right.set_shrink_start_child(False)
        right.set_shrink_end_child(False)
        # The right side shows a conversation, a group, call history or the Filtered list.
        self.main_stack = Gtk.Stack(transition_type=Gtk.StackTransitionType.CROSSFADE, hexpand=True)
        self.main_stack.add_named(right, "conversation")
        self.main_stack.add_named(self.build_calls_page(), "calls")
        self.main_stack.add_named(self.build_filtered_page(), "filtered")
        self.main_stack.add_named(self.build_group_page(), "group")
        self.main_stack.add_named(self.build_calendar_page(), "calendar")
        self.main_stack.add_named(self.build_orgs_page(), "orgs")
        self.main_stack.add_named(self.build_notes_page(), "notes")
        self.main_stack.set_visible_child_name("conversation")
        panes = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL, position=270)
        panes.set_start_child(sidebar_ground)
        panes.set_end_child(self.main_stack)
        panes.set_resize_start_child(False)
        panes.set_shrink_start_child(False)
        panes.set_shrink_end_child(False)
        self.stack.add_named(panes, "inbox")

    def side_button(self, text, icon, tooltip, clicked):
        """A flat sidebar entry like Calls; hidden until its feature is on."""
        widget = Gtk.Button()
        content = box(8, horizontal=True)
        content.append(Gtk.Image(icon_name=icon))
        widget.label = label(text)
        content.append(widget.label)
        widget.set_child(content)
        widget.add_css_class("flat")
        widget.set_tooltip_text(tooltip)
        widget.update_property([Gtk.AccessibleProperty.LABEL], [tooltip])
        widget.connect("clicked", lambda _: clicked())
        widget.set_visible(False)
        return widget

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
        if self.mailbox_lease is not None:
            self.mailbox_lease.retire()
        self.mailbox_lease = self.lease.child(row["address"]) if self.lease is not None else None
        self.messages, self.contacts, self.threads, self.archived = [], [], [], []
        self.quota, self.vocabulary, self.known_ids = {}, {}, None
        self.polling = self.sending = self.acking = False
        self.filtered = []
        self.reset_calls()
        self.reset_groups()
        self.reset_calendar()
        self.reset_orgs()
        self.reset_notes()
        self.avatars.reset(AVATARS in self.features)
        if self.page() != "conversation":
            self.main_stack.set_visible_child_name("conversation")
        self.title.set_subtitle(row.get("handle") or row.get("label") or row["address"])
        self.restore_draft()
        self.render_all()
        self.refresh()
        self.load_saved_work()
        if self.features:
            self.sync_side_nav()
            self.start_groups()
            self.refresh_side_data()

    def tick(self):
        if self.closed:
            return False
        if self.identity and not self.polling:
            self.refresh(wait=25)
        if self.identity:
            now = GLib.get_monotonic_time()
            if self.mailboxes and now - self.capabilities_at > 600 * 1_000_000:
                self.load_capabilities()
            elif self.features and now - self.side_data_at > 30 * 1_000_000:
                self.refresh_side_data()
            self.groups_tick()
            self.calendar_tick()
            self.orgs_tick()
            if any(m.get("expires_at") for m in self.messages):
                live = live_messages(self.messages)
                if len(live) != len(self.messages):
                    self.messages = live  # disappearing messages leave by this device's clock too
                    self.render_all()
        return True

    def refresh(self, wait=0):
        if not self.identity or self.polling:
            return
        identity, revision = self.identity, self.revision
        self.polling = True
        if self.known_ids is None:
            self.connection_status.set_text("Loading inbox…")
        def read():
            messages = self.read_inbox(identity, wait)
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
            messages = live_messages(messages)
            ids = {m["message_id"] for m in messages}
            if self.known_ids is not None:
                new = [m for m in messages if m["message_id"] not in self.known_ids and m.get("direction") != "out" and not m.get("read")]
                for message in new:
                    peer = next((p for p, rows in conversations(messages).items() if any(m["message_id"] == message["message_id"] for m in rows)), None)
                    visible = (self.is_active() and self.peer == peer and self.page() == "conversation"
                               and (self.subject is None or (message.get("thread_id") or "") == self.subject)
                               and self.message_search.get_text().casefold() in message_text(message).casefold())
                    if not visible:
                        notification = Gio.Notification.new("New Pigeonpost message")
                        notification.set_body("Open Pigeonpost to read your message.")
                        notification.set_default_action_and_target("app.open-message", GLib.Variant("(ss)", (identity, message["message_id"])))
                        self.get_application().send_notification("new-mail", notification)
            changed = (messages, threads, contacts.get("contacts", []), archived) != (self.messages, self.threads, self.contacts, self.archived)
            self.messages, self.threads = messages, threads
            self.contacts, self.vocabulary = contacts.get("contacts", []), contacts.get("vocabulary", {})
            self.archived, self.quota, self.known_ids = archived, quota, ids
            self.apply_notification_target()
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
        self.update_unread_indicator()
        self.render_pending_chrome()

    def update_unread_indicator(self):
        count = unread_count(self.messages)
        self.set_title(f"Pigeonpost — {count} unread" if count else "Pigeonpost")
        # Ubuntu Dock, Unity and compatible taskbars consume this desktop-entry badge.
        # Other shells still show thread counts and the count in the window title.
        connection = self.get_application().get_dbus_connection()
        if connection:
            try:
                connection.emit_signal(None, "/dev/pigeonpost/Desktop", "com.canonical.Unity.LauncherEntry", "Update",
                    GLib.Variant("(sa{sv})", ("application://dev.pigeonpost.Desktop.desktop",
                        {"count": GLib.Variant("x", count), "count-visible": GLib.Variant("b", count > 0)})))
            except GLib.Error:
                pass

    def open_notification(self, identity, message_id):
        if not self.mailboxes:
            self.notification_target = identity, message_id
            return
        index = next((i for i, row in enumerate(self.mailboxes) if row["address"] == identity), None)
        if index is None:
            return
        self.notification_target = identity, message_id
        if identity != self.identity:
            self.mailbox_picker.set_selected(index)
        else:
            self.apply_notification_target()
            self.render_all()
            self.ack_visible()
        self.present()

    def apply_notification_target(self):
        if not self.notification_target or self.notification_target[0] != self.identity:
            return
        message_id = self.notification_target[1]
        for peer, messages in conversations(self.messages).items():
            message = next((m for m in messages if m["message_id"] == message_id), None)
            if message:
                self.peer, self.subject = peer, message.get("thread_id") or ""
                self.message_search.set_text("")
                self.restore_draft()
                self.notification_target = None
                return

    def render_peers(self):
        self.rendering = True
        clear(self.peer_list)
        search = self.peer_search.get_text().casefold()
        self.render_group_rows(search)
        rows = conversations(self.messages, self.contacts)
        if self.peer:
            rows.setdefault(self.peer, [])
        for peer, messages in list(rows.items())[:1000]:
            contact = contact_for(peer, self.contacts)
            name = contact.get("alias") or display_name(peer)
            if peer in self.archived or search not in (name + " " + peer).casefold():
                continue
            row = Gtk.ListBoxRow()
            row.peer, row.kind = peer, "peer"
            content = box(10, horizontal=True, margin=9)
            content.append(self.avatars.widget(peer, name, 32))
            text = box(3)
            unread = unread_count(messages)
            waiting = any(a.peer == peer for a in self.pending.get(self.identity) or [])
            text.append(label(name + (f"  • {unread}" if unread else "") + ("  ⚠ unconfirmed" if waiting else ""), "pp-peer"))
            text.append(label(message_text(messages[-1]).replace("\n", " ")[:120] if messages else "Start a conversation", "pp-preview"))
            content.append(text)
            row.set_child(content)
            self.peer_list.append(row)
            if peer == self.peer and self.page() == "conversation":
                self.peer_list.select_row(row)
        # Filtered (SDS §13.4): at the bottom of the conversation list, only when something is.
        filtered = self.filtered_groups() if MESSAGE_FILTERING in self.features else []
        if filtered and not search:
            row = Gtk.ListBoxRow()
            row.kind, row.peer = "filtered", None
            content = box(10, horizontal=True, margin=9)
            content.append(Gtk.Image(icon_name="mail-mark-junk-symbolic"))
            content.append(label(f"Filtered ({len(filtered)})", "pp-peer"))
            row.set_child(content)
            row.update_property([Gtk.AccessibleProperty.LABEL],
                                [f"Filtered, {len(filtered)} {'conversation' if len(filtered) == 1 else 'conversations'}"])
            self.peer_list.append(row)
            if self.page() == "filtered":
                self.peer_list.select_row(row)
        self.rendering = False

    def avatars_changed(self):
        """A picture arrived; widgets made before it are updated in place by the cache."""

    def peer_selected(self, _, row):
        if self.rendering or not row:
            return
        kind = getattr(row, "kind", "peer")
        if kind == "filtered":
            self.peer = self.subject = None
            self.group_open = None
            self.show_page("filtered")
            return
        if kind in ("group", "invitations"):
            self.group_row_selected(row)
            return
        if kind != "peer":
            return
        self.group_open = None
        if self.page() != "conversation":
            self.main_stack.set_visible_child_name("conversation")
            self.sync_side_nav()
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
        messages = conversations(self.messages).get(self.peer, [])
        for item in rows:
            row = Gtk.ListBoxRow()
            row.subject = item["thread_id"]
            title = item.get("title") or ("General" if item.get("is_default") else "Untitled")
            unread = unread_count([m for m in messages if row.subject is None or (m.get("thread_id") or "") == row.subject])
            row.set_child(label(title + (f"  • {unread} unread" if unread else ""), "heading" if unread else None))
            row.get_child().add_css_class("pp-subject")
            row.get_child().set_margin_top(10)
            row.get_child().set_margin_bottom(10)
            row.get_child().set_margin_start(10)
            row.get_child().set_margin_end(10)
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
        self.message_list.append(label(text, "pp-status", wrap=True))
        self.update_send()

    def render_conversation_chrome(self):
        """Header picture, timer badge, menu and the blocked banner of a one-to-one conversation."""
        clear(self.peer_avatar)
        self.timer_badge.set_visible(False)
        self.blocked_banner.set_visible(False)
        menu = Gio.Menu()
        if self.peer:
            name = self.peer_title.get_text()
            self.peer_avatar.append(self.avatars.widget(self.peer, name, 28))
            secs = self.direct_timer()
            if secs:
                self.timer_badge.set_text("⏱ " + timer_label(secs))
                self.timer_badge.set_tooltip_text(f"Messages disappear after {timer_label(secs)}")
                self.timer_badge.set_visible(True)
            blocked = self.blocked(self.peer)
            self.blocked_banner.set_visible(blocked)
            if DISAPPEARING in self.features and self.direct_timer_thread():
                menu.append("Disappearing messages…", "conversation.timer")
            if not blocked:
                menu.append("Block…", "conversation.block")
        self.conversation_menu.set_menu_model(menu)
        self.conversation_menu.set_visible(bool(self.peer))

    def unblock_peer(self):
        """The banner's Unblock: removes the exact block, or the namespace rule that covers it."""
        peer = self.peer
        if not peer:
            return
        rule = next((c["peer"] for c in self.contacts if c.get("admission") == "block" and c.get("peer") == peer), None)
        if rule is None:
            rule = next((c["peer"] for c in self.contacts if c.get("admission") == "block" and c.get("peer", "").endswith("/*")
                         and peer.startswith(c["peer"][:-1])), None)
        if rule:
            self.unblock(rule)

    def render_messages(self):
        if not self.peer:
            self.peer_title.set_text("Your inbox")
            self.render_conversation_chrome()
            self.empty_thread("Choose a conversation, or start a new one.")
            return
        self.peer_title.set_text(contact_for(self.peer, self.contacts).get("alias") or display_name(self.peer))
        self.render_conversation_chrome()
        adj = self.message_scroll.get_vadjustment()
        bottom = adj.get_value() + adj.get_page_size() >= adj.get_upper() - 80
        clear(self.message_list)
        self.message_widgets = {}
        query = self.message_search.get_text().casefold()
        rows = [m for m in self.shown_messages() if query in message_text(m).casefold()]
        # Missed calls read as small lines in the conversation they belong to (SDS §12.1).
        lines = []
        if CALL_HISTORY in self.features and not query and (self.subject is None or self.subject == (self.direct_timer_thread() or {}).get("thread_id")):
            keys = {self.peer} | {m.get("from") for m in rows if m.get("direction") != "out" and m.get("from")}
            lines = missed_call_lines(self.call_state["entries"], keys)
        if len(rows) > self.message_limit:
            self.message_list.append(button(f"Load earlier messages ({len(rows) - self.message_limit})", self.load_earlier, style="pp-pill-button"))
        if not rows and not lines:
            secs = self.direct_timer()
            self.message_list.append(label("No matching messages." if query else f"Messages disappear after {timer_label(secs)}." if secs
                                           else "Say hello to start this conversation.", "pp-status", wrap=True))
        shown = rows[-self.message_limit:]
        first_at = timestamp(shown[0]) if len(rows) > self.message_limit and shown else 0
        first_at = first_at / 1000 if first_at > 10**11 else first_at
        pending = [line for line in lines if line["at"] >= first_at]
        for message in shown:
            when_s = timestamp(message)
            when_s = when_s / 1000 if when_s > 10**11 else when_s
            while pending and pending[0]["at"] <= when_s:
                self.message_list.append(self.call_line(pending.pop(0)))
            outgoing = message.get("direction") == "out"
            holder = box(4)
            # (holders are not focusable: their buttons and selectable text are)
            bubble = box(6)
            bubble.add_css_class("pp-bubble")
            if outgoing:
                bubble.add_css_class("pp-mine")
            bubble.insert_action_group("message", self.message_actions(message, outgoing))
            holder.insert_action_group("message", self.message_actions(message, outgoing))
            inner = box(8, margin=14)
            inner.set_margin_start(16)
            inner.set_margin_end(16)
            who = "You" if outgoing else self.peer
            when = timestamp(message)
            try:
                suffix = datetime.datetime.fromtimestamp(when / 1000 if when > 10**11 else when).strftime("%d %b · %H:%M") if when else ""
            except (ValueError, OSError, OverflowError):
                suffix = ""
            inner.append(self.message_meta(message, outgoing, who, suffix))
            quoted = self.reply_header(message)
            if quoted is not None:
                inner.append(quoted)
            if message.get("verb"):
                state = "Review required" if message.get("autonomy") != "auto" else "Automatic handling allowed"
                verb = label(state + " · " + str(message["verb"]), "pp-verb", wrap=True)
                verb.add_css_class("pp-auto" if message.get("autonomy") == "auto" else "pp-held")
                verb.set_halign(Gtk.Align.START)
                inner.append(verb)
            inner.append(self.message_body(message))
            for attachment in message.get("attachments") or []:
                identity = self.identity
                preview = self.attachment_preview(
                    f"{identity}:{attachment.get('id')}", attachment.get("filename", "attachment"), attachment.get("media_type", ""),
                    lambda a=attachment, owner=identity: self.api.download(owner, a["id"]))
                if preview:
                    opener = Gtk.Button(child=preview)
                    opener.add_css_class("flat")
                    opener.add_css_class("pp-preview-button")
                    opener.set_halign(Gtk.Align.START)
                    name = safe_filename(attachment.get("filename", "attachment"))
                    opener.set_tooltip_text("Open " + name)
                    opener.update_property([Gtk.AccessibleProperty.LABEL], ["Open " + name])
                    opener.connect("clicked", lambda _, a=attachment: self.open_attachment(a))
                    inner.append(opener)
                save = button("Save " + safe_filename(attachment.get("filename", "attachment")) + " · " + size_text(attachment.get("bytes", 0)),
                              lambda a=attachment: self.save_attachment(a), style="pp-file")
                save.set_halign(Gtk.Align.START)
                inner.append(save)
            bubble.append(inner)
            holder.append(bubble)
            holder.append(self.bubble_actions(message, outgoing))
            self.context_menu(bubble, self.message_menu(message, outgoing))
            holder.add_css_class("pp-message")
            holder.message = message
            holder.set_margin_start(64 if outgoing else 0)
            holder.set_margin_end(0 if outgoing else 64)
            for reference in (message.get("msg_ref"), message.get("message_id")):
                if reference:
                    self.message_widgets[reference] = holder
            self.message_list.append(holder)
        for line in pending:
            self.message_list.append(self.call_line(line))
        for attempt in self.pending_here():
            self.message_list.append(self.pending_bubble(attempt))
        self.render_mode()
        self.update_send()
        if bottom and not query:
            GLib.idle_add(lambda: adj.set_value(max(0, adj.get_upper() - adj.get_page_size())))

    def message_body(self, message):
        """Plain text (never markup) that can be partly selected and copied.

        The label's own menu keeps Copy for the selection and adds the message's actions, so the
        selection never hides them."""
        text = label(message_text(message), "pp-body", wrap=True)
        text.set_selectable(True)
        text.set_use_markup(False)
        text.set_extra_menu(self.message_menu(message, message.get("direction") == "out"))
        return text

    def call_line(self, line):
        widget = label(line["text"], "pp-system")
        widget.set_halign(Gtk.Align.CENTER)
        widget.call_line = line
        return widget

    def block_sender(self, message):
        peer = self.peer or message.get("peer_handle") or message.get("from")
        self.ask_block(peer, contact_for(peer, self.contacts).get("alias") or display_name(peer), message_id=message.get("message_id"))

    def message_actions(self, message, outgoing):
        group = Gio.SimpleActionGroup()
        handlers = {"copy": lambda: self.get_clipboard().set(message_text(message)),
                    "delete": lambda: self.delete_message(message)}
        if not outgoing:
            handlers["report"] = lambda: self.report_message(message)
            handlers["block"] = lambda: self.block_sender(message)
        self.extra_actions(message, outgoing, handlers)
        for name, handler in handlers.items():
            action = Gio.SimpleAction.new(name, None)
            action.connect("activate", lambda *_, run=handler: run())
            group.add_action(action)
        return group

    def load_earlier(self):
        self.message_limit += 200
        self.render_messages()

    def ack_visible(self):
        if not self.identity or not self.peer or not self.is_active() or self.page() != "conversation":
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
            return self.ack_ids(identity, ids)
        def done(acked):
            self.acking = False
            for m in self.messages:
                if m["message_id"] in acked:
                    m["read"] = True
            self.render_peers()
            self.render_subjects()
            self.update_unread_indicator()
        self._work(ack, done, lambda _: setattr(self, "acking", False))

    def draft_key(self):
        return self.identity, self.peer, self.subject

    def draft_changed(self, buffer):
        if not self.rendering:
            key = self.draft_key()
            self.drafts[key] = buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)
            mode = self.compose_modes.get(key)
            if not mode or mode["kind"] != "edit":
                self.schedule_draft_save(key)  # an edit's text is not a draft; the saved draft waits
        self.update_send()

    def restore_draft(self):
        self.rendering = True
        self.composer.get_buffer().set_text(self.drafts.get(self.draft_key(), ""))
        self.rendering = False
        clear(self.file_chips)
        for item in self.files.get(self.draft_key(), []):
            chip = box(4)
            preview = self.attachment_preview(f"draft:{id(item)}", item["name"], item["media_type"], lambda f=item: f["content"], compact=True)
            if preview:
                chip.append(preview)
            chip.append(button("Remove " + item["name"], lambda f=item: self.remove_file(f)))
            self.file_chips.append(chip)
        self.update_send()

    def update_send(self):
        # A blocked conversation keeps its history, but the composer is off (SDS §15.2).
        enabled = bool(self.peer and self.identity and not self.sending and not self.blocked(self.peer))
        self.voice_call_button.set_sensitive(bool(self.peer and self.identity))
        self.video_call_button.set_sensitive(bool(self.peer and self.identity))
        self.composer.set_sensitive(enabled)
        mode = self.compose_modes.get(self.draft_key())
        self.attach_button.set_sensitive(enabled and not (mode and mode["kind"] == "edit"))
        self.send_button.set_sensitive(enabled and not self.staging.get(self.draft_key(), 0) and bool(self.drafts.get(self.draft_key(), "").strip() or self.files.get(self.draft_key())))

    def stage_started(self, key):
        self.staging[key] = self.staging.get(key, 0) + 1
        self.update_send()

    def stage_finished(self, key):
        self.staging[key] = max(0, self.staging.get(key, 0) - 1)
        self.update_send()

    def composer_keys(self, _, key, code, state):
        paste = (key in (Gdk.KEY_v, Gdk.KEY_V) and state & Gdk.ModifierType.CONTROL_MASK) or (key == Gdk.KEY_Insert and state & Gdk.ModifierType.SHIFT_MASK)
        if paste and self.peer and not self.sending:
            clipboard = self.composer.get_clipboard()
            if clipboard.get_formats().contain_gtype(Gdk.Texture):
                target, generation = self.draft_key(), self.generation
                self.stage_started(target)
                def pasted(source, result):
                    try:
                        texture = source.read_texture_finish(result)
                        if generation != self.generation or texture is None:
                            return
                        if texture.get_width() * texture.get_height() > 32_000_000:
                            raise ValueError("That image is too large.")
                        data = texture.save_to_png_bytes().get_data()
                        if len(data) > MAX_FILE or len(self.files.get(target, [])) >= 5:
                            raise ValueError("Attach up to five files, at most 25 MiB each.")
                        self.files.setdefault(target, []).append({"name": "Pasted image.png", "media_type": "image/png", "content": data})
                        if target == self.draft_key():
                            self.restore_draft()
                    except Exception:
                        self.toast("Could not paste that image. Attach up to five files, at most 25 MiB each.")
                    finally:
                        if generation == self.generation:
                            self.stage_finished(target)
                clipboard.read_texture_async(None, pasted)
                return True
        if key in (Gdk.KEY_Return, Gdk.KEY_KP_Enter) and not state & Gdk.ModifierType.SHIFT_MASK:
            self.send()
            return True
        if key == Gdk.KEY_Escape and self.compose_modes.get(self.draft_key()):
            self.cancel_mode()
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
        self.send_protected()

    def keep_composer_focus(self, key):
        """Back to the message box after Send or Enter, so the next message can be typed at once.

        Sending disables the box, which drops focus; give it back unless the person has since moved
        to another conversation or into a dialog."""
        if key == self.draft_key() and self.composer.get_sensitive() and not any(d.get_visible() for d in self.dialogs):
            self.composer.grab_focus()

    def call(self, video):
        """Open the call in the web inbox, which has the full call client.

        The embedded call page needs WebRTC, and neither Debian/Ubuntu's WebKitGTK nor the GNOME
        Flatpak runtime's is built with it. The address is copied so it can be found at once."""
        if not self.peer:
            return
        peer = self.peer
        self.get_clipboard().set(peer)
        self.open_url(WEB_INBOX)
        kind = "video" if video else "voice"
        self.toast(f"Calls open in the web inbox. Choose {display_name(peer)} there and start a {kind} call — the address is copied.")

    def caller_name(self, address, handle):
        """A ringing caller as this mailbox's contacts name them, else their handle or address."""
        shown = display_name(handle or address)
        alias = next((c.get("alias") for c in self.contacts if c.get("peer") in (handle, address) and c.get("alias")), None)
        # Beside an alias, the address itself says who is really calling.
        where = handle[:-5] if handle and handle.endswith("/main") and handle.count("/") > 1 else handle or address
        return f"{alias} ({where})" if isinstance(alias, str) and alias.strip() else shown

    def answer_call(self, room):
        if self.calls and self.calls.answer(room):
            self.toast("The call opens in your browser.")

    def decline_call(self, room):
        if self.calls:
            self.calls.decline(room)

    def sent_snapshot(self, messages):
        self.messages = live_messages(messages)
        self.render_all()

    def reload_messages(self):
        identity, revision = self.identity, self.revision
        self._work(lambda: self.read_inbox(identity),
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
        if len(self.files.get(key, [])) + self.staging.get(key, 0) >= 5:
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
        self.stage_started(key)
        def staged(item):
            self.stage_finished(key)
            if len(self.files.get(key, [])) < 5:
                self.files.setdefault(key, []).append(item)
            if key == self.draft_key():
                self.restore_draft()
        def failed(error):
            self.stage_finished(key)
            self.toast(self.error_text(error))
        self._work(read, staged, failed)
        return True

    def remove_file(self, item):
        if not self.sending:
            self.files[self.draft_key()].remove(item)
            self.restore_draft()

    def attachment_preview(self, key, name, mime, read, compact=False):
        if not preview_supported(name, mime):
            return None
        frame = Gtk.Frame()
        frame.set_size_request(72 if compact else 220, 54 if compact else 150)
        frame.set_halign(Gtk.Align.START)
        frame.set_overflow(Gtk.Overflow.HIDDEN)
        frame.set_child(label("Loading preview…", "dim-label"))
        frame.set_tooltip_text("Preview of " + name)
        frame.preview_started = False
        frame.preview_timer = None
        generation = self.generation

        def painted(result):
            if not frame.get_root() or generation != self.generation:
                return
            if isinstance(result, str) or result is None:
                text = label(result or "Preview unavailable", "dim-label", wrap=True)
                text.set_lines(2 if compact else 8)
                text.set_ellipsize(Pango.EllipsizeMode.END)
                text.set_max_width_chars(12 if compact else 30)
                frame.set_child(text)
            else:
                from gi.repository import GdkPixbuf
                scale = min((72 if compact else 220) / result.get_width(), (54 if compact else 150) / result.get_height())
                sized = result.scale_simple(max(1, int(result.get_width() * scale)), max(1, int(result.get_height() * scale)), GdkPixbuf.InterpType.BILINEAR)
                picture = Gtk.Picture.new_for_paintable(Gdk.Texture.new_for_pixbuf(sized))
                picture.set_can_shrink(True)
                picture.set_content_fit(Gtk.ContentFit.CONTAIN)
                frame.set_child(picture)

        def visible():
            if not frame.get_root() or generation != self.generation or frame.preview_started:
                frame.preview_timer = None
                return False
            if not compact:
                ok, bounds = frame.compute_bounds(self.message_scroll)
                if not ok or bounds.get_y() > self.message_scroll.get_height() + 100 or bounds.get_y() + bounds.get_height() < -100:
                    return True
            frame.preview_started = True
            frame.preview_timer = None
            self._work(lambda: self.previews.load(key, name, mime, read), painted, lambda _: painted(None))
            return False

        def mapped(_):
            if not frame.preview_started and frame.preview_timer is None:
                frame.preview_timer = GLib.timeout_add(100, visible)

        def unmapped(_):
            if frame.preview_timer is not None:
                GLib.source_remove(frame.preview_timer)
                frame.preview_timer = None

        frame.connect("map", mapped)
        frame.connect("unmap", unmapped)
        return frame

    def open_attachment(self, attachment):
        identity = self.identity
        name = safe_filename(attachment.get("filename", "attachment"))
        return AttachmentViewer(self, name, attachment.get("media_type", ""),
                                lambda: self.api.download(identity, attachment["id"]),
                                lambda data: self.save_attachment(attachment, data))

    def save_attachment(self, attachment, data=None):
        identity = self.identity
        chooser = Gtk.FileChooserNative.new("Save attachment", self, Gtk.FileChooserAction.SAVE, "Save", "Cancel")
        chooser.set_current_name(safe_filename(attachment.get("filename", "attachment")))
        def chosen(dialog, response):
            file = dialog.get_file() if response == Gtk.ResponseType.ACCEPT else None
            dialog.destroy()
            if not file:
                return
            def save():
                content = data if data is not None else self.api.download(identity, attachment["id"])
                # GIO atomically replaces only the explicit user-selected path, including portals.
                file.replace_contents(content, None, False, Gio.FileCreateFlags.REPLACE_DESTINATION, None)
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

    def form_dialog(self, title, placeholder, submitted, initial="", address=False):
        window, content = self.dialog(title)
        entry = Gtk.Entry(placeholder_text=placeholder, text=initial)
        if address:
            prefix_pending = False
            def prefix_address(widget):
                nonlocal prefix_pending
                value = conversation_address_input(widget.get_text())
                if value != widget.get_text():
                    position = widget.get_position()
                    widget.set_text(value)
                    widget.set_position(max(1, position + 1))
                prefix_pending = False
                return GLib.SOURCE_REMOVE
            def schedule_prefix(widget):
                nonlocal prefix_pending
                # Replacing text emits delete and insert changes. Wait until GTK
                # completes both before changing the buffer again.
                if not prefix_pending:
                    prefix_pending = True
                    GLib.idle_add(prefix_address, widget)
            entry.connect("changed", schedule_prefix)
        content.append(entry)
        error_label = label("", "error", wrap=True)
        content.append(error_label)
        def submit():
            value = entry.get_text().strip()
            if address:
                value = conversation_address_input(value)
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
                raise ValueError("Use an address such as /bekir, /bekir/main or /k/your-address.")
            self.peer, self.subject = normalize(peer, self.messages), None
            self.render_all()
            self.restore_draft()
            self.composer.grab_focus()
        self.form_dialog("New conversation", "/bekir", start, initial="/", address=True)

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
        content.append(self.appearance_picker())
        settings_group(content, settings_row("Account", self.title.get_subtitle() or "Your profile and devices",
                       "avatar-default-symbolic", lambda: self.account_settings(navigation)))
        # Phase 1b pages: each only where the postbox has the feature on; Blocked always (SDS §15).
        privacy_rows = []
        if AVATARS in self.features and self.identity:
            privacy_rows.append(settings_row("Profile picture", "Your picture and who can see it", "camera-photo-symbolic",
                                             lambda: self.profile_settings(navigation)))
        if self.privacy_available() and self.identity:
            privacy_rows.append(settings_row("Privacy & filtering", "Calls and messages from people you don’t know",
                                             "security-high-symbolic", lambda: self.privacy_settings(navigation)))
        if self.identity:
            privacy_rows.append(settings_row("Blocked", "People who can’t message or call you", "action-unavailable-symbolic",
                                             lambda: self.blocked_settings(navigation)))
        if privacy_rows:
            settings_group(content, *privacy_rows)
        pairing_rows = self.pairing_settings_rows(navigation) if self.identity else []
        if pairing_rows:
            settings_group(content, *pairing_rows)
        settings_group(content,
            settings_row("Handles", "Your names and subscriptions", "insert-link-symbolic", lambda: self.handles(navigation)),
            settings_row("Inbox and storage", "Storage and archived conversations", "mail-unread-symbolic", lambda: self.inbox_settings(navigation)),
            settings_row("Contacts and permissions", "Senders you know and trust", "system-users-symbolic", lambda: self.contact_list(navigation)))
        settings_group(content, settings_row("Help and about", "Support, privacy and app information",
                       "help-about-symbolic", lambda: self.help_settings(navigation)))
        navigation.window.present()

    def appearance_picker(self):
        """System, Light or Dark, applied at once and kept between launches."""
        section = box(8)
        section.append(label("Appearance", "heading"))
        strip = box(0, horizontal=True)
        strip.add_css_class("pp-segmented")
        strip.set_halign(Gtk.Align.START)
        self.appearance_buttons = {}
        first = None
        for choice in CHOICES:
            toggle = Gtk.ToggleButton(label=choice.title(), active=self.preferences.appearance == choice)
            toggle.update_property([Gtk.AccessibleProperty.LABEL], [choice.title() + " appearance"])
            if first:
                toggle.set_group(first)
            first = first or toggle
            toggle.connect("toggled", lambda button, value=choice: self.choose_appearance(value) if button.get_active() else None)
            self.appearance_buttons[choice] = toggle
            strip.append(toggle)
        section.append(strip)
        return section

    def choose_appearance(self, choice):
        self.preferences.appearance = choice
        apply_appearance(choice)

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
        if peer and not self.blocked(peer):
            content.append(button("Block…", lambda: (window.close(), self.ask_block(peer, original.get("alias") or display_name(peer))),
                                  style="destructive-action"))
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
            self.mailbox_model.splice(0, self.mailbox_model.get_n_items(), [display_name(r["handle"]) if r.get("handle") else r.get("label") or display_name(r["address"]) for r in rows])
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
        # The calls stream goes first: nothing below may keep it open for a signed-out account.
        if self.calls:
            self.calls.stop()
        for dialog in list(self.dialogs):
            dialog.close()
        self.features = frozenset()
        self.filtered = []
        self.own_avatar_texture = None
        self.avatars.reset()
        self.reset_calls()
        self.group_clients = {}
        self.reset_groups()
        if self.page() != "conversation":
            self.main_stack.set_visible_child_name("conversation")
        self.new_group_button.set_visible(False)
        for lease in (self.mailbox_lease, self.lease):
            if lease is not None:
                lease.retire()
        self.lease = self.mailbox_lease = None
        self.reset_calendar()
        self.reset_orgs()
        self.reset_pairing()
        self.reset_notes(keep_login=False)
        self.calendar_button.set_visible(False)
        self.orgs_button.set_visible(False)
        self.notes_button.set_visible(False)
        self.sync_side_nav()
        self.identity = self.peer = self.subject = None
        self.messages, self.contacts, self.threads, self.archived, self.mailboxes = [], [], [], [], []
        self.drafts, self.files, self.quota, self.vocabulary = {}, {}, {}, {}
        for timer in list(getattr(self, "draft_timers", {}).values()):
            GLib.source_remove(timer)
        self.memory_work = None
        self.reset_messages()
        self.staging.clear()
        self.previews.clear()
        self.known_ids = None
        self.notification_target = None
        application = self.get_application()
        if application:
            application.withdraw_notification("new-mail")
        self.polling = self.sending = self.acking = False
        self.restore_draft()
        self.render_all()

    def sign_out(self):
        def signout():
            self.cancel.set()
            mailboxes = [row["address"] for row in self.mailboxes] if GROUPS in self.features else []
            clients = dict(self.group_clients)
            owner = self.lease.owner if self.lease is not None else None
            notes_key = self.notes_state.get("key_id")
            self.generation += 1
            self.clear_account()
            self.show_signin("Signing out…")
            self.login_button.set_sensitive(False)

            def leave():
                # This computer's group devices end with the session: revoked at the postbox (members see
                # it), their keys deleted here. A later sign-in enrols new devices without history.
                self.group_clients = clients
                try:
                    self.retire_group_devices(mailboxes)
                finally:
                    self.group_clients = {}
                if notes_key:
                    try:  # notes made from now on skip this computer's key, which is deleted below
                        self.api.call("DELETE", "/v1/meetings/keys/" + notes_key)
                    except Exception:
                        pass
                try:
                    self.protected.forget_owner(owner)  # this account's drafts, unconfirmed work and notes key here
                except Exception:
                    pass
                self.session.sign_out()
            self._work(leave, lambda _: self.show_signin("You have signed out."),
                       lambda error: self.show_signin("Could not clear your desktop keyring. Unlock it and sign out again."))
        waiting = sum(len(rows) for rows in self.pending.values())
        self.confirm("Sign out?", "Unsent drafts and staged attachments on this device will be discarded" +
                     (f", and {waiting} unconfirmed {'message' if waiting == 1 else 'messages'} will be forgotten here." if waiting else "."),
                     signout, "Sign out")

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
        self.preferences = Preferences()
        self.design = None
        action = Gio.SimpleAction.new("open-message", GLib.VariantType.new("(ss)"))
        action.connect("activate", self.open_message)
        self.add_action(action)
        action = Gio.SimpleAction.new("open-group", GLib.VariantType.new("(ss)"))
        action.connect("activate", self.open_group_notification)
        self.add_action(action)
        for name, handler in (("call-answer", "answer_call"), ("call-decline", "decline_call")):
            action = Gio.SimpleAction.new(name, GLib.VariantType.new("s"))
            action.connect("activate", self.call_action, handler)
            self.add_action(action)

    def call_action(self, action, parameter, handler):
        room = parameter.get_string()
        if not valid_room(room):
            return
        if self.window and not self.window.closed:
            getattr(self.window, handler)(room)
        else:
            self.withdraw_notification("call-" + room)

    def open_group_notification(self, action, parameter):
        self.activate()
        identity, gid = parameter.unpack()
        if self.window and self.window.identity == identity and self.window.group_by_id(gid):
            self.window.open_group(gid)

    def open_message(self, action, parameter):
        self.activate()
        self.window.open_notification(*parameter.unpack())

    def do_startup(self):
        Adw.Application.do_startup(self)
        apply_appearance(self.preferences.appearance)
        self.design = Design()

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
