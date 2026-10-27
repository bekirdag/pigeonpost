"""End-to-end encrypted groups (SDS §4, docs/groups.md): the client loop, the group list, the group
view, New group, Group info, invitations and joining by link. Mixed into the main window.

Every network and MLS operation runs on a worker thread through `GroupClient`, which serializes
them per mailbox. Groups appear only when the postbox enables them and the encryption library
loaded: there is no weaker fallback.
"""

import datetime
import mimetypes
import time
import urllib.parse
from pathlib import Path

from gi.repository import Adw, Gdk, Gio, GLib, Gtk

from . import groupcrypto
from .api import MAX_FILE, APIError
from .features import (DISAPPEARING, E2EE_NOTE, GROUPS, HOSTED_NOTE, LIMITS_NOTE, ROLE_LABEL, SAFETY_NOTE, TIMERS, WARNINGS, can_post,
                       commit_lines, explain, is_admin, notice_text, parse_invite_link, preview_of, spaced, timer_label)
from .actions import MESSAGE_EDIT, MESSAGE_REPLY, RECEIPTS, group_tick, group_tick_names, quote_text
from .groups_core import GroupClient, UncertainSend, quote
from .message_ui import action_row, highlight, mode_bar, quote_block, tick_widget
from .model import contact_for, display_name, safe_filename, valid_peer
from .previews import viewer_kind
from .viewer import AttachmentViewer
from .widgets import action_row as settings_action_row, box, button, clear, label, scroll, plain_row

CALLS = "https://inbox.pigeonpost.dev/calls/v1"
CYCLE_SECONDS = 8
OPEN_CYCLE_SECONDS = 4
CRYPTO_SECONDS = 20
TOP_UP_SECONDS = 300
CALL_POLL_SECONDS = 15
FOREVER = 4102444800  # "muted" with no end, as the web inbox sets it
RECEIPTS_SECONDS = 10
RECONCILE_SECONDS = 60
ACTION_ERRORS = {
    "outbox_full": "Review the unconfirmed messages in your groups (retry or dismiss them) before sending more.",
    "outbox_expired": "This message is more than a day old and can no longer be retried. Dismiss it.",
    "not_editable": "This message can no longer be edited.",
    "empty_edit": "An edited message needs text.",
}


def group_explain(error):
    return ACTION_ERRORS.get(getattr(error, "code", None)) or explain(error)


GROUP_WARNINGS = {**WARNINGS,
                  "edit_unverified": "Security warning: a message claimed to change another one but could not be verified. Both are shown as sent."}


