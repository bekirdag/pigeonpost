"""Organization rooms (SDS §4, docs/orgs.md): the Organizations page.

The catalog belongs to the accepted login; the room directory, joins and creations belong to the
login *and* the organization inbox they act as (the selected mailbox), through `self.mailbox_lease`.
Everything shown here is a hint: each action asks the postbox again, and a refusal clears what was
cached. Agents delegated to the person are listed apart and never treated as their own inboxes.
"""

import time

import gi
gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk

from . import orgs_core as oc
from .model import display_name
from .theme import add_css
from .widgets import box, button, clear, label, scroll

add_css("""
.pp-org-card { background-color: @pp_ground; border: 1px solid @pp_stroke; border-radius: 12px; padding: 12px 14px; }
.pp-org-room { background-color: @pp_ground; border: 1px solid @pp_stroke; border-radius: 10px; padding: 8px 10px; }
""")

CATALOG_REFRESH = 300
E2EE = "Rooms are end-to-end encrypted groups. Joining adds this computer’s device once a member’s device confirms it."


class OrgsMixin:
    # ---- lifecycle -----------------------------------------------------------------------------

    def reset_orgs(self):
        """Called on sign-out and on every mailbox switch. The catalog belongs to the login, so it
        survives a switch within the same accepted login; rooms and pending work do not."""
        previous = getattr(self, "org_state", None)
        keep = (previous is not None and previous.get("catalog") is not None and previous.get("lease") is not None
                and previous["lease"] is self.lease and self.lease.current)
        self.org_state = {"catalog": previous["catalog"] if keep else None, "lease": self.lease if keep else None,
                          "loaded_at": previous["loaded_at"] if keep else 0, "loading": False,
                          "selected": previous["selected"] if keep else None, "rooms": None, "rooms_for": None,
                          "search": "", "message": None, "pending": [], "pending_for": None, "busy": False,
                          "retry_at": previous["retry_at"] if keep else None, "memory_only": False}
        if getattr(self, "orgs_content", None) is not None:
            self.render_orgs()

    def orgs_on(self):
        return oc.ORGS in self.features and bool(self.identity) and self.lease is not None

    def orgs_features_changed(self):
        on = self.orgs_on()
        self.orgs_button.set_visible(on)
        if not on:
            if self.page() == "orgs":
                self.main_stack.set_visible_child_name("conversation")
            return
        if self.org_state["catalog"] is None and not self.org_state["loading"]:
            self.load_orgs()

    def orgs_tick(self):
        if not self.orgs_on():
            return
        state = self.org_state
        if state["retry_at"] and state["retry_at"] <= time.time():
            state["retry_at"] = None
            if self.page() == "orgs":
                self.render_orgs()
        if (self.page() == "orgs" and not state["loading"] and not state["busy"]
                and time.monotonic() - state["loaded_at"] > CATALOG_REFRESH):
            self.load_orgs()

    def page_shown_orgs(self):
        self.render_orgs()
        if self.orgs_on() and not self.org_state["loading"]:
            self.load_orgs()

    def build_orgs_page(self):
        self.orgs_content = box(14, margin=24)
        self.orgs_content.set_size_request(420, -1)
        page = box(0)
        page.add_css_class("pp-wash")
        page.append(scroll(self.orgs_content))
        return page

    # ---- state helpers -------------------------------------------------------------------------

    def org_api(self, lease):
        return oc.OrgsAPI(self.api_for(lease))

    def selected_org(self):
        catalog = self.org_state["catalog"]
        if not catalog:
            return None
        return next((o for o in catalog["orgs"] if o["id"] == self.org_state["selected"]), None)

    def org_acting(self, org):
        """The org inbox when it is this account's own and currently selected, else None."""
        if not org or not org["mailbox"]:
            return None
        address = org["mailbox"]["address"]
        own = oc.owned_org_mailboxes(self.org_state["catalog"], self.mailboxes)
        if own.get(org["id"]) != address or self.identity != address or self.mailbox_lease is None:
            return None
        return address

    def org_failed(self, error, decision=False):
        """Refusals clear cached access; 429 waits; anything else keeps state and explains."""
        state = self.org_state
        state["busy"] = False
        status, code = getattr(error, "status", 0), getattr(error, "code", "")
        if status in (401, 403, 404, 410) or code in oc.ACCESS_LOST:
            state.update(catalog=None, selected=None, rooms=None, rooms_for=None)
            state["message"] = "Access could not be confirmed. Refresh your organizations."
        elif status == 429:
            state["retry_at"] = time.time() + oc.retry_after(error)
            state["message"] = oc.explain(error)
        else:
            state["message"] = ("The result is not confirmed. Review the room again before deciding." if decision
                                else oc.explain(error))
        self.render_orgs()

    # ---- loading -------------------------------------------------------------------------------

    def load_orgs(self):
        lease = self.lease
        if not self.orgs_on() or self.org_state["loading"]:
            return
        self.org_state["loading"] = True
        state = self.org_state

        def loaded(catalog):
            if state is not self.org_state:
                return
            state.update(loading=False, catalog=catalog, lease=lease, loaded_at=time.monotonic(), message=None)
            ids = [o["id"] for o in catalog["orgs"] if oc.readable(o)]
            if state["selected"] not in ids:
                # Prefer the org whose inbox is selected; else the only one.
                acting = next((o["id"] for o in catalog["orgs"] if o["mailbox"] and o["mailbox"]["address"] == self.identity), None)
                state["selected"] = acting or (ids[0] if len(ids) == 1 else None)
                state["rooms"] = None
            self.render_orgs()
            self.load_org_rooms()
            self.load_org_pending()

        def failed(error):
            if state is not self.org_state:
                return
            state["loading"] = False
            self.org_failed(error)
        self.bound_work(lease, lambda: self.org_api(lease).catalog(), loaded, failed)
        self.render_orgs()

    def load_org_rooms(self):
        org = self.selected_org()
        acting = self.org_acting(org)
        if not acting or not oc.readable(org):
            return
        lease, state, search = self.mailbox_lease, self.org_state, self.org_state["search"]

        def loaded(rooms):
            if state is self.org_state and lease is self.mailbox_lease and self.org_state["selected"] == org["id"]:
                state.update(rooms=rooms, rooms_for=(org["id"], acting))
                self.render_orgs()
        self.bound_work(lease, lambda: self.org_api(lease).rooms(acting, org["id"], search), loaded,
                        lambda error: self.org_failed(error) if state is self.org_state else None)

    def load_org_pending(self):
        org = self.selected_org()
        acting = self.org_acting(org)
        if not acting:
            return
        lease, state = self.mailbox_lease, self.org_state

        def read():
            return oc.pending_rooms(self.protected.read(lease, acting))

        def loaded(rows):
            if state is self.org_state and lease is self.mailbox_lease:
                state.update(pending=rows, pending_for=acting, memory_only=not getattr(self.protected, "available", False))
                self.render_orgs()

        def failed(error):
            if state is self.org_state:
                state["message"] = "Unconfirmed room creations could not be read. Unlock your desktop keyring and try again."
                self.render_orgs()
        self.bound_work(lease, read, loaded, failed)

    # ---- rendering -----------------------------------------------------------------------------

    def render_orgs(self):
        """Rebuild the page on idle (coalesced): never under the widget whose click asked for it."""
        if getattr(self, "orgs_content", None) is None or getattr(self, "orgs_render_pending", False):
            return
        self.orgs_render_pending = True

        def run():
            self.orgs_render_pending = False
            if not self.closed:
                self.render_orgs_now()
            return False
        GLib.idle_add(run)

    def render_orgs_now(self):
        content = self.orgs_content
        clear(content)
        state = self.org_state
        head = box(8, horizontal=True)
        title = label("Organizations", "title-2")
        title.set_hexpand(True)
        head.append(title)
        refresh = button("Refresh organizations", self.load_orgs, "view-refresh-symbolic", "flat")
        refresh.set_sensitive(not state["loading"])
        head.append(refresh)
        content.append(head)
        if state["message"]:
            message = label(state["message"], "pp-status", wrap=True)
            message.set_name("org-message")
            content.append(message)
        catalog = state["catalog"]
        if catalog is None:
            content.append(label("Loading your organizations…" if state["loading"] else "Your organizations could not be loaded.",
                                 "pp-status", wrap=True))
            return
        orgs = [o for o in catalog["orgs"] if oc.readable(o)]
        if not orgs:
            content.append(label("You’re not an active member of any organization.", "pp-status", wrap=True))
        if len(orgs) > 1:
            picker = Gtk.DropDown(model=Gtk.StringList.new([o["name"] for o in orgs]))
            picker.update_property([Gtk.AccessibleProperty.LABEL], ["Organization"])
            index = next((i for i, o in enumerate(orgs) if o["id"] == state["selected"]), Gtk.INVALID_LIST_POSITION)
            picker.set_selected(index)
            picker.connect("notify::selected", lambda w, _: self.choose_org(orgs[w.get_selected()]["id"])
                           if w.get_selected() < len(orgs) else None)
            content.append(picker)
        org = self.selected_org()
        if org:
            self.render_org(content, org)
        agents = oc.operated_agents(catalog)
        if agents:
            content.append(label("Agents you operate", "heading"))
            content.append(label("Delegated to you by your organization. They are not your personal inboxes; this app "
                                 "doesn’t open them yet. Use the web inbox to read them.", "pp-status", wrap=True))
            for agent in agents:
                row = box(2)
                row.add_css_class("pp-org-room")
                row.append(label(display_name(agent["handle"] or agent["address"]) + " · " + (agent["handle"] or agent["address"]), "pp-peer"))
                caps = ", ".join(agent["capabilities"]) or "no capabilities"
                row.append(label(f"{agent['org_name']} · {caps}", "pp-meta", wrap=True))
                row.agent_address = agent["address"]
                content.append(row)
        if hasattr(self, "approve_device"):
            content.append(button("Approve a machine…", self.approve_device, style="pp-pill-button"))

    def choose_org(self, org_id):
        state = self.org_state
        if state["selected"] == org_id or state["busy"]:
            return
        state.update(selected=org_id, rooms=None, rooms_for=None, message=None)
        self.render_orgs()
        self.load_org_rooms()
        self.load_org_pending()

    def render_org(self, content, org):
        state = self.org_state
        card = box(4)
        card.add_css_class("pp-org-card")
        card.append(label(org["name"], "title-3"))
        roles = ", ".join(r.replace("_", " ") for r in org["roles"]) or "member"
        status = {"active": "Active", "past_due": "Payment due", "read_only": "Read-only"}.get(org["status"], org["status"])
        card.append(label(f"{status} · {roles}", "pp-meta"))
        own = oc.owned_org_mailboxes(state["catalog"], self.mailboxes)
        address = org["mailbox"]["address"] if org["mailbox"] else None
        if not address:
            card.append(label("You haven’t chosen your address in this organization yet. Do that on the enterprise portal.",
                              "pp-status", wrap=True))
        elif own.get(org["id"]) != address:
            card.append(label("Your inbox in this organization isn’t listed for this account on this computer. Refresh your mailboxes.",
                              "pp-status", wrap=True))
        else:
            handle = org["mailbox"].get("handle") or address
            card.append(label("Your inbox here: " + handle, "pp-mono"))
            if self.identity != address:
                switch = button("Use " + handle + " to see its rooms", lambda a=address: self.switch_to_mailbox(a),
                                style="suggested-action")
                switch.set_halign(Gtk.Align.START)
                card.append(switch)
        content.append(card)
        acting = self.org_acting(org)
        if not acting:
            return
        self.render_org_pending(content, org, acting)
        tools = box(8, horizontal=True)
        search = Gtk.SearchEntry(placeholder_text="Search rooms", text=state["search"])
        search.set_hexpand(True)
        search.connect("activate", lambda w: self.search_org_rooms(w.get_text()))
        search.connect("stop-search", lambda w: self.search_org_rooms(""))
        tools.append(search)
        if oc.may_create(org, acting) and self.groups_on():
            new = button("New room", lambda: self.new_org_room(org), "list-add-symbolic", "pp-pill-button")
            new.set_sensitive(not state["busy"] and not state["retry_at"])
            tools.append(new)
        content.append(tools)
        if not self.groups_on():
            content.append(label("Rooms need group encryption on this computer, which isn’t ready.", "pp-status", wrap=True))
        rooms = state["rooms"] if state["rooms_for"] == (org["id"], acting) else None
        if rooms is None:
            content.append(label("Loading rooms…", "pp-status"))
            return
        if not rooms:
            content.append(label("No rooms found." if state["search"] else "No listed rooms yet.", "pp-status"))
        for room in rooms:
            content.append(self.org_room_row(org, acting, room))

    def org_room_row(self, org, acting, room):
        row = box(10, horizontal=True)
        row.add_css_class("pp-org-room")
        text = box(2)
        text.set_hexpand(True)
        text.append(label(room["name"], "pp-peer"))
        if room["description"]:
            line = label(room["description"], "pp-preview", wrap=True)
            line.set_lines(2)
            text.append(line)
        count = room["member_count"]
        policy = "Anyone in " + org["name"] if room["join_policy"] == "public" else "Invited people"
        text.append(label(f"{count} {'member' if count == 1 else 'members'} · {policy}", "pp-meta"))
        row.append(text)
        action = oc.room_action(room, org)
        busy = self.org_state["busy"] or bool(self.org_state["retry_at"]) or not self.groups_on()
        if action == "open":
            widget = button("Open", lambda: self.open_org_room(room["id"]))
        elif action == "join":
            widget = button("Join…", lambda: self.join_org_room(org, acting, room), style="suggested-action")
        elif action == "accept":
            widget = button("Accept invitation…", lambda: self.join_org_room(org, acting, room, accept=True), style="suggested-action")
        else:
            widget = label("Invite only" if room["join_policy"] != "public" else "Unavailable", "pp-meta")
        if isinstance(widget, Gtk.Button):
            widget.set_valign(Gtk.Align.CENTER)
            widget.set_sensitive(not busy or action == "open")
            widget.update_property([Gtk.AccessibleProperty.LABEL], [f"{widget.get_label()} {room['name']}"])
        row.append(widget)
        row.room_id, row.room_action = room["id"], action
        return row

    def render_org_pending(self, content, org, acting):
        state = self.org_state
        if state["pending_for"] != acting:
            return
        for pending in [p for p in state["pending"] if p["org_id"] == org["id"]]:
            bar = box(8, horizontal=True)
            bar.add_css_class("pp-banner")
            text = label(f"Creating “{pending['name']}” wasn’t confirmed. It may already exist. Retry sends exactly the "
                         "same request; it never makes a second room." + (" This attempt is kept until the app closes."
                                                                           if state["memory_only"] else ""), wrap=True)
            text.set_hexpand(True)
            bar.append(text)
            retry = button("Retry", lambda p=pending: self.retry_org_room(org, acting, p))
            retry.set_sensitive(not state["busy"] and not state["retry_at"])
            bar.append(retry)
            discard = button("Discard", lambda p=pending: self.discard_org_room(acting, p))
            discard.set_sensitive(not state["busy"])
            bar.append(discard)
            bar.pending_key = pending["request_key"]
            content.append(bar)

    # ---- actions -------------------------------------------------------------------------------

    def switch_to_mailbox(self, address):
        index = next((i for i, row in enumerate(self.mailboxes) if row["address"] == address), None)
        if index is None:
            return
        self.mailbox_picker.set_selected(index)
        if self.page() != "orgs":
            self.show_page("orgs")
        self.render_orgs()
        if self.org_state["catalog"] is None:
            self.load_orgs()
        else:
            self.load_org_rooms()
            self.load_org_pending()

    def search_org_rooms(self, text):
        text = text.strip()[:80]
        if text == self.org_state["search"]:
            return
        self.org_state.update(search=text, rooms=None, rooms_for=None)
        self.render_orgs()
        self.load_org_rooms()

    def open_org_room(self, gid):
        if self.group_by_id(gid):
            self.open_group(gid)
            return
        self.toast("Opening the room…")
        self.groups_cycle(force=True)
        lease = self.mailbox_lease

        def later():
            if lease is self.mailbox_lease and lease is not None and lease.current and self.group_by_id(gid):
                self.open_group(gid)
            return False
        GLib.timeout_add(1500, later)

    def join_org_room(self, org, acting, room, accept=False):
        """Ask the server again (preview), then the person decides, then join or accept."""
        state, lease = self.org_state, self.mailbox_lease
        if state["busy"]:
            return
        state["busy"] = True
        self.render_orgs()

        def previewed(preview):
            state["busy"] = False
            self.render_orgs()
            mine = preview.get("my_state")
            if mine in ("active", "keying"):
                self.open_org_room(room["id"])
                return
            if not accept and not preview.get("can_join"):
                self.notice(preview.get("name") or room["name"], "This room is invite only.")
                return
            detail = f"{preview.get('member_count') or room['member_count']} members · as {display_name(acting)}.\n\n{E2EE}"
            verb = "Accept the invitation to" if accept else "Join"
            self.confirm(f"{verb} {preview.get('name') or room['name']}?", detail,
                         lambda: self.decide_org_room(org, acting, room, accept, lease), "Accept" if accept else "Join")

        def failed(error):
            self.org_failed(error)
        self.bound_work(lease, lambda: self.org_api(lease).preview(acting, room["id"]), previewed, failed)

    def decide_org_room(self, org, acting, room, accept, lease):
        state = self.org_state
        if lease is not self.mailbox_lease or state["busy"]:
            return
        state["busy"] = True
        self.render_orgs()

        def act():
            api = self.org_api(lease)
            try:
                api.accept(acting, room["id"]) if accept else api.join(acting, room["id"])
            except Exception as error:
                if getattr(error, "code", "") != "already_member":
                    raise
            return True

        def done(_):
            state["busy"] = False
            self.toast("Joined. A member’s device will add you to the encryption in a moment.")
            self.groups_cycle(force=True)
            self.load_org_rooms()

        self.bound_work(lease, act, done, lambda error: self.org_failed(error, decision=True))

    def new_org_room(self, org):
        acting = self.org_acting(org)
        if not acting:
            return
        window, content = self.dialog("New room in " + org["name"], 520)
        content.append(label(E2EE, "pp-status", wrap=True))
        name = Gtk.Entry(placeholder_text="Room name", max_length=80)
        name.update_property([Gtk.AccessibleProperty.LABEL], ["Room name"])
        content.append(name)
        description = Gtk.Entry(placeholder_text="Description (optional)", max_length=500)
        description.update_property([Gtk.AccessibleProperty.LABEL], ["Room description"])
        content.append(description)
        content.append(label("Who can join", "heading"))
        join = Gtk.DropDown(model=Gtk.StringList.new(["Anyone in " + org["name"], "Invited people only"]))
        content.append(join)
        content.append(label("Who can post", "heading"))
        post = Gtk.DropDown(model=Gtk.StringList.new(["Everyone", "Admins only"]))
        content.append(post)
        listed = Gtk.CheckButton(label="List it in the organization’s room directory", active=True)
        content.append(listed)
        error = label("", "error", wrap=True)
        content.append(error)

        def submit():
            try:
                wire = oc.room_request(org["id"], name.get_text(), description.get_text(),
                                       "public" if join.get_selected() == 0 else "private",
                                       "everyone" if post.get_selected() == 0 else "admins", listed.get_active())
            except ValueError as problem:
                error.set_text(str(problem))
                return
            window.close()
            self.create_org_room(org, acting, wire)
        create = button("Create room", submit, style="suggested-action")
        content.append(create)
        name.connect("activate", lambda _: submit())
        window.new_room = {"name": name, "description": description, "join": join, "post": post, "listed": listed,
                           "submit": submit, "error": error}
        window.present()
        name.grab_focus()
        return window

    def create_org_room(self, org, acting, wire, saved=False):
        """Save the exact request first, then post it. Lost answers keep it for an explicit retry."""
        state, lease = self.org_state, self.mailbox_lease
        if state["busy"] or lease is None or self.identity != acting:
            return
        state["busy"] = True
        state["message"] = None
        self.render_orgs()
        key = oc.parse_room_request(wire)["request_key"]

        def work():
            if not saved:
                self.protected.update(lease, acting, lambda record: oc.save_pending(record, wire))
            try:
                group = self.org_api(lease).create_exact(acting, wire)
            except Exception as error:
                if not oc.uncertain(error):
                    # Refused before anything was created: forget this attempt, keep nothing.
                    try:
                        self.protected.update(lease, acting, lambda record: oc.remove_pending(record, key))
                    except Exception:
                        pass
                raise
            try:
                self.protected.update(lease, acting, lambda record: oc.remove_pending(record, key))
                cleaned = True
            except Exception:
                cleaned = False
            return group, cleaned

        def done(result):
            group, cleaned = result
            state["busy"] = False
            state["pending"] = [p for p in state["pending"] if p["request_key"] != key]
            self.toast(f"Room “{group.get('name') or 'room'}” created." +
                       ("" if cleaned else " Its local record couldn’t be cleared; discard it from the list."))
            self.groups_cycle(force=True)  # activates the room with this computer's device
            self.load_org_rooms()
            self.load_org_pending()

        def failed(error):
            state["busy"] = False
            if isinstance(error, ValueError):
                state["message"] = str(error)
            elif oc.uncertain(error):
                state["message"] = "Creating the room wasn’t confirmed. Retry the same request or discard it."
            else:
                self.org_failed(error)
                self.load_org_pending()
                return
            self.render_orgs()
            self.load_org_pending()
        self.bound_work(lease, work, done, failed)

    def retry_org_room(self, org, acting, pending):
        if self.org_state["busy"]:
            return
        self.create_org_room(org, acting, pending["wire"], saved=True)

    def discard_org_room(self, acting, pending):
        lease, state = self.mailbox_lease, self.org_state

        def discard():
            def done(_):
                state["pending"] = [p for p in state["pending"] if p["request_key"] != pending["request_key"]]
                self.render_orgs()
            self.bound_work(lease, lambda: self.protected.update(lease, acting, lambda record: oc.remove_pending(record, pending["request_key"])),
                            done, lambda error: self.toast("The attempt could not be discarded. Unlock your keyring and try again."))
        self.confirm("Discard this unconfirmed room?",
                     "If the room was created, it stays in your room list. This only forgets the attempt on this computer.",
                     discard, "Discard")
