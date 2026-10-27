"""Direct-message actions in the window (SDS §14C, docs/message-actions.md §6): reply and edit
modes, ticks, message info, read receipts, protected drafts and unconfirmed-delivery review.

Every protected write runs on the login lease that started it (window.bound_work), so a send that
finishes after the person switched mailbox or conversation still clears the right draft, and one
that finishes after a sign-out or another account's sign-in is dropped instead of being shown.
"""

import datetime

import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk

from .actions import (AGENT_STATUS, MESSAGE_EDIT, MESSAGE_REPLY, RECEIPTS, can_reply, editable, quote_text, ref_of,
                      seconds, tick_state)
from .api import APIError
from .binding import Retired
from .delivery import Coordinator, DeliveryConflict, DeliveryLimit, load_drafts, save_draft, uncertain
from .message_ui import action_row, highlight, mode_bar, quote_block, tick_widget
from .model import contact_for, display_name, message_text, subjects, target_thread
from .protected import MemoryProtectedStore, ProtectedUnavailable
from .widgets import box, button, clear, label

DRAFT_SAVE_MS = 800


class MessagesMixin:
    # ---- state ---------------------------------------------------------------------------------

    def reset_messages(self):
        self.compose_modes = {}       # draft key → {"kind": "reply"|"edit", "message": …, "saved": draft text}
        self.pending = {}             # mailbox → [delivery.Attempt] (unconfirmed deliveries)
        self.draft_timers = {}
        self.message_widgets = {}
        self.memory_work = getattr(self, "memory_work", None) or MemoryProtectedStore()

    def actions_on(self):
        return bool({MESSAGE_EDIT, MESSAGE_REPLY, RECEIPTS, AGENT_STATUS} & self.features)

    def read_inbox(self, identity, wait=0):
        """`actions=1&fold=1` once the postbox offers message actions; the legacy view otherwise."""
        if self.actions_on():
            return self.api.inbox(identity, wait, actions=True)
        return self.api.inbox(identity, wait)

    def work_store(self, lease):
        """The sealed store for a signed-in account; memory only when it cannot be used."""
        if lease is not None and lease.owner and getattr(self.protected, "available", False):
            return self.protected
        return self.memory_work

    def delivery(self, lease, mailbox):
        return Coordinator(self.work_store(lease), self.api_for(lease), lease, mailbox, self.features)

    # ---- protected drafts ----------------------------------------------------------------------

    def load_saved_work(self):
        """After choosing a mailbox: its saved drafts and unconfirmed deliveries."""
        lease, identity = self.mailbox_lease, self.identity
        if lease is None or not identity:
            return
        store = self.work_store(lease)

        def read():
            drafts = load_drafts(store, lease, identity)
            return drafts, self.delivery(lease, identity).pending()

        def loaded(result):
            drafts, pending = result
            for (peer, subject), text in drafts.items():
                self.drafts.setdefault((identity, peer, subject), text)
            self.pending[identity] = pending
            if identity == self.identity:
                self.restore_draft()
                self.render_all()

        def failed(error):
            if identity == self.identity:
                self.toast("Saved drafts and unconfirmed messages could not be opened. Unlock your desktop keyring.")
        self.bound_work(lease, read, loaded, failed)

    def schedule_draft_save(self, key):
        lease = self.mailbox_lease
        if lease is None or key[0] != lease.mailbox or key[1] is None:
            return
        timer = self.draft_timers.pop(key, None)
        if timer:
            GLib.source_remove(timer)

        def fire():
            self.draft_timers.pop(key, None)
            text = self.drafts.get(key, "")
            store = self.work_store(lease)
            self.bound_work(lease, lambda: save_draft(store, lease, key[0], key[1], key[2], text), None, lambda _: None)
            return False
        self.draft_timers[key] = GLib.timeout_add(DRAFT_SAVE_MS, fire)

    def forget_saved_draft(self, lease, key):
        store = self.work_store(lease)
        self.bound_work(lease, lambda: save_draft(store, lease, key[0], key[1], key[2], ""), None, lambda _: None)

    # ---- reply and edit modes ------------------------------------------------------------------

    def start_reply(self, message):
        if not can_reply(message, self.features) or not self.peer:
            return
        key = self.draft_key()
        mode = self.compose_modes.get(key)
        if mode and mode["kind"] == "edit":
            self.cancel_mode()
        self.compose_modes[key] = {"kind": "reply", "message": message}
        self.render_mode()
        self.composer.grab_focus()

    def start_edit(self, message):
        if not editable(message, self.features) or not self.peer:
            return
        key = self.draft_key()
        current = self.compose_modes.get(key)
        saved = current["saved"] if current and current["kind"] == "edit" else self.drafts.get(key, "")
        self.compose_modes[key] = {"kind": "edit", "message": message, "saved": saved}
        self.rendering = True
        self.composer.get_buffer().set_text(message_text(message))
        self.rendering = False
        self.drafts[key] = message_text(message)
        self.render_mode()
        self.update_send()
        self.composer.grab_focus()

    def cancel_mode(self):
        key = self.draft_key()
        mode = self.compose_modes.pop(key, None)
        if mode and mode["kind"] == "edit":
            self.drafts[key] = mode.get("saved") or ""
            if not self.drafts[key]:
                self.drafts.pop(key)
            self.restore_draft()
        self.render_mode()
        self.update_send()

    def render_mode(self):
        clear(self.mode_holder)
        mode = self.compose_modes.get(self.draft_key())
        if not mode:
            self.mode_holder.set_visible(False)
            return
        message = mode["message"]
        if mode["kind"] == "edit":
            bar = mode_bar("Editing message", quote_text(message_text(message)), self.cancel_mode)
        else:
            who = "yourself" if message.get("direction") == "out" else (
                contact_for(self.peer, self.contacts).get("alias") or display_name(self.peer))
            bar = mode_bar("Replying to " + who, quote_text(message_text(message)), self.cancel_mode)
        self.mode_holder.append(bar)
        self.mode_holder.set_visible(True)

    # ---- bubbles -------------------------------------------------------------------------------

    def message_meta(self, message, outgoing, who, suffix):
        """Sender, time, "edited", the disappearing timer and, on sent messages, the ticks."""
        meta = box(6, horizontal=True)
        name = label(who + "   " + suffix, "pp-meta")
        if not outgoing:
            name.add_css_class("pp-mono")
        meta.append(name)
        if message.get("edited_at") or message.get("edit_count"):
            edited = label("edited", "pp-edited")
            edited.set_tooltip_text("Edited — open Message info for the versions")
            meta.append(edited)
        if message.get("expires_at"):
            timer = Gtk.Image(icon_name="alarm-symbolic")
            try:
                timer.set_tooltip_text("Disappears " + datetime.datetime.fromtimestamp(seconds(message["expires_at"])).strftime("%d %b %H:%M"))
            except (ValueError, OSError, OverflowError, TypeError):
                pass
            meta.append(timer)
        if outgoing and (RECEIPTS in self.features or AGENT_STATUS in self.features or message.get("pending")):
            state, text = tick_state(message)
            meta.append(tick_widget(state, text))
        return meta

    def reply_header(self, message):
        reply = message.get("reply_to")
        if not isinstance(reply, dict):
            return None
        mine = reply.get("direction") == "out"
        sender = "You" if mine else (contact_for(self.peer, self.contacts).get("alias") or display_name(self.peer or reply.get("from") or ""))
        text = reply.get("excerpt") or reply.get("quote") or "Message unavailable"
        text = quote_text(str(text)) or "Message unavailable"
        target = reply.get("msg_ref") or reply.get("message_id")
        return quote_block(sender, text, (lambda: self.jump_to(target)) if target else None)

    def jump_to(self, reference):
        widget = self.message_widgets.get(reference)
        if widget is None:
            self.toast("The original message isn’t shown here. It may be in another subject or deleted.")
            return
        ok, bounds = widget.compute_bounds(self.message_list)
        if ok:
            adj = self.message_scroll.get_vadjustment()
            adj.set_value(max(0, bounds.get_y() - 40))
        highlight(widget)
        widget.grab_focus()

    def bubble_actions(self, message, outgoing):
        """Below and outside the bubble: received → copy, reply; sent → edit (while editable), copy.
        A More menu keeps the full list (as the right-click menu does)."""
        buttons = []
        if outgoing and editable(message, self.features):
            buttons.append(("Edit message", "document-edit-symbolic", lambda m=message: self.start_edit(m)))
        buttons.append(("Copy message", "edit-copy-symbolic", lambda m=message: self.get_clipboard().set(message_text(m))))
        if not outgoing and can_reply(message, self.features):
            buttons.append(("Reply", "mail-reply-sender-symbolic", lambda m=message: self.start_reply(m)))
        row = action_row(outgoing, buttons)
        more = Gtk.MenuButton(icon_name="view-more-symbolic")
        more.add_css_class("flat")
        more.set_tooltip_text("More actions")
        more.update_property([Gtk.AccessibleProperty.LABEL], ["More actions"])
        more.set_menu_model(self.message_menu(message, outgoing))
        if outgoing:
            row.prepend(more)
        else:
            row.append(more)
        return row

    def message_menu(self, message, outgoing):
        menu = Gio.Menu()
        if can_reply(message, self.features):
            menu.append("Reply", "message.reply")
        menu.append("Copy message", "message.copy")
        if outgoing and editable(message, self.features):
            menu.append("Edit message", "message.edit")
        if self.actions_on() and message.get("message_id"):
            menu.append("Message info", "message.info")
        menu.append("Delete message", "message.delete")
        if not outgoing:
            menu.append("Report spam", "message.report")
            menu.append("Block sender", "message.block")
        return menu

    def context_menu(self, bubble, menu):
        """Right-click (or long-press) anywhere on the bubble opens the full menu. The popover is
        made only when asked for and unparented when it closes, so re-rendering never leaves one
        attached to a removed bubble."""
        def show(x, y):
            if not bubble.get_root():
                return
            popover = Gtk.PopoverMenu.new_from_model(menu)
            popover.set_parent(bubble)
            popover.set_has_arrow(False)
            rect = Gdk.Rectangle()
            rect.x, rect.y, rect.width, rect.height = int(x), int(y), 1, 1
            popover.set_pointing_to(rect)
            popover.connect("closed", lambda widget: GLib.idle_add(lambda: (widget.unparent() if widget.get_parent() else None, False)[1]))
            popover.popup()
            bubble.context_popover = popover
        click = Gtk.GestureClick(button=3)
        click.connect("pressed", lambda _g, _n, x, y: show(x, y))
        bubble.add_controller(click)
        press = Gtk.GestureLongPress()
        press.connect("pressed", lambda _g, x, y: show(x, y))
        bubble.add_controller(press)

    def extra_actions(self, message, outgoing, handlers):
        if can_reply(message, self.features):
            handlers["reply"] = lambda: self.start_reply(message)
        if outgoing and editable(message, self.features):
            handlers["edit"] = lambda: self.start_edit(message)
        if self.actions_on() and message.get("message_id"):
            handlers["info"] = lambda: self.message_info(message)

    # ---- message info --------------------------------------------------------------------------

    def message_info(self, message):
        identity, mid = self.identity, message.get("message_id")
        if not identity or not mid:
            return
        window, content = self.dialog("Message info", 480)
        status = label("Loading…", "dim-label", wrap=True)
        content.append(status)

        def when(value):
            value = seconds(value)
            try:
                return datetime.datetime.fromtimestamp(value).strftime("%d %b %Y · %H:%M") if value else "—"
            except (ValueError, OSError, OverflowError):
                return "—"

        def shown(info):
            status.set_text("")
            direction = "Sent" if info.get("direction") == "out" else "Received"
            content.append(label(f"{direction} {when(info.get('sent_at'))}", "heading"))
            if info.get("direction") == "out":
                state, text = tick_state(info)
                content.append(label(text if state != "sent" else "Sent — not delivered yet", "dim-label"))
                receipt = info.get("receipt") if isinstance(info.get("receipt"), dict) else {}
                if receipt.get("delivered_at"):
                    content.append(label("Delivered " + when(receipt["delivered_at"]), "dim-label"))
                if receipt.get("seen_at"):
                    content.append(label("Seen " + when(receipt["seen_at"]), "dim-label"))
            versions = [v for v in info.get("versions") or [] if isinstance(v, dict)]
            if len(versions) > 1:
                content.append(label("Versions", "heading"))
                for index, version in enumerate(versions):
                    title = "Original" if index == 0 else f"Edit {index}"
                    row = box(2)
                    row.append(label(f"{title} · {when(version.get('at'))}", "pp-meta"))
                    text = label(message_text({"body": version.get("body") or ""}), "pp-body", wrap=True)
                    text.set_selectable(True)
                    text.set_use_markup(False)
                    row.append(text)
                    content.append(row)
            window.info = info

        def failed(error):
            status.set_text(self.error_text(error) if not getattr(error, "code", "") == "not_found"
                            else "This message is no longer in your mailbox.")
        self._work(lambda: self.api.call("GET", f"/v1/messages/{GLib.uri_escape_string(mid, None, False)}/info", identity),
                   shown, failed)
        window.present()
        return window

    # ---- sending -------------------------------------------------------------------------------

    def send_protected(self):
        """Send or edit through the delivery coordinator (see delivery.py)."""
        key = self.draft_key()
        identity, peer, selected = key
        lease = self.mailbox_lease
        if lease is None or lease.mailbox != identity:
            return
        body = self.drafts.get(key, "").strip()
        files = list(self.files.get(key, []))
        mode = self.compose_modes.get(key)
        rows = subjects(peer, self.messages, self.threads)
        thread = target_thread(selected, rows)
        label_text = next((r.get("title") for r in rows if (r.get("thread_id") or None) == thread), None)
        options = {"peer": peer, "subject_label": label_text}
        if mode and mode["kind"] == "edit":
            original = mode["message"]
            options.update(thread=original.get("thread_id") or None, edit_of=ref_of(original))
            files = []
        else:
            options["thread"] = thread
            if mode and mode["kind"] == "reply" and MESSAGE_REPLY in self.features:
                parent = mode["message"]
                options.update(reply_to=ref_of(parent), quote=quote_text(message_text(parent)) or None)
                if selected is None and parent.get("thread_id"):
                    options["thread"] = parent["thread_id"]  # from All messages, answer in the parent's subject
        coordinator = self.delivery(lease, identity)
        self.sending = True
        self.update_send()

        def run():
            coordinator.check_free(peer, options.get("thread"))
            attachments = []
            for file in files:
                if not file.get("uploaded"):
                    file["uploaded"] = coordinator.api.upload(identity, file["name"], file["media_type"], file["content"])
                attachments.append(file["uploaded"]["id"])
            attempt = coordinator.prepare(peer, body, attachments=attachments, **options)
            try:
                return "sent", coordinator.send(attempt), attempt
            except Exception as error:
                if uncertain(error) and not isinstance(error, Retired):
                    return "uncertain", error, attempt
                raise

        def done(outcome):
            kind, _, attempt = outcome
            if identity == self.identity:
                self.sending = False
            if kind == "uncertain":
                self.pending.setdefault(identity, [])
                self.pending[identity] = [a for a in self.pending[identity] if a.slot != attempt.slot] + [attempt]
                self.update_send()
                self.render_all()
                self.notice("Message not confirmed",
                            "Pigeonpost didn’t confirm this message; it may already have been delivered. It is saved on this "
                            "computer: review it in the conversation and choose Retry (it won’t be sent twice) or Discard.")
                return
            self.revision += 1
            current = self.compose_modes.get(key)
            if current is mode and mode and mode["kind"] == "edit":
                # The edit's text was never the draft: give back what was being written before.
                self.drafts[key] = mode.get("saved") or ""
                if not self.drafts[key]:
                    self.drafts.pop(key)
                self.compose_modes.pop(key, None)
            elif self.drafts.get(key, "").strip() == body:
                self.drafts.pop(key, None)
                self.forget_saved_draft(lease, key)
                if current is mode:
                    self.compose_modes.pop(key, None)
            self.files.pop(key, None)
            if key == self.draft_key():
                self.restore_draft()
                self.render_mode()
                self.keep_composer_focus(key)
            self.toast("Message edited" if options.get("edit_of") else "Message sent")
            if identity == self.identity:
                self.reload_messages()

        def failed(error):
            if identity == self.identity:
                self.sending = False
                self.update_send()
            if key == self.draft_key():
                self.keep_composer_focus(key)
            if isinstance(error, (DeliveryConflict, DeliveryLimit)):
                self.notice("Unconfirmed message waiting", str(error))
            elif isinstance(error, ProtectedUnavailable):
                self.notice("Message not sent", str(error) + " Your draft is still here.")
            elif isinstance(error, ValueError):
                self.notice("Message not sent", str(error))
            else:
                self.notice("Message not sent", self.error_text(error) + " Your draft is still here.")
        self.bound_work(lease, run, done, failed)

    # ---- unconfirmed deliveries ----------------------------------------------------------------

    def pending_here(self):
        """Unconfirmed deliveries in the open conversation (and subject, unless All messages)."""
        rows = self.pending.get(self.identity) or []
        return [a for a in rows if a.peer == self.peer and (self.subject is None or (a.thread or "") == (self.subject or ""))]

    def pending_bubble(self, attempt):
        holder = box(4)
        holder.set_margin_start(64)
        bubble = box(6)
        bubble.add_css_class("pp-bubble")
        bubble.add_css_class("pp-mine")
        inner = box(8, margin=14)
        inner.set_margin_start(16)
        inner.set_margin_end(16)
        meta = box(6, horizontal=True)
        meta.append(label("You   " + ("edit · " if attempt.is_edit else "") + "not confirmed", "pp-meta"))
        meta.append(tick_widget("sending", "Not confirmed"))
        inner.append(meta)
        text = label(attempt.body, "pp-body", wrap=True)
        text.set_selectable(True)
        text.set_use_markup(False)
        inner.append(text)
        bubble.append(inner)
        holder.append(bubble)
        note = label("Pigeonpost didn’t confirm this message. Retry sends exactly this text, and never twice.", "pp-status", wrap=True)
        note.set_halign(Gtk.Align.END)
        holder.append(note)
        holder.append(action_row(True, [
            ("Discard unconfirmed message", "user-trash-symbolic", lambda a=attempt: self.discard_attempt(a)),
            ("Retry sending", "view-refresh-symbolic", lambda a=attempt: self.retry_attempt(a)),
        ]))
        holder.attempt = attempt
        return holder

    def retry_attempt(self, attempt):
        lease, identity = self.mailbox_lease, self.identity
        if lease is None or attempt.request["from"] != identity:
            return
        coordinator = self.delivery(lease, identity)

        def done(result):
            self.pending[identity] = [a for a in self.pending.get(identity, []) if a.slot != attempt.slot]
            key = (identity, attempt.peer, attempt.thread if attempt.thread else None)
            for draft_key in (key, (identity, attempt.peer, None), (identity, attempt.peer, attempt.thread or "")):
                if self.drafts.get(draft_key, "").strip() == attempt.body.strip():
                    self.drafts.pop(draft_key, None)
                    self.forget_saved_draft(lease, draft_key)
            self.revision += 1
            self.restore_draft()
            self.toast("Message confirmed")
            if identity == self.identity:
                self.reload_messages()
                self.render_all()

        def failed(error):
            if not uncertain(error):
                self.pending[identity] = [a for a in self.pending.get(identity, []) if a.slot != attempt.slot]
                self.render_all()
                self.notice("Message not sent", self.error_text(error) + " It was not delivered; its text is still in your draft.")
                key = (identity, attempt.peer, attempt.thread or None)
                if not self.drafts.get(key, "").strip():
                    self.drafts[key] = attempt.body
                    self.restore_draft()
            else:
                self.toast("Still not confirmed. Check your connection and try again.")
        self.bound_work(lease, lambda: coordinator.retry(attempt), done, failed)

    def discard_attempt(self, attempt):
        lease, identity = self.mailbox_lease, self.identity
        if lease is None or attempt.request["from"] != identity:
            return
        coordinator = self.delivery(lease, identity)

        def discard():
            def done(_):
                self.pending[identity] = [a for a in self.pending.get(identity, []) if a.slot != attempt.slot]
                self.render_all()
                self.toast("Removed from this computer")
            self.bound_work(lease, lambda: coordinator.discard(attempt), done)
        self.confirm("Discard this unconfirmed message?",
                     "This only removes the copy saved on this computer. If Pigeonpost already delivered it, it stays delivered.",
                     discard, "Discard")

    def review_deliveries(self):
        """Every unconfirmed delivery of this mailbox, including other conversations."""
        identity = self.identity
        rows = self.pending.get(identity) or []
        window, content = self.dialog("Unconfirmed messages", 520)
        content.append(label("These messages weren’t confirmed. Retry sends exactly the saved text and is never delivered twice. "
                             "Discard only removes the copy on this computer.", wrap=True))
        for attempt in rows:
            row = box(4)
            row.append(label(f"To {display_name(attempt.peer)}" + (f" · {attempt.review.get('subject')}" if attempt.review.get("subject") else ""),
                             "heading", wrap=True))
            body = label(attempt.body[:400], "pp-body", wrap=True)
            body.set_use_markup(False)
            row.append(body)
            actions = box(6, horizontal=True)
            actions.append(button("Open conversation", lambda a=attempt: (window.close(), self.open_conversation(a.peer))))
            actions.append(button("Retry", lambda a=attempt: (window.close(), self.retry_attempt(a)), style="suggested-action"))
            actions.append(button("Discard", lambda a=attempt: (window.close(), self.discard_attempt(a))))
            row.append(actions)
            content.append(row)
        if not rows:
            content.append(label("Nothing is waiting.", "dim-label"))
        window.present()
        return window

    def render_pending_chrome(self):
        count = len(self.pending.get(self.identity) or [])
        self.recovery_button.set_visible(bool(count))
        self.recovery_button.label.set_text(f"Unconfirmed ({count})")
        self.recovery_button.update_property([Gtk.AccessibleProperty.LABEL], [f"{count} unconfirmed messages: review"])

    # ---- receipts ------------------------------------------------------------------------------

    def ack_ids(self, identity, ids):
        """Acknowledge read messages; with folded edits every version is acknowledged together."""
        acked = []
        for mid in ids:
            data = {"message_id": mid}
            if self.actions_on():
                data["with_edits"] = True
            try:
                self.api.call("POST", "/v1/ack", identity, data=data)
                acked.append(mid)
            except APIError:
                break
        return acked

    def receipts_switch(self, state, save):
        """Settings → Privacy: 'Send read receipts' (per mailbox, default on)."""
        section = box(6)
        row = box(12, horizontal=True)
        title = label("Send read receipts", "heading")
        title.set_hexpand(True)
        row.append(title)
        switch = Gtk.Switch(valign=Gtk.Align.CENTER)
        policy = state.get("policy")
        switch.set_active(bool(policy.get("read_receipts", True)) if policy else True)
        switch.set_sensitive(policy is not None)
        switch.update_property([Gtk.AccessibleProperty.LABEL], ["Send read receipts"])
        switch.connect("notify::active", lambda widget, _: save("read_receipts", widget.get_active()))
        row.append(switch)
        section.append(row)
        section.append(label("Off: people you write to won’t see when you’ve read their messages, and you won’t see when they’ve "
                             "read yours. Delivered ticks always show.", "dim-label", wrap=True))
        switch.set_name("policy-read_receipts")
        return section