class GroupsMixin:
    # ---- lifecycle -----------------------------------------------------------------------------

    def reset_groups(self):
        self.group_clients = getattr(self, "group_clients", {})
        self.groups = []
        self.group_open = None
        self.group_members = {}
        self.group_handles = {}
        self.group_devices = {}
        self.group_state = None          # None (off) | "loading" | "ready" | "unavailable"
        self.group_persistent = True
        self.invites_open = False
        self.group_busy = False
        self.group_last_cycle = 0
        self.group_last_top_up = 0
        self.group_last_crypto = {}
        self.group_seen = None           # gid → highest seq already considered for notifications
        self.group_call = None
        self.group_call_polled = 0
        self.group_drafts = {}
        self.group_files = {}
        self.group_sending = False
        self.group_mode = None           # {"gid", "kind": "edit"|"reply", "seq", "saved", ...}
        self.group_receipts = {}         # gid → GET …/receipts, for ticks on the open group
        self.group_receipts_at = {}
        self.group_reconciled = {}
        self.group_bubbles = {}          # seq → bubble widget of the open group (jump to a reply's parent)

    def groups_on(self):
        return GROUPS in self.features and self.group_state == "ready"

    def group_storage_for(self, gc):
        factory = getattr(self, "group_storage_factory", None)
        if factory:
            return factory(gc)
        from .groupstore import GroupStorage
        return GroupStorage(gc)

    def group_api(self, mailbox):
        # The client belongs to this mailbox under the login that made it: a later sign-in to
        # another account never posts this mailbox's queued work (binding.Lease).
        lease = getattr(self, "lease", None)
        api = self.api_for(lease) if hasattr(self, "api_for") else self.api

        def call(method, path, body=None, device=None, raw=None, binary=False):
            return api.scoped(method, path, mailbox, body, device=device, raw=raw, binary=binary,
                              timeout=120 if raw is not None or binary else 35)
        return call

    def client_for(self, mailbox):
        """Worker thread: this mailbox's group device, enrolled on first use (docs/groups.md §3.1)."""
        client = self.group_clients.get(mailbox)
        if client:
            return client
        gc = groupcrypto.load()
        if gc is None:
            raise RuntimeError(groupcrypto.reason() or "group encryption unavailable")
        client = GroupClient(gc, self.group_api(mailbox), mailbox, self.group_storage_for(gc))
        info = client.init()
        self.group_persistent = info["persistent"]
        try:
            client.top_up_key_packages()
        except Exception:
            pass  # topped up by the loop
        self.group_clients[mailbox] = client
        return client

    def start_groups(self):
        """Called when the capabilities or the mailbox change."""
        identity = self.identity
        if GROUPS not in self.features or not identity:
            self.group_state = None
            self.groups = []
            self.render_group_chrome()
            return
        if self.group_state in ("ready", "loading") and self.group_clients.get(identity):
            return
        self.group_state = "loading"
        self.render_group_chrome()

        def ready(_):
            self.group_state = "ready"
            self.group_last_top_up = time.monotonic()
            self.render_group_chrome()
            self.groups_cycle(force=True)

        def failed(error):
            # No library, no keyring or no device: no groups, never a weaker fallback.
            self.group_state = "unavailable"
            self.group_error = str(error)
            self.render_group_chrome()
        self._work(lambda: self.client_for(identity), ready, failed)

    def group_client(self):
        return self.group_clients.get(self.identity)

    def groups_tick(self):
        if not self.groups_on() or self.group_busy:
            return
        now = time.monotonic()
        due = OPEN_CYCLE_SECONDS if self.group_open and self.is_active() else CYCLE_SECONDS
        if now - self.group_last_cycle >= due:
            self.groups_cycle()
        if self.group_open and now - self.group_call_polled >= CALL_POLL_SECONDS:
            self.poll_group_call()

    def groups_cycle(self, force=False):
        """One pass of the client loop: key packages, Welcomes, the group list, catch-up,
        pending commits, weekly self-updates and local expiry."""
        client, mailbox = self.group_client(), self.identity
        if not client or self.group_busy:
            return
        self.group_busy = True
        self.group_last_cycle = time.monotonic()
        open_gid = self.group_open
        top_up = time.monotonic() - self.group_last_top_up >= TOP_UP_SECONDS
        crypto_due = {gid for gid, at in self.group_last_crypto.items() if time.monotonic() - at < CRYPTO_SECONDS}
        reconcile = open_gid if open_gid and (force or time.monotonic() - self.group_reconciled.get(open_gid, 0) >= RECONCILE_SECONDS) else None

        def cycle():
            if top_up:
                try:
                    client.top_up_key_packages()
                except Exception:
                    pass
            try:
                client.process_welcomes()
            except Exception:
                pass
            groups = self.api.scoped("GET", "/v1/groups?state=active,invited,keying", mailbox).get("groups") or []
            groups = [g for g in groups if isinstance(g, dict) and g.get("id")]
            checked = set()
            for g in groups:
                gid = g["id"]
                my = g.get("my") or {}
                try:
                    if g.get("deleted_at"):
                        if client.in_group(gid):
                            client.forget(gid)
                        continue
                    if (my.get("state") == "keying" and g.get("activation_state") == "provisioning"
                            and g.get("created_by") == mailbox and my.get("role") == "owner"):
                        client.activate(gid)
                    elif my.get("state") == "active" and client.in_group(gid):
                        if (g.get("last_seq") or 0) > client.cursor(gid) or gid == open_gid:
                            client.sync(gid)
                        if force or gid == open_gid or gid not in crypto_due:
                            checked.add(gid)
                            client.process_pending(gid)
                        client.maintain(gid)
                except Exception as exc:
                    if getattr(exc, "code", None) in ("not_member", "group_deleted"):
                        continue
            if reconcile and client.in_group(reconcile):
                try:
                    client.reconcile(reconcile)
                except Exception:
                    pass  # tried again on a later pass
            client.prune_expired()
            return groups, checked

        def done(result):
            self.group_busy = False
            groups, checked = result
            if reconcile:
                self.group_reconciled[reconcile] = time.monotonic()
            for gid in checked:
                self.group_last_crypto[gid] = time.monotonic()
            if top_up:
                self.group_last_top_up = time.monotonic()
            self.groups = groups
            self.notify_group_messages()
            self.render_peers()
            if self.group_open:
                self.render_group_view()
                self.mark_group_read()
                self.load_group_receipts(self.group_open)

        def failed(error):
            self.group_busy = False
            if getattr(error, "code", None) == "feature_disabled":
                self.features = self.features - {GROUPS}
                self.start_groups()
        self._work(cycle, done, failed)

    def notify_group_messages(self):
        client = self.group_client()
        if not client:
            return
        first = self.group_seen is None
        seen = self.group_seen or {}
        for g in self.groups:
            gid, my = g["id"], g.get("my") or {}
            items = [x for x in client.history(gid) if x.get("kind") == "message" and isinstance(x.get("seq"), int)]
            newest = max((x["seq"] for x in items), default=0)
            previous = seen.get(gid, newest if first else 0)
            seen[gid] = max(previous, newest)
            if first:
                continue
            muted = (my.get("muted_until") or 0) > time.time() or my.get("notify") in ("none", "mentions")
            fresh = [x for x in items if x["seq"] > previous and x.get("sender") != self.identity and not x.get("deleted")
                     and not self.blocked(self.group_handles.get(x.get("sender")) or x.get("sender")) and not self.blocked(x.get("sender"))]
            visible = self.is_active() and self.group_open == gid and self.page() == "group"
            if fresh and not muted and not visible:
                notification = Gio.Notification.new("New Pigeonpost message")
                notification.set_body("Open Pigeonpost to read your message.")
                notification.set_default_action_and_target("app.open-group", GLib.Variant("(ss)", (self.identity, gid)))
                application = self.get_application()
                if application:
                    application.send_notification("group-" + gid, notification)
        self.group_seen = seen

    # ---- names ---------------------------------------------------------------------------------

    def name_of(self, address):
        if not address:
            return "Someone"
        if address == self.identity:
            return "You"
        handle = self.group_handles.get(address)
        alias = contact_for(handle or address, self.contacts).get("alias") if (handle or address) else None
        return alias or display_name(handle or address)

    def group_by_id(self, gid):
        return next((g for g in self.groups if g.get("id") == gid), None)

    # ---- list rows -----------------------------------------------------------------------------

    def render_group_rows(self, search):
        """Invitations first, then groups by last activity, above direct conversations."""
        if not self.groups_on():
            return
        client = self.group_client()
        invited = [g for g in self.groups if (g.get("my") or {}).get("state") == "invited" and not g.get("deleted_at")]
        if invited and not search:
            row = Gtk.ListBoxRow()
            row.kind = "invitations"
            content = box(10, horizontal=True, margin=9)
            content.append(Adw.Avatar.new(32, "Invitations", False))
            text = box(3)
            text.append(label(f"Invitations ({len(invited)})", "pp-peer"))
            text.append(label("Choose to join or decline" if self.invites_open else "Group invitations waiting for you", "pp-preview"))
            content.append(text)
            row.set_child(content)
            self.peer_list.append(row)
            if self.invites_open:
                for g in invited:
                    self.peer_list.append(self.invitation_row(g))
        rows = [g for g in self.groups if (g.get("my") or {}).get("state") != "invited" and not g.get("deleted_at")
                and (not search or search in str(g.get("name", "")).casefold())]
        rows.sort(key=lambda g: -(g.get("last_at") or 0))
        for g in rows:
            row = Gtk.ListBoxRow()
            row.kind, row.group_id, row.peer = "group", g["id"], None
            content = box(10, horizontal=True, margin=9)
            content.append(Adw.Avatar.new(32, g.get("name") or "Group", True))
            text = box(3)
            text.set_hexpand(True)
            top = box(6, horizontal=True)
            name = label(g.get("name") or "Group", "pp-peer")
            name.set_hexpand(True)
            top.append(name)
            my = g.get("my") or {}
            if g.get("ephemeral_secs"):
                timer = Gtk.Image(icon_name="alarm-symbolic")
                timer.set_tooltip_text("Disappearing messages: " + timer_label(g["ephemeral_secs"]))
                top.append(timer)
            if (my.get("muted_until") or 0) > time.time():
                muted = Gtk.Image(icon_name="notifications-disabled-symbolic")
                muted.set_tooltip_text("Muted")
                top.append(muted)
            unread = my.get("unread") or 0
            if unread and g["id"] != self.group_open:
                top.append(label(str(unread), "pp-badge"))
            text.append(top)
            if my.get("state") == "keying":
                preview = "Waiting for a member’s device to add you…"
            else:
                preview = (preview_of(client.history(g["id"]), self.identity, self.name_of) if client else "") or (
                    "Setting up encryption…" if g.get("activation_state") == "provisioning" else "No messages yet")
            text.append(label(preview.replace("\n", " ")[:120], "pp-preview"))
            content.append(text)
            row.set_child(content)
            row.update_property([Gtk.AccessibleProperty.LABEL], [f"Group {g.get('name')}" + (f", {unread} unread" if unread else "")])
            self.peer_list.append(row)
            if g["id"] == self.group_open and self.page() == "group":
                self.peer_list.select_row(row)

    def invitation_row(self, g):
        row = Gtk.ListBoxRow(activatable=False, selectable=False)
        row.kind, row.group_id = "invitation", g["id"]
        content = box(8, horizontal=True, margin=9)
        content.append(Adw.Avatar.new(28, g.get("name") or "Group", True))
        text = box(2)
        text.set_hexpand(True)
        text.append(label(g.get("name") or "Group", "pp-peer"))
        my = g.get("my") or {}
        bits = ["From " + self.name_of(my["invited_by"])] if my.get("invited_by") else []
        if my.get("filtered"):
            bits.append("someone you don’t know")
        bits.append(f"{g.get('member_count') or 1} members")
        text.append(label(" · ".join(bits), "pp-preview"))
        content.append(text)
        decline = button("Decline", lambda: self.answer_invitation(g, False), style="flat")
        decline.update_property([Gtk.AccessibleProperty.LABEL], [f"Decline {g.get('name')}"])
        join = button("Join", lambda: self.answer_invitation(g, True), style="suggested-action")
        join.update_property([Gtk.AccessibleProperty.LABEL], [f"Join {g.get('name')}"])
        for widget in (decline, join):
            widget.set_valign(Gtk.Align.CENTER)
            content.append(widget)
        row.set_child(content)
        return row

    def answer_invitation(self, g, accept):
        mailbox = self.identity

        def answered(_):
            self.toast(f"Joined {g.get('name')}. A member’s device will add you in a moment." if accept else "Invitation declined.")
            self.groups_cycle(force=True)
        self._work(lambda: self.api.scoped("POST", f"/v1/groups/{quote(g['id'])}/invitation", mailbox, {"accept": accept}),
                   answered, lambda error: self.toast(explain(error)))

    def group_row_selected(self, row):
        if row.kind == "invitations":
            self.invites_open = not self.invites_open
            GLib.idle_add(lambda: (self.render_peers(), False)[1])
            return True
        if row.kind == "group":
            self.open_group(row.group_id)
            return True
        return False

    # ---- the group view ------------------------------------------------------------------------

    def build_group_page(self):
        page = box(0)
        page.add_css_class("pp-wash")
        head = box(8, horizontal=True, margin=12)
        head.add_css_class("pp-toolbar")
        self.group_head_avatar = box(0)
        head.append(self.group_head_avatar)
        names = box(2)
        names.set_hexpand(True)
        self.group_title = label("", "pp-title")
        self.group_subtitle = label("", "dim-label")
        names.append(self.group_title)
        names.append(self.group_subtitle)
        head.append(names)
        self.group_join_call = button("Call in progress — Join", lambda: self.group_call_open(None, join=True), style="suggested-action")
        self.group_join_call.set_visible(False)
        head.append(self.group_join_call)
        self.group_voice = button("Group voice call", lambda: self.group_call_open(False), "call-start-symbolic", "flat")
        self.group_video = button("Group video call", lambda: self.group_call_open(True), "camera-video-symbolic", "flat")
        head.append(self.group_voice)
        head.append(self.group_video)
        head.append(button("Group info", self.group_info, "help-about-symbolic", "flat"))
        page.append(head)
        self.group_status = label("", "pp-status", wrap=True)
        self.group_status.set_margin_start(16)
        self.group_status.set_margin_end(16)
        page.append(self.group_status)
        self.group_messages = box(10, margin=20)
        self.group_scroll = scroll(self.group_messages)
        page.append(self.group_scroll)
        self.group_announce = label("Only admins can send messages", "pp-status", wrap=True)
        self.group_announce.set_margin_start(16)
        self.group_announce.set_margin_bottom(12)
        page.append(self.group_announce)
        self.group_chips = box(horizontal=True, margin=8)
        page.append(self.group_chips)
        self.group_mode_holder = box(0)
        page.append(self.group_mode_holder)
        composer = box(6, horizontal=True, margin=16)
        composer.set_margin_top(4)
        composer.add_css_class("pp-composer")
        self.group_attach = button("Attach a file", self.group_pick_file, "mail-attachment-symbolic", "flat")
        self.group_attach.set_valign(Gtk.Align.END)
        composer.append(self.group_attach)
        self.group_composer = Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR, accepts_tab=False)
        for side in ("top", "bottom"):
            getattr(self.group_composer, f"set_{side}_margin")(8)
        self.group_composer.set_left_margin(10)
        self.group_composer.set_right_margin(10)
        self.group_composer.update_property([Gtk.AccessibleProperty.LABEL], ["Write to the group. Enter sends; Shift Enter adds a line."])
        self.group_composer.get_buffer().connect("changed", self.group_draft_changed)
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", self.group_composer_keys)
        self.group_composer.add_controller(keys)
        compose_scroll = Gtk.ScrolledWindow(hexpand=True, min_content_height=64, max_content_height=130)
        compose_scroll.set_child(self.group_composer)
        composer.append(compose_scroll)
        self.group_send_button = button("Send to group", self.group_send, "mail-send-symbolic", "suggested-action")
        self.group_send_button.set_valign(Gtk.Align.END)
        composer.append(self.group_send_button)
        self.group_composer_box = composer
        page.append(composer)
        return page

    def open_group(self, gid):
        if self.group_mode:
            self.cancel_group_mode()
        self.group_open = gid
        self.peer = self.subject = None
        self.group_call = None
        self.group_call_polled = 0
        self.show_page("group")
        self.render_group_view()
        self.restore_group_draft()
        self.render_group_mode()
        self.render_peers()
        self.mark_group_read()
        client = self.group_client()
        if client and self.group_state == "ready":
            mailbox = self.identity
            self._work(lambda: self.load_group_members(mailbox, gid), lambda _: self.render_group_view(), lambda _: None)
            self.load_group_receipts(gid, force=True)
            self.groups_cycle()

    def load_group_members(self, mailbox, gid):
        members = self.api.scoped("GET", f"/v1/groups/{quote(gid)}/members", mailbox).get("members") or []
        for member in members:
            if member.get("handle"):
                self.group_handles[member["address"]] = member["handle"]
        self.group_members[gid] = members
        return members

    def render_group_chrome(self):
        """Things that only exist while groups are on: the New group button and the list rows."""
        on = self.groups_on()
        self.new_group_button.set_visible(on)
        if not on and self.page() == "group":
            self.group_open = None
            self.show_page("conversation")
        self.render_peers()

    def render_group_view(self):
        g = self.group_by_id(self.group_open) if self.group_open else None
        if not g:
            return
        client = self.group_client()
        my = g.get("my") or {}
        clear(self.group_head_avatar)
        self.group_head_avatar.append(Adw.Avatar.new(36, g.get("name") or "Group", True))
        self.group_title.set_text(g.get("name") or "Group")
        count = g.get("member_count") or 1
        sub = [f"{count} {'member' if count == 1 else 'members'}"]
        if g.get("ephemeral_secs"):
            sub.append("⏱ " + timer_label(g["ephemeral_secs"]))
        self.group_subtitle.set_text(" · ".join(sub))
        active = my.get("state") == "active" and not g.get("deleted_at")
        self.group_voice.set_visible(active)
        self.group_video.set_visible(active)
        self.group_join_call.set_visible(bool(active and self.group_call and self.group_call.get("active")))
        status = ""
        if g.get("deleted_at"):
            status = "This group was deleted."
        elif my.get("state") == "keying":
            status = "You have joined. Waiting for a member’s device to add you to the encryption — this happens automatically when one is online."
        elif g.get("activation_state") == "provisioning":
            status = "Setting up encryption…"
        elif active and client and not client.in_group(g["id"]):
            status = "This device is being added to the group’s encryption. New messages will appear once it is."
        if not self.group_persistent:
            status = (status + " " if status else "") + "This device cannot keep group keys between runs."
        self.group_status.set_text(status)
        self.group_status.set_visible(bool(status))
        posting = can_post(g) and bool(client and client.in_group(g["id"])) and not g.get("deleted_at")
        announce = ""
        if active and my.get("role") == "restricted":
            announce = "You can read this group but not send messages."
        elif active and g.get("post_policy") == "admins" and not can_post(g):
            announce = "Only admins can send messages"
        self.group_announce.set_text(announce)
        self.group_announce.set_visible(bool(announce))
        self.group_composer_box.set_visible(posting)
        self.group_chips.set_visible(posting)
        self.update_group_send()
        self.render_group_messages(g)

    def render_group_messages(self, g):
        adj = self.group_scroll.get_vadjustment()
        bottom = adj.get_value() + adj.get_page_size() >= adj.get_upper() - 80
        clear(self.group_messages)
        self.group_bubbles = {}
        client = self.group_client()
        if not client:
            return
        now = time.time()
        items = [x for x in client.history(g["id"]) if not (x.get("expires_at") and x["expires_at"] <= now)]
        messages = [x for x in items if x.get("kind") == "message"]
        if not messages:
            empty = label(f"Messages disappear after {timer_label(g['ephemeral_secs'])}." if g.get("ephemeral_secs") else "No messages yet. Say hello.",
                          "pp-status", wrap=True)
            self.group_messages.append(empty)
        last_sender, last_day = None, None
        my_device = client.device_id()
        for item in items:
            at = item.get("created_at") or 0
            if at:
                day = datetime.date.fromtimestamp(at)
                if day != last_day:
                    last_day, last_sender = day, None
                    self.group_messages.append(self.system_line(day.strftime("%A %d %B").replace(" 0", " ")))
            kind = item.get("kind")
            if kind == "notice":
                text = notice_text(item, self.name_of)
                if text:
                    self.group_messages.append(self.system_line(text))
                    last_sender = None
                continue
            if kind == "commit":
                for line in commit_lines(items, item, self.identity, my_device, self.name_of):
                    text = line["text"]
                    if line.get("fingerprint"):
                        text += " · " + client.fingerprint_digits(line["fingerprint"])
                    self.group_messages.append(self.system_line(text, security=line.get("security")))
                last_sender = None
                continue
            if kind == "warning":
                self.group_messages.append(self.system_line(GROUP_WARNINGS.get(item.get("warning"), "Security warning."), security=True))
                last_sender = None
                continue
            if kind != "message" or item.get("replaced_by") is not None:
                continue
            bubble = self.group_bubble(g, item, show_sender=item.get("sender") != last_sender)
            self.group_messages.append(bubble)
            if isinstance(item.get("seq"), int):
                self.group_bubbles[item["seq"]] = bubble
            last_sender = item.get("sender")
        if bottom:
            GLib.idle_add(lambda: (adj.set_value(max(0, adj.get_upper() - adj.get_page_size())), False)[1])

    def system_line(self, text, security=False):
        line = label(text, "pp-system", wrap=True)
        line.set_halign(Gtk.Align.CENTER)
        line.set_justify(Gtk.Justification.CENTER)
        line.set_selectable(security)
        if security:
            line.add_css_class("pp-security")
        return line

    def group_bubble(self, g, item, show_sender):
        mine = item.get("sender") == self.identity
        gid = g["id"]
        client = self.group_client()
        outer = box(8, horizontal=True)
        outer.add_css_class("pp-message")
        outer.set_halign(Gtk.Align.END if mine else Gtk.Align.START)
        sender_key = self.group_handles.get(item.get("sender")) or item.get("sender")
        if not mine:
            holder = box(0)
            holder.set_size_request(28, -1)
            if show_sender:
                holder.append(self.avatars.widget(sender_key, self.name_of(item.get("sender")), 28))
            holder.set_valign(Gtk.Align.START)
            outer.append(holder)
        column = box(2)
        column.set_halign(Gtk.Align.END if mine else Gtk.Align.START)
        bubble = box(6)
        bubble.add_css_class("pp-bubble")
        if mine:
            bubble.add_css_class("pp-mine")
        inner = box(6, margin=12)
        inner.set_margin_start(14)
        inner.set_margin_end(14)
        if not mine and show_sender:
            inner.append(label(self.name_of(item.get("sender")), "pp-meta"))
        hidden = not mine and (self.blocked(sender_key) or self.blocked(item.get("sender")))
        readable = not (item.get("deleted") or item.get("undecryptable") or item.get("missing") or hidden)
        if item.get("reply_to") and readable:
            inner.append(self.group_quote(gid, item))
        if item.get("deleted"):
            inner.append(label("This message was deleted", "dim-label"))
        elif item.get("undecryptable"):
            inner.append(label("Encrypted message — not readable on this device", "dim-label"))
        elif item.get("missing"):
            inner.append(label("An earlier version of this message is not on this device", "dim-label"))
        elif hidden:
            inner.append(label("Message from someone you blocked", "dim-label"))
        else:
            if item.get("text"):
                text = label(item["text"], "pp-body", wrap=True)
                text.set_selectable(True)
                text.set_use_markup(False)
                inner.append(text)
            for file in item.get("files") or []:
                name = safe_filename(file.get("name") or "file")
                opener = button(name + (" · " + self.size_label(file.get("size")) if file.get("size") else ""),
                                lambda f=file: self.group_open_file(gid, f), style="pp-file")
                opener.set_halign(Gtk.Align.START)
                opener.update_property([Gtk.AccessibleProperty.LABEL], ["Open " + name])
                inner.append(opener)
        meta = box(6, horizontal=True)
        meta.set_halign(Gtk.Align.END if mine else Gtk.Align.START)
        if item.get("expires_at"):
            timer = Gtk.Image(icon_name="alarm-symbolic")
            try:
                timer.set_tooltip_text("Disappears " + datetime.datetime.fromtimestamp(item["expires_at"]).strftime("%d %b %H:%M"))
            except (ValueError, OSError, OverflowError):
                pass
            meta.append(timer)
        at = item.get("created_at")
        if item.get("uncertain"):
            when = "Not confirmed"
        elif item.get("pending"):
            when = "Sending…"
        else:
            when = datetime.datetime.fromtimestamp(at).strftime("%H:%M") if at else ""
        meta.append(label(when, "pp-meta"))
        if item.get("edit_of") and readable:
            meta.append(label("edited", "pp-edited"))
        if mine and readable and not item.get("uncertain"):
            if item.get("pending"):
                state, accessible = "sending", "Sending"
            elif RECEIPTS in self.features and isinstance(item.get("seq"), int):
                state, accessible = group_tick(item["seq"], self.group_receipts.get(gid))
            else:
                state, accessible = "sent", "Sent"
            meta.append(tick_widget(state, accessible))
        inner.append(meta)
        bubble.append(inner)
        column.append(bubble)
        actions = self.group_message_actions(g, item, mine, readable, sender_key)
        buttons = []
        if item.get("uncertain"):
            entry = next((e for e in (client.outbox(gid) if client else []) if e["cmid"] == item.get("cmid")), None)
            if entry and entry["retryable"]:
                buttons.append(("Retry sending", "view-refresh-symbolic", lambda c=item.get("cmid"): self.group_retry(gid, c)))
            buttons.append(("Dismiss on this device", "edit-clear-symbolic", lambda c=item.get("cmid"): self.group_dismiss(gid, c)))
            if item.get("text"):
                buttons.append(("Copy message", "edit-copy-symbolic", actions["copy"]))
        elif readable and not item.get("pending"):
            if mine:
                if "edit" in actions:
                    buttons.append(("Edit message", "document-edit-symbolic", actions["edit"]))
                buttons.append(("Copy message", "edit-copy-symbolic", actions["copy"]))
            else:
                buttons.append(("Copy message", "edit-copy-symbolic", actions["copy"]))
                if "reply" in actions:
                    buttons.append(("Reply", "mail-reply-sender-symbolic", actions["reply"]))
        row = action_row(mine, buttons) if buttons or actions else None
        if row is not None:
            menu = self.group_menu(actions)
            if menu is not None:
                more = Gtk.MenuButton(icon_name="view-more-symbolic", menu_model=menu)
                more.add_css_class("flat")
                more.set_tooltip_text("More actions")
                more.update_property([Gtk.AccessibleProperty.LABEL], ["More actions"])
                row.append(more)
                click = Gtk.GestureClick(button=3)

                def pressed(_, _n, x, y, model=menu, anchor=bubble):
                    # Made on demand and unparented when it closes, so a redraw leaves nothing behind.
                    popover = Gtk.PopoverMenu.new_from_model(model)
                    popover.set_parent(anchor)
                    popover.set_has_arrow(False)
                    rect = Gdk.Rectangle()
                    rect.x, rect.y, rect.width, rect.height = int(x), int(y), 1, 1
                    popover.set_pointing_to(rect)
                    popover.connect("closed", lambda p: GLib.idle_add(lambda: (p.unparent() if p.get_parent() else None, False)[1]))
                    popover.popup()
                click.connect("pressed", pressed)
                bubble.add_controller(click)
                long_press = Gtk.GestureLongPress()
                long_press.connect("pressed", lambda _, x, y, f=pressed: f(None, 1, x, y))
                bubble.add_controller(long_press)
            column.append(row)
        group = Gio.SimpleActionGroup()
        for name, handler in actions.items():
            action = Gio.SimpleAction.new(name, None)
            action.connect("activate", lambda *_, run=handler: run())
            group.add_action(action)
        outer.insert_action_group("gmsg", group)
        outer.append(column)
        outer.item = item
        outer.bubble = bubble
        outer.actions = actions
        outer.action_row = row
        return outer

    def group_message_actions(self, g, item, mine, readable, sender_key):
        """Every action of one message: the row shows the frequent ones, the menu keeps all."""
        gid, client = g["id"], self.group_client()
        seq = item.get("seq") if isinstance(item.get("seq"), int) else None
        actions = {}
        if not readable or item.get("pending"):
            if item.get("uncertain") and item.get("text"):
                actions["copy"] = lambda: self.get_clipboard().set(item.get("text") or "")
            return actions
        actions["copy"] = lambda: self.get_clipboard().set(item.get("text") or "")
        posting = can_post(g) and bool(client and client.in_group(gid)) and not g.get("deleted_at")
        if MESSAGE_REPLY in self.features and seq is not None and posting:
            actions["reply"] = lambda: self.start_group_reply(gid, item)
        if mine and MESSAGE_EDIT in self.features and seq is not None and posting and client and client.editable(gid, seq):
            actions["edit"] = lambda: self.start_group_edit(gid, item)
        if seq is not None:
            actions["info"] = lambda: self.group_message_info(gid, seq)
        if seq is not None and (mine or is_admin(g)):
            actions["delete"] = lambda: self.group_delete_message(gid, seq)
        if not mine and item.get("sender"):
            actions["block"] = lambda: self.ask_block(sender_key, self.name_of(item.get("sender")))
        return actions

    @staticmethod
    def group_menu(actions):
        names = [("reply", "Reply"), ("copy", "Copy message"), ("edit", "Edit message"), ("info", "Message info"),
                 ("delete", "Delete for everyone…"), ("block", "Block sender…")]
        menu = Gio.Menu()
        for name, text in names:
            if name in actions:
                menu.append(text, "gmsg." + name)
        return menu if menu.get_n_items() else None

    def group_quote(self, gid, item):
        client = self.group_client()
        try:
            parent_seq = int(item["reply_to"])
        except (TypeError, ValueError):
            parent_seq = None
        parent = client.current(gid, parent_seq) if client and parent_seq is not None else None
        if not parent or parent.get("deleted") or parent.get("missing") or parent.get("undecryptable"):
            return quote_block("Reply", "Message unavailable")
        name = self.name_of(parent.get("sender"))
        text = quote_text(parent.get("text")) or ("📎 " + parent["files"][0]["name"] if parent.get("files") else "Message")
        target = parent.get("seq")
        return quote_block(name, text, lambda: self.jump_to_group_message(target))

    def jump_to_group_message(self, seq):
        widget = self.group_bubbles.get(seq)
        if widget is None:
            self.toast("The original message is not on this device any more.")
            return
        ok, bounds = widget.compute_bounds(self.group_messages)
        if ok:
            adj = self.group_scroll.get_vadjustment()
            adj.set_value(max(0, min(bounds.get_y() - 40, adj.get_upper() - adj.get_page_size())))
        highlight(getattr(widget, "bubble", widget))
        widget.grab_focus()

    def load_group_receipts(self, gid, force=False):
        if RECEIPTS not in self.features or not gid:
            return
        client = self.group_client()
        if not client or not client.in_group(gid):
            return
        if not force and time.monotonic() - self.group_receipts_at.get(gid, 0) < RECEIPTS_SECONDS:
            return
        self.group_receipts_at[gid] = time.monotonic()

        def loaded(receipts):
            if not isinstance(receipts, dict):
                return
            changed = self.group_receipts.get(gid) != receipts
            self.group_receipts[gid] = receipts
            if changed and gid == self.group_open and self.page() == "group":
                self.render_group_view()
        self._work(lambda: client.receipts(gid), loaded, lambda _: None)

    def group_message_info(self, gid, seq):
        client = self.group_client()
        if not client:
            return
        item = client.current(gid, seq) or {}
        info = client.versions(gid, seq)
        window, content = self.dialog("Message info")
        window.info_content = content
        at = item.get("created_at")
        who = self.name_of(item.get("sender"))
        content.append(label(f"From {who}" + (" · " + datetime.datetime.fromtimestamp(at).strftime("%d %b %Y %H:%M") if at else ""), "heading"))
        content.append(label("Versions on this device", "heading"))
        for version in info["versions"]:
            when = datetime.datetime.fromtimestamp(version["created_at"]).strftime("%d %b %H:%M") if version.get("created_at") else ""
            row = box(2)
            row.append(label(when, "pp-meta"))
            text = label(version.get("text") or "", wrap=True)
            text.set_selectable(True)
            row.append(text)
            content.append(row)
        if info["missing"]:
            content.append(label("Some earlier versions were sent before this device received them, so they are not shown.", "dim-label", wrap=True))
        if item.get("sender") == self.identity and RECEIPTS in self.features and isinstance(item.get("seq"), int):
            receipts = self.group_receipts.get(gid)
            seen, delivered, waiting = group_tick_names(item["seq"], receipts, self.name_of)
            content.append(label("Receipts", "heading"))
            for title, names in (("Seen by", seen), ("Delivered to", delivered), ("Not delivered yet", waiting)):
                if names:
                    content.append(label(f"{title}: " + ", ".join(names), wrap=True))
            if receipts and not receipts.get("read_receipts", True):
                content.append(label("You turned read receipts off, so you don’t see who has read your messages.", "dim-label", wrap=True))
        window.present()
        return window

    # ---- reply and edit modes ------------------------------------------------------------------

    def start_group_reply(self, gid, item):
        if self.group_mode:
            self.cancel_group_mode()
        name = self.name_of(item.get("sender"))
        self.group_mode = {"gid": gid, "kind": "reply", "seq": item["seq"], "name": name, "text": quote_text(item.get("text"))}
        self.render_group_mode()
        self.group_composer.grab_focus()

    def start_group_edit(self, gid, item):
        if self.group_mode:
            self.cancel_group_mode()
        self.group_mode = {"gid": gid, "kind": "edit", "seq": item["seq"], "saved": self.group_drafts.get(gid, ""),
                           "original": item.get("text") or ""}
        self.rendering = True
        self.group_composer.get_buffer().set_text(item.get("text") or "")
        self.rendering = False
        self.render_group_mode()
        self.update_group_send()
        self.group_composer.grab_focus()

    def cancel_group_mode(self):
        """✕ or Esc: leave reply/edit mode; an edit gives the ordinary draft back."""
        mode, self.group_mode = self.group_mode, None
        if mode and mode["kind"] == "edit":
            self.group_drafts[mode["gid"]] = mode.get("saved", "")
            if mode["gid"] == self.group_open:
                self.restore_group_draft()
        self.render_group_mode()
        self.update_group_send()

    def render_group_mode(self):
        clear(self.group_mode_holder)
        mode = self.group_mode
        if not mode or mode["gid"] != self.group_open:
            return
        if mode["kind"] == "edit":
            self.group_mode_holder.append(mode_bar("Editing message", None, self.cancel_group_mode))
        else:
            self.group_mode_holder.append(mode_bar("Replying to " + mode["name"], mode.get("text"), self.cancel_group_mode))

    @staticmethod
    def size_label(size):
        from .model import size_text
        return size_text(size or 0)

    def mark_group_read(self):
        g = self.group_by_id(self.group_open) if self.group_open else None
        client = self.group_client()
        if not g or not client or not self.is_active() or self.page() != "group":
            return
        my = g.get("my") or {}
        last = max([x["seq"] for x in client.history(g["id"]) if isinstance(x.get("seq"), int)] + [g.get("last_seq") or 0])
        if last > (my.get("read_cursor") or 0):
            my["read_cursor"], my["unread"] = last, 0
            mailbox = self.identity
            self._work(lambda: self.api.scoped("POST", f"/v1/groups/{quote(g['id'])}/read", mailbox, {"seq": last}), lambda _: None, lambda _: None)

    # ---- composing -----------------------------------------------------------------------------

    def group_draft_changed(self, buffer):
        if not self.rendering and self.group_open:
            text = buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)
            mode = self.group_mode
            if mode and mode["kind"] == "edit" and mode["gid"] == self.group_open:
                mode["text"] = text
            else:
                self.group_drafts[self.group_open] = text
        self.update_group_send()

    def restore_group_draft(self):
        self.rendering = True
        self.group_composer.get_buffer().set_text(self.group_drafts.get(self.group_open, ""))
        self.rendering = False
        clear(self.group_chips)
        for item in self.group_files.get(self.group_open, []):
            self.group_chips.append(button("Remove " + item["name"], lambda f=item: self.group_remove_file(f)))
        self.update_group_send()

    def update_group_send(self):
        gid = self.group_open
        ready = bool(gid and not self.group_sending)
        self.group_composer.set_sensitive(ready)
        self.group_attach.set_sensitive(ready)
        mode = self.group_mode if self.group_mode and self.group_mode["gid"] == gid else None
        if mode and mode["kind"] == "edit":
            text = mode.get("text", mode.get("original", ""))
            self.group_attach.set_sensitive(False)
            self.group_send_button.set_sensitive(ready and bool(text.strip()))
            self.group_send_button.set_tooltip_text("Save edit")
            return
        self.group_send_button.set_tooltip_text("Send to group")
        self.group_send_button.set_sensitive(ready and bool(self.group_drafts.get(gid, "").strip() or self.group_files.get(gid)))

    def group_composer_keys(self, _, key, code, state):
        if key == Gdk.KEY_Escape and self.group_mode:
            self.cancel_group_mode()
            return True
        if key in (Gdk.KEY_Return, Gdk.KEY_KP_Enter) and not state & Gdk.ModifierType.SHIFT_MASK:
            self.group_send()
            return True
        return False

    def group_pick_file(self):
        gid = self.group_open
        if not gid:
            return
        chooser = Gtk.FileChooserNative.new("Attach a file", self, Gtk.FileChooserAction.OPEN, "Attach", "Cancel")
        chooser.set_select_multiple(True)

        def chosen(dialog, response):
            if response == Gtk.ResponseType.ACCEPT:
                files = dialog.get_files()
                for index in range(files.get_n_items()):
                    self.group_stage_file(gid, files.get_item(index).get_path())
            dialog.destroy()
        chooser.connect("response", chosen)
        chooser.show()

    def group_stage_file(self, gid, path):
        staged = self.group_files.setdefault(gid, [])
        if len(staged) >= 5:
            self.toast("Attach up to five files per message.")
            return
        try:
            if not path or not Path(path).is_file():
                raise OSError("not a local file")
            if Path(path).stat().st_size > MAX_FILE:
                self.toast(Path(path).name + " is larger than 25 MiB.")
                return
            data = Path(path).read_bytes()[:MAX_FILE + 1]
        except OSError:
            self.toast("The selected file could not be read.")
            return
        staged.append({"name": Path(path).name, "mime": mimetypes.guess_type(path)[0] or "application/octet-stream", "bytes": data})
        if gid == self.group_open:
            self.restore_group_draft()

    def group_remove_file(self, item):
        files = self.group_files.get(self.group_open, [])
        if item in files and not self.group_sending:
            files.remove(item)
            self.restore_group_draft()

    def group_send(self):
        gid, client = self.group_open, self.group_client()
        if not gid or not client or not self.group_send_button.get_sensitive():
            return
        mode = self.group_mode if self.group_mode and self.group_mode["gid"] == gid else None
        if mode and mode["kind"] == "edit":
            return self.group_save_edit(gid, client, mode)
        text = self.group_drafts.get(gid, "").strip()
        files = list(self.group_files.get(gid, []))
        doc = {"text": text, "files": files}
        if mode and mode["kind"] == "reply":
            doc["reply_to"] = mode["seq"]
        self.group_sending = True
        self.group_drafts.pop(gid, None)
        self.group_files.pop(gid, None)
        if mode:
            self.group_mode = None
            self.render_group_mode()
        self.restore_group_draft()

        def sent(_):
            self.group_sending = False
            self.update_group_send()
            if gid == self.group_open:
                self.render_group_view()
                self.group_composer.grab_focus()
            self.render_peers()

        def failed(error):
            self.group_sending = False
            if isinstance(error, UncertainSend):
                # Kept with its exact request: the bubble offers Retry or Dismiss.
                self.update_group_send()
                if gid == self.group_open:
                    self.render_group_view()
                self.toast("Not confirmed. Check the conversation, then retry or dismiss it.")
                return
            self.group_drafts[gid] = text
            self.group_files[gid] = files
            if mode and gid == self.group_open and not self.group_mode:
                self.group_mode = mode
                self.render_group_mode()
            if gid == self.group_open:
                self.restore_group_draft()
                self.render_group_view()
            self.toast("Not sent: " + group_explain(error))
        self._work(lambda: client.send(gid, doc), sent, failed)
        self.render_group_view()

    def group_save_edit(self, gid, client, mode):
        text = mode.get("text", mode.get("original", "")).strip()
        if not text or text == (mode.get("original") or "").strip():
            self.cancel_group_mode()
            return
        self.group_sending = True
        self.update_group_send()

        def done(_):
            self.group_sending = False
            if self.group_mode is mode:
                self.cancel_group_mode()
            if gid == self.group_open:
                self.render_group_view()
            self.render_peers()

        def failed(error):
            self.group_sending = False
            if isinstance(error, UncertainSend):
                if self.group_mode is mode:
                    self.cancel_group_mode()
                if gid == self.group_open:
                    self.render_group_view()
                self.toast("Edit not confirmed. Check the conversation, then retry or dismiss it.")
                return
            self.update_group_send()
            self.toast("Not saved: " + group_explain(error))
        self._work(lambda: client.edit(gid, mode["seq"], text), done, failed)

    def group_retry(self, gid, cmid):
        client = self.group_client()
        if not client:
            return

        def done(_):
            if gid == self.group_open:
                self.render_group_view()
            self.toast("Sent")

        def failed(error):
            if gid == self.group_open:
                self.render_group_view()
            self.toast("Still not confirmed. Try again later." if isinstance(error, UncertainSend) else "Not sent: " + group_explain(error))
        self._work(lambda: client.retry_outbox(cmid), done, failed)

    def group_dismiss(self, gid, cmid):
        client = self.group_client()
        if not client:
            return
        self.confirm("Dismiss this message on this device?",
                     "If it already reached the group, it stays there; delete it for everyone from the conversation.",
                     lambda: self._work(lambda: client.dismiss_outbox(cmid),
                                        lambda _: self.render_group_view() if gid == self.group_open else None), "Dismiss")

    def group_open_file(self, gid, file):
        client = self.group_client()
        name = safe_filename(file.get("name") or "file")
        if not client:
            return
        read = lambda: client.open_file(gid, file)  # noqa: E731
        if viewer_kind(name, file.get("mime") or ""):
            return AttachmentViewer(self, name, file.get("mime") or "", read, lambda data: self.save_attachment({"filename": name}, data))

        def opened(data):
            self.save_attachment({"filename": name}, data)
        self._work(read, opened, lambda error: self.toast("Could not open the file: " + explain(error)))

    def group_delete_message(self, gid, seq):
        client = self.group_client()
        if not client:
            return

        def delete():
            # The postbox erases every version of this message; so does this device.
            client.delete_message(gid, seq)
            if self.group_mode and self.group_mode["gid"] == gid and self.group_mode["kind"] == "edit":
                GLib.idle_add(lambda: (self.cancel_group_mode() if self.group_mode and self.group_mode["kind"] == "edit" else None, False)[1])
        self.confirm("Delete this message for everyone?",
                     "Every version is removed from the postbox. Copies people already saw on their screens stay where they are.",
                     lambda: self._work(delete, lambda _: self.render_group_view(), lambda error: self.toast(explain(error))))

    # ---- calls: they run in the web inbox ------------------------------------------------------

    def group_call_open(self, video, join=False):
        g = self.group_by_id(self.group_open)
        if not g:
            return
        self.get_clipboard().set(g.get("name") or "")
        self.open_url("https://inbox.pigeonpost.dev/")
        if join:
            self.toast(f"Calls open in the web inbox. Open {g.get('name')} there and join the call.")
        else:
            self.toast(f"Calls open in the web inbox. Open {g.get('name')} there and start a {'video' if video else 'voice'} call.")

    def poll_group_call(self):
        gid, mailbox = self.group_open, self.identity
        self.group_call_polled = time.monotonic()
        transport = getattr(self.api, "transport", None)
        if not gid or transport is None:
            return

        def fetch():
            url = f"{CALLS}/groups/{quote(gid)}/call?from={urllib.parse.quote(mailbox, safe='')}"
            return transport.request("GET", url, token=self.session.access_token(), timeout=10)

        def done(state):
            if gid == self.group_open:
                self.group_call = state if isinstance(state, dict) else None
                self.render_group_view()
        self._work(fetch, done, lambda _: None)

    # ---- New group ------------------------------------------------------------------------------

    def new_group(self):
        if not self.groups_on():
            return
        window, content = self.dialog("New group", 560)
        chips = []
        as_model = [r for r in self.mailboxes]
        sending_as = Gtk.DropDown(model=Gtk.StringList.new([display_name(r.get("handle") or r["address"]) for r in as_model]))
        sending_as.set_selected(next((i for i, r in enumerate(as_model) if r["address"] == self.identity), 0))
        if len(as_model) > 1:
            content.append(label("Sending as", "heading"))
            content.append(sending_as)
        content.append(label("Who", "heading"))
        chip_row = Gtk.FlowBox(selection_mode=Gtk.SelectionMode.NONE, max_children_per_line=4)
        content.append(chip_row)
        who_row = box(6, horizontal=True)
        who = Gtk.Entry(placeholder_text="/alp/main or /k/…", hexpand=True)
        who.update_property([Gtk.AccessibleProperty.LABEL], ["Add a person by handle or address"])
        completion = Gtk.StringList.new([c["peer"] for c in self.contacts if c.get("peer") and "*" not in c["peer"]])
        who.contacts = completion
        who_row.append(who)
        content.append(who_row)
        error = label("", "error", wrap=True)

        def render_chips():
            while chip_row.get_first_child():
                chip_row.remove(chip_row.get_first_child())
            for address in chips:
                chip_row.append(button("Remove " + address, lambda a=address: (chips.remove(a), render_chips())))

        def add_chip():
            value = who.get_text().strip().rstrip(",")
            if not value:
                return True
            if not value.startswith("/"):
                value = "/" + value
            if not valid_peer(value):
                error.set_text("That does not look like a Pigeonpost address.")
                return False
            if value not in chips and len(chips) < 200:
                chips.append(value)
            who.set_text("")
            error.set_text("")
            render_chips()
            return True
        who.connect("activate", lambda _: add_chip())
        who_row.append(button("Add", add_chip))
        content.append(label("Name", "heading"))
        name = Gtk.Entry(placeholder_text="Garden club", max_length=80)
        content.append(name)
        content.append(label("Description (optional)", "heading"))
        description = Gtk.Entry(max_length=500)
        content.append(description)
        content.append(label("Who can join", "heading"))
        join_private = Gtk.CheckButton(label="Private — invite only", active=True)
        join_public = Gtk.CheckButton(label="Public — anyone with the link")
        join_public.set_group(join_private)
        content.append(join_private)
        content.append(join_public)
        content.append(label("Who can post", "heading"))
        post_everyone = Gtk.CheckButton(label="Everyone", active=True)
        post_admins = Gtk.CheckButton(label="Only admins")
        post_admins.set_group(post_everyone)
        content.append(post_everyone)
        content.append(post_admins)
        timer = None
        if DISAPPEARING in self.features:
            content.append(label("Disappearing messages", "heading"))
            timer = Gtk.DropDown(model=Gtk.StringList.new([timer_label(t) for t in TIMERS]))
            content.append(timer)
        content.append(label(E2EE_NOTE, "dim-label", wrap=True))
        content.append(error)
        create = button("Create group", lambda: submit(), style="suggested-action")
        content.append(create)
        content.append(Gtk.Separator())
        content.append(label("Have an invite link?", "heading"))
        link_row = box(6, horizontal=True)
        link = Gtk.Entry(placeholder_text="https://inbox.pigeonpost.dev/g/…", hexpand=True)
        link_row.append(link)
        link_row.append(button("Open link", lambda: self.join_by_link(link.get_text(), window)))
        content.append(link_row)

        def submit():
            if not add_chip():
                return
            title = name.get_text().strip()
            if not title:
                error.set_text("Give the group a name.")
                name.grab_focus()
                return
            mailbox = as_model[sending_as.get_selected()]["address"] if as_model else self.identity
            settings = {"name": title, "join_policy": "public" if join_public.get_active() else "private",
                        "post_policy": "admins" if post_admins.get_active() else "everyone", "visibility": "unlisted",
                        "invite": list(chips)}
            if description.get_text().strip():
                settings["description"] = description.get_text().strip()
            if timer is not None and TIMERS[timer.get_selected()]:
                settings["ephemeral_secs"] = TIMERS[timer.get_selected()]
            create.set_sensitive(False)
            current = self.identity

            def made(result):
                window.close()
                refused = [i for i in result.get("invitations") or [] if i.get("status") != "invited"]
                if refused:
                    self.toast(f"{len(refused)} {'person' if len(refused) == 1 else 'people'} could not be invited.")
                if mailbox == current:
                    self.groups.append(result["group"])
                    self.open_group(result["group"]["id"])
                    self.groups_cycle(force=True)
                else:
                    self.toast(f"Created as {display_name(mailbox)}. Switch to that mailbox to open it.")

            def failed(problem):
                create.set_sensitive(True)
                error.set_text(explain(problem))
            self._work(lambda: self.client_for(mailbox).create_group(settings), made, failed)
        window.new_group = {"who": who, "add": add_chip, "name": name, "submit": submit, "chips": chips, "public": join_public,
                            "admins": post_admins, "timer": timer, "error": error, "link": link}
        window.present()
        who.grab_focus()
        return window

    def join_by_link(self, text, parent=None):
        parsed = parse_invite_link(text)
        if not parsed:
            self.toast("That is not a Pigeonpost group link.")
            return
        gid, token = parsed
        mailbox = self.identity
        body = {"group_id": gid}
        if token:
            body["link_token"] = token

        def previewed(preview):
            if parent:
                parent.close()
            if preview.get("my_state") == "active":
                self.open_group(gid)
                return
            detail = f"{preview.get('member_count') or 1} members · joining as {display_name(self.title.get_subtitle() or mailbox)}.\n\n{E2EE_NOTE}"
            if preview.get("description"):
                detail = preview["description"] + "\n\n" + detail
            if not preview.get("can_join"):
                self.notice(preview.get("name") or "Group", "You have an invitation to this group: accept it from Invitations in your conversation list."
                            if preview.get("needs") == "invitation" else "This group is invite only.")
                return
            join_body = {"link_token": token} if token else {}

            def joined(_):
                self.toast("Joined. A member’s device will add you to the encryption in a moment.")
                self.groups_cycle(force=True)
            self.confirm(f"Join {preview.get('name') or 'this group'}?", detail,
                         lambda: self._work(lambda: self.api.scoped("POST", f"/v1/groups/{quote(gid)}/join", mailbox, join_body), joined,
                                            lambda error: self.toast(explain(error))), "Join")
        self._work(lambda: self.api.scoped("POST", "/v1/groups/resolve", mailbox, body), previewed,
                   lambda error: self.toast("This invite link is not valid any more." if getattr(error, "code", None) in ("not_found", "invalid_link")
                                            else explain(error)))

    # ---- Group info -----------------------------------------------------------------------------

    def group_info(self):
        g = self.group_by_id(self.group_open)
        if not g:
            return
        window, content = self.dialog("Group info", 600)
        window.set_default_size(600, 720)
        window.group_content = content
        self.group_info_window = window
        self.render_group_info(window)
        mailbox, gid = self.identity, g["id"]
        self._work(lambda: self.load_group_members(mailbox, gid), lambda _: self.render_group_info(window), lambda _: None)
        window.present()
        return window

    def render_group_info(self, window):
        content = window.group_content
        g = self.group_by_id(self.group_open)
        if not g:
            return
        clear(content)
        client = self.group_client()
        admin, owner = is_admin(g), (g.get("my") or {}).get("role") == "owner"
        members = self.group_members.get(g["id"], [])
        head = box(12, horizontal=True)
        head.append(Adw.Avatar.new(56, g.get("name") or "Group", True))
        names = box(4)
        names.append(label(g.get("name") or "Group", "title-2"))
        if g.get("description"):
            names.append(label(g["description"], "dim-label", wrap=True))
        head.append(names)
        content.append(head)

        if admin:
            content.append(label("Name and description", "heading"))
            name = Gtk.Entry(text=g.get("name") or "", max_length=80)
            description = Gtk.Entry(text=g.get("description") or "", max_length=500, placeholder_text="Description")
            content.append(name)
            content.append(description)
            content.append(button("Save name", lambda: self.patch_group(g, {"name": name.get_text().strip(), "description": description.get_text().strip() or None})))

        active = [m for m in members if m.get("state") == "active"]
        content.append(label(f"Members ({len(active)})", "heading"))
        listing = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
        listing.add_css_class("boxed-list")
        for member in members:
            if member.get("state") in ("active", "keying") or (admin and member.get("state") == "invited"):
                listing.append(self.member_row(g, member, client))
        if not members:
            listing.append(settings_action_row(title="Loading members…"))
        content.append(listing)
        if admin or (g.get("members_can_invite") and (g.get("my") or {}).get("state") == "active"):
            add = box(6, horizontal=True)
            entry = Gtk.Entry(placeholder_text="/alp/main", hexpand=True)
            entry.update_property([Gtk.AccessibleProperty.LABEL], ["Add people by handle or address"])
            add.append(entry)
            add.append(button("Add people", lambda: self.invite_to_group(g, entry.get_text())))
            content.append(add)

        if admin and g.get("join_policy") == "public":
            content.append(label("Invite link", "heading"))
            content.append(label("Anyone with the current link can join. Resetting makes the old link stop working." if g.get("has_link")
                                 else "Create a link anyone can use to join this public group.", "dim-label", wrap=True))
            out = box(6)
            content.append(out)
            links = box(6, horizontal=True)
            links.append(button("Reset link" if g.get("has_link") else "Create link", lambda: self.make_group_link(g, True, out)))
            if g.get("has_link"):
                links.append(button("Turn off link", lambda: self.make_group_link(g, False, out)))
            content.append(links)

        if admin:
            content.append(label("Settings", "heading"))
            join = Gtk.DropDown(model=Gtk.StringList.new(["Private — invite only", "Public — anyone with the link"]))
            join.set_selected(1 if g.get("join_policy") == "public" else 0)
            post = Gtk.DropDown(model=Gtk.StringList.new(["Everyone", "Only admins"]))
            post.set_selected(1 if g.get("post_policy") == "admins" else 0)
            invite = Gtk.CheckButton(label="Members can add people", active=bool(g.get("members_can_invite")))
            change_timer = Gtk.CheckButton(label="Members can change the timer", active=bool(g.get("members_can_change_timer")))
            content.append(label("Who can join", "dim-label"))
            content.append(join)
            content.append(label("Who can post", "dim-label"))
            content.append(post)
            content.append(invite)
            if DISAPPEARING in self.features:
                content.append(change_timer)

            def save_settings():
                fields = {"join_policy": "public" if join.get_selected() == 1 else "private",
                          "post_policy": "admins" if post.get_selected() == 1 else "everyone",
                          "members_can_invite": invite.get_active()}
                if DISAPPEARING in self.features:
                    fields["members_can_change_timer"] = change_timer.get_active()
                self.patch_group(g, fields)
            content.append(button("Save settings", save_settings))

        if DISAPPEARING in self.features:
            content.append(label("Disappearing messages", "heading"))
            may = admin or (g.get("members_can_change_timer") and (g.get("my") or {}).get("state") == "active")
            timer = Gtk.DropDown(model=Gtk.StringList.new([timer_label(t) for t in TIMERS]))
            timer.set_selected(TIMERS.index(g.get("ephemeral_secs")) if g.get("ephemeral_secs") in TIMERS else 0)
            timer.set_sensitive(bool(may))
            timer.connect("notify::selected", lambda d, _: self.patch_group(g, {"ephemeral_secs": TIMERS[d.get_selected()]})
                          if TIMERS[d.get_selected()] != (g.get("ephemeral_secs") or None) else None)
            content.append(label("New messages disappear after", "dim-label"))
            content.append(timer)
            content.append(label("The clock starts when a message is sent. " + LIMITS_NOTE, "dim-label", wrap=True))
            window.group_timer = timer

        content.append(label("Notifications", "heading"))
        my = g.get("my") or {}
        notify_values = ["all", "mentions", "none"]
        notify = Gtk.DropDown(model=Gtk.StringList.new(["All messages", "Only mentions", "Nothing"]))
        notify.set_selected(notify_values.index(my.get("notify")) if my.get("notify") in notify_values else 0)
        mute = Gtk.CheckButton(label="Mute", active=(my.get("muted_until") or 0) > time.time())

        def save_notify(*_):
            self.set_group_notifications(g, notify_values[notify.get_selected()], FOREVER if mute.get_active() else None)
        notify.connect("notify::selected", save_notify)
        mute.connect("toggled", save_notify)
        content.append(notify)
        content.append(mute)
        content.append(label("Notifications say only “New message”: the postbox cannot read group messages.", "dim-label", wrap=True))

        content.append(label("Encryption", "heading"))
        content.append(label(SAFETY_NOTE, "dim-label", wrap=True))
        safety = client.safety_number(g["id"]) if client else None
        number = label(spaced(safety) if safety else "Not available on this device yet.", "pp-mono", wrap=True)
        number.set_selectable(True)
        content.append(number)
        window.safety_label = number
        content.append(label("This device: " + ((client.fingerprint() or "") if client else ""), "dim-label", wrap=True))
        content.append(label(HOSTED_NOTE, "dim-label", wrap=True))

        content.append(label("Membership", "heading"))
        if owner:
            admins = [m for m in members if m.get("role") == "admin" and m.get("state") == "active"]
            if admins:
                pick = Gtk.DropDown(model=Gtk.StringList.new([self.name_of(m["address"]) for m in admins]))
                content.append(pick)
                content.append(button("Transfer ownership", lambda: self.transfer_group(g, admins[pick.get_selected()]["address"])))
            else:
                content.append(label("To leave, first make another member an admin and transfer ownership to them.", "dim-label", wrap=True))
        content.append(button("Leave group", lambda: self.leave_group(g), style="destructive-action"))
        if owner:
            content.append(button("Delete group", lambda: self.delete_group(g), style="destructive-action"))

    def member_row(self, g, member, client):
        address = member["address"]
        self_row = address == self.identity
        key = member.get("handle") or address
        title = (display_name(member.get("handle") or self.title.get_subtitle() or address) + " (you)") if self_row else self.name_of(address)
        state = member.get("state")
        badge = ROLE_LABEL.get(member.get("role"), "Member") if state == "active" else "Invited" if state == "invited" else "Joining"
        row = plain_row(Adw.ExpanderRow())
        row.set_title(title)
        row.set_subtitle(badge)
        row.add_prefix(self.avatars.widget(key, self.name_of(address), 32))
        row.member = member
        admin, owner = is_admin(g), (g.get("my") or {}).get("role") == "owner"
        actions = box(6, horizontal=True, margin=8)
        actions.set_halign(Gtk.Align.START)

        def act(text, change):
            actions.append(button(text, lambda: self.change_member(g, member, change)))
        if state == "invited" and admin:
            act("Revoke", {"state": "removed"})
        if state == "active" and not self_row and member.get("role") != "owner":
            if admin and member.get("role") != "admin":
                act("Make admin", {"role": "admin"})
            if owner and member.get("role") == "admin":
                act("Remove admin", {"role": "member"})
            if admin and member.get("role") == "member":
                act("Restrict", {"role": "restricted"})
            if admin and member.get("role") == "restricted":
                act("Unrestrict", {"role": "member"})
            if admin and (member.get("role") != "admin" or owner):
                act("Remove", {"state": "removed"})
                act("Ban", {"state": "banned"})
        if self_row and member.get("role") == "admin":
            act("Step down as admin", {"role": "member"})
        if not self_row:
            actions.append(button("Block", lambda: self.ask_block(key, self.name_of(address))))
        if actions.get_first_child():
            holder = Gtk.ListBoxRow(activatable=False)
            holder.set_child(actions)
            row.add_row(holder)
        if state == "active":
            devices_row = settings_action_row(title="Devices", subtitle="Loading…")
            devices_row.set_subtitle_lines(0)
            row.add_row(devices_row)
            row.connect("notify::expanded", lambda r, _: self.load_member_devices(address, devices_row, client) if r.get_expanded() else None)
            row.devices_row = devices_row
        return row

    def load_member_devices(self, address, row, client):
        mailbox = self.identity

        def fetch():
            return self.api.scoped("GET", "/v1/mls/devices?mailbox=" + urllib.parse.quote(address, safe=""), mailbox).get("devices") or []

        def shown(devices):
            self.group_devices[address] = devices
            if not devices:
                row.set_subtitle("No devices visible.")
                return
            lines = [f"{d.get('device_id')}: {d.get('fingerprint_digits') or d.get('fingerprint')}" for d in devices]
            fingerprints = [str(d.get("fingerprint") or "") for d in devices]
            status = client.verification(address, fingerprints) if client else None
            if status == "changed":
                lines.insert(0, "⚠ Devices changed since you verified them. Compare fingerprints again.")
            elif status == "verified":
                lines.insert(0, "✓ You verified these devices.")
            row.set_subtitle("\n".join(lines))
            if client and address != self.identity and status != "verified" and not getattr(row, "verify_button", None):
                verify = button("Mark as verified", lambda: (client.verify_devices(address, fingerprints), shown(devices)))
                verify.set_valign(Gtk.Align.CENTER)
                row.add_suffix(verify)
                row.verify_button = verify
        self._work(fetch, shown, lambda _: row.set_subtitle("Devices are not visible."))

    def refresh_group_info(self):
        window = getattr(self, "group_info_window", None)
        if window is not None and window.get_visible():
            self.render_group_info(window)

    def patch_group(self, g, fields):
        mailbox = self.identity

        def patch():
            for attempt in range(2):
                fresh = self.api.scoped("GET", f"/v1/groups/{quote(g['id'])}", mailbox)
                revision = (fresh.get("group") or fresh).get("revision") or g.get("revision")
                try:
                    result = self.api.scoped("PATCH", f"/v1/groups/{quote(g['id'])}", mailbox, {"expected_revision": revision, **fields})
                    return result.get("group") or result
                except APIError as exc:
                    if exc.code == "revision_conflict" and attempt == 0:
                        continue
                    raise

        def saved(group):
            g.update(group or {})
            self.toast("Saved.")
            self.render_group_view()
            self.refresh_group_info()
            self.groups_cycle(force=True)
        self._work(patch, saved, lambda error: self.toast(explain(error)))

    def change_member(self, g, member, change):
        mailbox, client = self.identity, self.group_client()
        destructive = change.get("state") in ("removed", "banned")

        def run():
            self.api.scoped("PATCH", f"/v1/groups/{quote(g['id'])}/members/{urllib.parse.quote(member['address'], safe='')}", mailbox,
                            {"expected_revision": member.get("revision"), **change})
            if destructive and client:
                try:
                    client.process_pending(g["id"], no_jitter=True)
                except Exception:
                    pass
            return self.load_group_members(mailbox, g["id"])

        def go():
            self._work(run, lambda _: (self.refresh_group_info(), self.groups_cycle(force=True)), lambda error: self.toast(explain(error)))
        if destructive:
            self.confirm("Remove and ban?" if change["state"] == "banned" else "Remove from the group?",
                         f"{self.name_of(member['address'])} loses access at once. Messages they already saw stay on their devices.",
                         go, "Ban" if change["state"] == "banned" else "Remove")
        else:
            go()

    def invite_to_group(self, g, raw):
        people = [v if v.startswith("/") else "/" + v for v in raw.replace(",", " ").split() if v]
        if not people:
            return
        mailbox = self.identity

        def invite():
            from .groups_core import new_request_key
            result = self.api.scoped("POST", f"/v1/groups/{quote(g['id'])}/members", mailbox, {"request_key": new_request_key(), "invite": people})
            self.load_group_members(mailbox, g["id"])
            return result

        def invited(result):
            ok = len([r for r in result.get("results") or [] if r.get("status") == "invited"])
            self.toast("Invited." if ok == len(people) else f"{ok} of {len(people)} invited.")
            self.refresh_group_info()
        self._work(invite, invited, lambda error: self.toast(explain(error)))

    def make_group_link(self, g, enabled, out):
        mailbox = self.identity

        def made(result):
            clear(out)
            if result.get("url"):
                field = Gtk.Entry(text=result["url"], editable=False)
                out.append(field)
                out.append(button("Copy link", lambda: (self.get_clipboard().set(result["url"]), self.toast("Link copied."))))
                out.append(label("Shown once. Anyone with this link can join while it is on.", "dim-label", wrap=True))
            g["has_link"] = bool(result.get("link_token"))
            out.link_result = result
        self._work(lambda: self.api.scoped("POST", f"/v1/groups/{quote(g['id'])}/link", mailbox, {"enabled": enabled}), made,
                   lambda error: self.toast(explain(error)))

    def set_group_notifications(self, g, notify, muted_until):
        mailbox = self.identity

        def saved(_):
            my = g.setdefault("my", {})
            my["notify"], my["muted_until"] = notify, muted_until
            self.render_peers()
        self._work(lambda: self.api.scoped("PUT", f"/v1/groups/{quote(g['id'])}/notifications", mailbox,
                                           {"notify": notify, "muted_until": muted_until}), saved, lambda error: self.toast(explain(error)))

    def transfer_group(self, g, to):
        mailbox = self.identity
        self.confirm("Transfer ownership?", f"{self.name_of(to)} becomes the owner. You stay an admin.",
                     lambda: self._work(lambda: self.api.scoped("POST", f"/v1/groups/{quote(g['id'])}/transfer", mailbox,
                                                                {"to": to, "expected_revision": g.get("revision")}),
                                        lambda _: (self.toast("Ownership transferred."), self.groups_cycle(force=True)),
                                        lambda error: self.toast(explain(error))), "Transfer")

    def leave_group(self, g):
        mailbox, client = self.identity, self.group_client()

        def leave():
            self.api.scoped("POST", f"/v1/groups/{quote(g['id'])}/leave", mailbox)
            if client:
                client.forget(g["id"])

        def left(_):
            self.close_group_info()
            self.groups = [x for x in self.groups if x.get("id") != g["id"]]
            self.group_open = None
            self.show_page("conversation")
            self.render_all()
            self.groups_cycle(force=True)
        self.confirm("Leave this group?", "You stop receiving its messages. To come back you need a new invitation.",
                     lambda: self._work(leave, left, lambda error: self.toast(explain(error))), "Leave")

    def delete_group(self, g):
        mailbox, client = self.identity, self.group_client()

        def delete():
            self.api.scoped("DELETE", f"/v1/groups/{quote(g['id'])}", mailbox)
            if client:
                client.forget(g["id"])
        self.confirm("Delete this group for everyone?", "Members see that it was deleted and its messages are removed from the postbox.",
                     lambda: self._work(delete, lambda _: (self.close_group_info(), setattr(self, "group_open", None), self.show_page("conversation"),
                                                           self.groups_cycle(force=True)),
                                        lambda error: self.toast(explain(error))), "Delete group")

    def close_group_info(self):
        window = getattr(self, "group_info_window", None)
        if window is not None:
            window.close()
        self.group_info_window = None

    # ---- sign-out ------------------------------------------------------------------------------

    def retire_group_devices(self, mailboxes):
        """Worker thread, before the session ends: revoke this computer's group devices and delete
        their keys, including mailboxes whose device was not opened in this run."""
        gc = groupcrypto.load()
        if gc is None:
            return
        storage = self.group_storage_for(gc)
        for mailbox in mailboxes:
            client = self.group_clients.get(mailbox)
            try:
                if client:
                    client.revoke()
                    continue
                record = storage.load(mailbox)
                if record and record.get("registered") and record.get("device_id"):
                    try:
                        self.api.scoped("DELETE", "/v1/mls/devices/" + quote(record["device_id"]), mailbox, device=record["device_id"])
                    except Exception:
                        pass
                storage.remove(mailbox)
            except Exception:
                pass
        self.group_clients = {}
