"""Call history (SDS §12), the Filtered list (§13.4) and disappearing one-to-one timers (§17).
Mixed into the main window; the web inbox is the reference. Calls themselves open the web inbox
(WebKitGTK has no WebRTC), but their history is native."""

from gi.repository import Gio, GLib, Gtk, Pango

from .features import (CALL_HISTORY, DISAPPEARING, LIMITS_NOTE, MESSAGE_FILTERING, TIMERS, call_detail, call_outcome_label,
                       call_peer_key, direct_thread, is_missed, kind_word, list_time, timer_label)
from .model import contact_for, display_name, message_text
from .widgets import action_row, box, button, clear, label, scroll

CALL_PAGE = 50


class CallsMixin:
    # ---- pages ---------------------------------------------------------------------------------

    def build_calls_page(self):
        page = box(0)
        page.add_css_class("pp-wash")
        head = box(8, horizontal=True, margin=12)
        head.add_css_class("pp-toolbar")
        title = label("Calls", "pp-title")
        title.set_hexpand(True)
        head.append(title)
        self.calls_filter = {}
        strip = box(0, horizontal=True)
        strip.add_css_class("linked")
        first = None
        for value, text in (("all", "All"), ("missed", "Missed")):
            toggle = Gtk.ToggleButton(label=text, active=value == "all")
            if first:
                toggle.set_group(first)
            first = first or toggle
            toggle.connect("toggled", lambda b, v=value: self.set_call_filter(v) if b.get_active() else None)
            self.calls_filter[value] = toggle
            strip.append(toggle)
        head.append(strip)
        self.calls_clear = button("Clear all", self.clear_calls, style="flat")
        head.append(self.calls_clear)
        page.append(head)
        content = box(10, margin=16)
        self.calls_status = label("", "pp-status", wrap=True)
        content.append(self.calls_status)
        self.calls_list = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
        self.calls_list.add_css_class("boxed-list")
        content.append(self.calls_list)
        self.calls_more = button("Load earlier calls", lambda: self.load_calls(more=True), style="pp-pill-button")
        self.calls_more.set_halign(Gtk.Align.CENTER)
        content.append(self.calls_more)
        self.calls_filtered_box = Gtk.Expander(label="Filtered calls")
        self.calls_filtered_box.connect("notify::expanded", lambda e, _: self.load_filtered_calls() if e.get_expanded() else None)
        filtered_inner = box(6)
        filtered_inner.set_margin_top(8)
        filtered_inner.append(label("Calls your settings silenced. They didn’t ring.", "dim-label", wrap=True))
        self.calls_filtered_list = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
        self.calls_filtered_list.add_css_class("boxed-list")
        filtered_inner.append(self.calls_filtered_list)
        self.calls_filtered_box.set_child(filtered_inner)
        content.append(self.calls_filtered_box)
        page.append(scroll(content))
        return page

    def build_filtered_page(self):
        page = box(0)
        page.add_css_class("pp-wash")
        head = box(8, horizontal=True, margin=12)
        head.add_css_class("pp-toolbar")
        title = label("Filtered", "pp-title")
        title.set_hexpand(True)
        head.append(title)
        page.append(head)
        content = box(12, margin=16)
        content.append(label("Messages your privacy settings moved out of your inbox. They didn’t notify you. Accept a sender "
                             "to move their messages to your inbox.", "dim-label", wrap=True))
        self.filtered_list = box(12)
        content.append(self.filtered_list)
        page.append(scroll(content))
        return page

    # ---- side navigation -----------------------------------------------------------------------

    def show_page(self, name):
        if name == "calls" and CALL_HISTORY not in self.features:
            name = "conversation"
        if name == "filtered" and MESSAGE_FILTERING not in self.features:
            name = "conversation"
        self.main_stack.set_visible_child_name(name)
        if name == "calls":
            self.render_calls()
            self.load_calls()
        elif name == "filtered":
            self.render_filtered()
        else:
            shown = getattr(self, "page_shown_" + name, None)  # calendar, orgs: their mixins
            if shown:
                shown()
        self.sync_side_nav()

    def page(self):
        return self.main_stack.get_visible_child_name()

    def sync_side_nav(self):
        on = CALL_HISTORY in self.features and bool(self.identity)
        self.calls_button.set_visible(on)
        unseen = self.call_state["unseen"] if on else 0
        text = "Calls" + (f"  • {unseen if unseen < 100 else '99+'}" if unseen else "")
        self.calls_button_label.set_text(text)
        self.calls_button.update_property([Gtk.AccessibleProperty.LABEL], [f"Calls, {unseen} missed" if unseen else "Calls"])
        if unseen:
            self.calls_button_label.add_css_class("error")
        else:
            self.calls_button_label.remove_css_class("error")

    # ---- call log ------------------------------------------------------------------------------

    def reset_calls(self):
        self.call_state = {"entries": [], "unseen": 0, "filter": "all", "next": None, "loaded": False, "loading": False,
                           "filtered": None, "error": None}

    def load_calls(self, more=False, quiet=False):
        if CALL_HISTORY not in self.features or not self.identity:
            return
        state, identity = self.call_state, self.identity
        if state["loading"]:
            return
        wanted = state["filter"]
        query = {"filter": wanted, "limit": str(CALL_PAGE)}
        if more and state["next"]:
            query["before"] = str(state["next"])
        state["loading"] = True
        if not quiet:
            self.render_calls()

        def loaded(body):
            state["loading"] = False
            if state["filter"] != wanted:
                return
            entries = [e for e in body.get("entries") or [] if isinstance(e, dict) and e.get("id")]
            if more:
                seen = {e["id"] for e in state["entries"]}
                state["entries"] += [e for e in entries if e["id"] not in seen]
            else:
                state["entries"] = entries
            state["next"] = body.get("next_before")
            state["unseen"] = max(0, int(body.get("unseen_missed") or 0))
            state["loaded"], state["error"] = True, None
            self.sync_side_nav()
            if self.page() == "calls":
                self.render_calls()
                self.mark_calls_seen()
            elif self.page() == "conversation" and self.peer:
                self.render_messages()

        def failed(_):
            state["loading"] = False
            state["error"] = "Could not load your calls."
            if self.page() == "calls":
                self.render_calls()
        self._work(lambda: self.api.call("GET", "/v1/calls/log", identity, query=query), loaded, failed)

    def load_filtered_calls(self):
        identity, state = self.identity, self.call_state

        def loaded(body):
            state["filtered"] = body.get("entries") or []
            self.render_calls()
        self._work(lambda: self.api.call("GET", "/v1/calls/log", identity, query={"filter": "filtered", "limit": str(CALL_PAGE)}),
                   loaded, lambda _: (state.__setitem__("filtered", []), self.render_calls()))

    def set_call_filter(self, value):
        if self.call_state["filter"] == value:
            return
        self.call_state.update(filter=value, entries=[], next=None, loaded=False)
        self.load_calls()

    def mark_calls_seen(self):
        """Looking at the page is seeing the missed calls on it."""
        state, identity = self.call_state, self.identity
        newest = max((int(e.get("started_at") or 0) for e in state["entries"]), default=0)
        if not state["unseen"] or not newest or not self.is_active():
            return
        state["unseen"] = 0
        self.sync_side_nav()

        def done(body):
            state["unseen"] = max(0, int(body.get("unseen_missed") or 0))
            for entry in state["entries"]:
                if (entry.get("started_at") or 0) <= newest:
                    entry["seen"] = True
            self.sync_side_nav()
        self._work(lambda: self.api.call("POST", "/v1/calls/log/seen", identity, data={"up_to": newest}), done, lambda _: None)

    def call_name(self, entry):
        key = call_peer_key(entry)
        if key:
            return contact_for(key, self.contacts).get("alias") or display_name(key)
        if entry.get("group_id"):
            group = next((g for g in self.groups if g.get("id") == entry["group_id"]), None)
            if group:
                return group.get("name") or "Group call"
        people = len(entry.get("participants") or [])
        return f"{people} people" if people else "Call"

    def call_row(self, entry):
        key = call_peer_key(entry)
        name = self.call_name(entry)
        row = action_row(title=name, subtitle=call_detail(entry), activatable=bool(key))
        row.set_title_lines(1)
        row.entry = entry
        missed = is_missed(entry)
        if missed:
            row.add_css_class("pp-call-missed")
        row.add_prefix(self.avatars.widget(key, name, 36))
        when = label(list_time(entry.get("started_at")) if entry.get("started_at") else "", "dim-label")
        when.set_valign(Gtk.Align.CENTER)
        row.add_suffix(when)
        outcome = call_outcome_label(entry)
        row.update_property([Gtk.AccessibleProperty.LABEL], [f"{name}, {outcome.lower()} {kind_word(entry.get('kind'))} call, {when.get_text()}"])
        if key:
            back = button(f"Call {name} back, {kind_word(entry.get('kind'))}", lambda: self.call_back(key, entry.get("kind") == "video"),
                          "camera-video-symbolic" if entry.get("kind") == "video" else "call-start-symbolic", "flat")
            back.set_valign(Gtk.Align.CENTER)
            row.add_suffix(back)
            row.connect("activated", lambda _: self.open_conversation(key))
        menu = Gio.Menu()
        actions = Gio.SimpleActionGroup()

        def add(name_, text, handler):
            action = Gio.SimpleAction.new(name_, None)
            action.connect("activate", lambda *_: handler())
            actions.add_action(action)
            menu.append(text, "call." + name_)
        add("delete", "Delete from history", lambda: self.delete_call(entry))
        if key:
            add("block", "Block", lambda: self.ask_block(key, name, call_id=entry["id"] if entry.get("direction") == "incoming" else None))
        if key and entry.get("direction") == "incoming":
            add("report", "Report spam", lambda: self.report_call(entry))
        more = Gtk.MenuButton(icon_name="view-more-symbolic", menu_model=menu, valign=Gtk.Align.CENTER)
        more.add_css_class("flat")
        more.set_tooltip_text("More actions for this call")
        more.update_property([Gtk.AccessibleProperty.LABEL], ["More actions for this call"])
        row.insert_action_group("call", actions)
        row.add_suffix(more)
        row.actions = actions
        return row

    def render_calls(self):
        if not hasattr(self, "calls_list"):
            return
        state = self.call_state
        for value, toggle in self.calls_filter.items():
            if toggle.get_active() != (value == state["filter"]):
                toggle.set_active(value == state["filter"])
        clear(self.calls_list)
        entries = [e for e in state["entries"] if e.get("outcome") != "filtered"]
        for entry in entries:
            self.calls_list.append(self.call_row(entry))
        status = state["error"] or ("Loading calls…" if state["loading"] and not state["loaded"] else
                                    ("No missed calls." if state["filter"] == "missed" else "No calls yet.") if state["loaded"] and not entries else "")
        self.calls_status.set_text(status)
        self.calls_list.set_visible(bool(entries))
        self.calls_more.set_visible(bool(state["next"]))
        self.calls_clear.set_sensitive(bool(state["entries"]))
        clear(self.calls_filtered_list)
        filtered = state["filtered"] or []
        for entry in filtered:
            self.calls_filtered_list.append(self.call_row(entry))
        self.calls_filtered_box.set_label(f"Filtered calls ({len(filtered)})" if filtered else "Filtered calls")
        if state["filtered"] is not None and not filtered:
            self.calls_filtered_list.append(action_row(title="No filtered calls."))

    def call_back(self, key, video):
        self.open_conversation(key)
        self.call(video)

    def delete_call(self, entry):
        identity, state = self.identity, self.call_state

        def deleted(_):
            state["entries"] = [e for e in state["entries"] if e.get("id") != entry.get("id")]
            if state["filtered"]:
                state["filtered"] = [e for e in state["filtered"] if e.get("id") != entry.get("id")]
            self.toast("Removed from call history.")
            self.render_calls()
            if self.peer:
                self.render_messages()
        self._work(lambda: self.api.call("DELETE", "/v1/calls/log/" + GLib.uri_escape_string(entry["id"], None, False), identity),
                   deleted, lambda _: self.toast("Could not remove that call."))

    def report_call(self, entry):
        identity = self.identity
        self._work(lambda: self.api.call("POST", "/v1/report-spam", identity, data={"call_id": entry["id"]}),
                   lambda _: self.toast("Reported as spam."), lambda _: self.toast("Could not send the report."))

    def clear_calls(self):
        identity, state = self.identity, self.call_state

        def cleared(_):
            state.update(entries=[], filtered=[] if state["filtered"] is not None else None, next=None, unseen=0)
            self.toast("Call history cleared.")
            self.sync_side_nav()
            self.render_calls()
            if self.peer:
                self.render_messages()
        self.confirm("Clear all calls?", "This removes every call from this mailbox’s history. Other people keep theirs.",
                     lambda: self._work(lambda: self.api.call("DELETE", "/v1/calls/log", identity), cleared,
                                        lambda _: self.toast("Could not clear your call history.")), "Clear all")

    def open_conversation(self, peer):
        self.group_open = None
        self.peer, self.subject, self.message_limit = peer, None, 200
        self.message_search.set_text("")
        self.show_page("conversation")
        self.render_all()
        self.restore_draft()
        self.ack_visible()

    # ---- filtered messages ---------------------------------------------------------------------

    @staticmethod
    def filtered_key(message):
        return message.get("peer_handle") or message.get("sender_handle") or message.get("peer") or message.get("from")

    def load_filtered(self):
        if MESSAGE_FILTERING not in self.features or not self.identity:
            return
        identity = self.identity

        def loaded(messages):
            self.filtered = [m for m in messages if m.get("direction") != "out"]
            self.render_peers()
            if self.page() == "filtered":
                self.render_filtered()
        self._work(lambda: self.api.call("GET", "/v1/inbox", identity, query={"filtered": "1", "include_read": "true"}).get("messages") or [],
                   loaded, lambda _: None)

    def filtered_groups(self):
        groups = {}
        for message in self.filtered:
            key = self.filtered_key(message)
            if not key or self.blocked(key):
                continue
            group = groups.setdefault(key, {"peer": key, "messages": [], "last": 0})
            group["messages"].append(message)
            group["last"] = max(group["last"], message.get("received_at") or 0)
        for group in groups.values():
            group["messages"].sort(key=lambda m: m.get("received_at") or 0)
        return sorted(groups.values(), key=lambda g: -g["last"])

    def render_filtered(self):
        if not hasattr(self, "filtered_list"):
            return
        clear(self.filtered_list)
        groups = self.filtered_groups()
        if not groups:
            self.filtered_list.append(label("Nothing is filtered.", "pp-status"))
        for group in groups:
            peer = group["peer"]
            card = box(8, margin=12)
            card.add_css_class("card")
            head = box(10, horizontal=True)
            head.append(self.avatars.widget(peer, display_name(peer), 32))
            names = box(2)
            names.append(label(display_name(peer), "heading"))
            last = group["messages"][-1]
            names.append(label("From " + (last.get("from") or peer) + (f" · {len(group['messages'])} messages" if len(group["messages"]) > 1 else ""),
                               "dim-label"))
            head.append(names)
            card.append(head)
            for message in group["messages"][-5:]:
                text = label(message_text(message), "pp-body", wrap=True)
                text.set_selectable(True)
                text.set_use_markup(False)
                text.set_lines(6)
                text.set_ellipsize(Pango.EllipsizeMode.END)
                card.append(text)
            actions = box(6, horizontal=True)
            actions.append(button("Accept", lambda p=peer, m=last: self.accept_filtered(p, m), style="suggested-action"))
            actions.append(button("Block…", lambda p=peer, m=last: self.ask_block(p, message_id=m.get("message_id"), report=True)))
            actions.append(button("Report spam", lambda m=last: self.report_message(m)))
            card.append(actions)
            self.filtered_list.append(card)

    def accept_filtered(self, peer, last):
        identity = self.identity
        sender = last.get("from") or peer

        def accepted(_):
            self.revision += 1
            self.filtered = [m for m in self.filtered if self.filtered_key(m) != peer]
            self.toast("Accepted. Their messages are in your inbox.")
            self.render_filtered()
            self.refresh()
        self._work(lambda: self.api.call("POST", "/v1/filtered/accept", identity, data={"peer": sender}), accepted,
                   lambda error: self.toast("Could not accept. " + self.error_text(error)))

    # ---- disappearing one-to-one timers --------------------------------------------------------

    def direct_timer_thread(self):
        if DISAPPEARING not in self.features or not self.peer:
            return None
        return direct_thread(self.peer, self.subject, self.threads)

    def direct_timer(self):
        thread = self.direct_timer_thread()
        return (thread or {}).get("ephemeral_secs") or None

    def choose_direct_timer(self):
        thread = self.direct_timer_thread()
        if not thread:
            self.toast("Send a message first; the timer belongs to a conversation’s subject.")
            return
        self.timer_dialog("Disappearing messages", thread.get("ephemeral_secs"),
                          "Applies to both of you; they see who changed it. The clock starts when a message is sent. " + LIMITS_NOTE,
                          lambda secs: self.set_direct_timer(thread, secs))

    def timer_dialog(self, title, current, note, chosen):
        window, content = self.dialog(title, 440)
        content.append(label("New messages disappear after", "heading"))
        group = None
        choices = []
        for secs in TIMERS:
            check = Gtk.CheckButton(label=timer_label(secs), active=(current or None) == secs)
            if group:
                check.set_group(group)
            group = group or check
            choices.append((secs, check))
            content.append(check)
        content.append(label(note, "dim-label", wrap=True))

        def save():
            secs = next((s for s, c in choices if c.get_active()), None)
            window.close()
            if secs != (current or None):
                chosen(secs)
        content.append(button("Save", save, style="suggested-action"))
        window.timer_choices = choices
        window.timer_save = save
        window.present()
        return window

    def set_direct_timer(self, thread, secs):
        identity = self.identity

        def done(_):
            for row in self.threads:
                if row.get("thread_id") == thread["thread_id"]:
                    row["ephemeral_secs"] = secs
            self.revision += 1
            self.toast(f"New messages disappear after {timer_label(secs)}." if secs else "Disappearing messages are off.")
            self.render_messages()
        self._work(lambda: self.api.call("PATCH", "/v1/threads/" + GLib.uri_escape_string(thread["thread_id"], None, False), identity,
                                         data={"ephemeral_secs": secs}),
                   done, lambda error: self.toast("Could not change the timer: " + self.error_text(error)))
