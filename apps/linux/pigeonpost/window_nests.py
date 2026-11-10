"""Nests on Linux (docs/nests/phase-b.md): the Products page and Nest management.

Products has three tabs: Buy an Inbox (names, as before), Buy Nests and Services. Desktop has no
in-app payment: buying, upgrading and managing a package open the website. Everything shown is
the postbox's answer; each action asks again. The page and the sidebar entry exist only while the
postbox reports the `nests` feature for this account.

A Nest's conversation shows its status (Ready, Working, Needs you, Paused, Unavailable) in the
header, its waiting approval cards above the composer and a way to add a file. Its page lists
files, approvals and usage and offers pause, rename, export and delete to those who manage it.
Organization Nests (B7) appear in both places; what a person may do is decided by the server.
"""

import time

import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gio, Gtk

from . import nests_core as nc
from .widgets import box, button, clear, label, scroll

REFRESH = 60  # while Products or a Nest's page is open
IDLE_REFRESH = 300  # elsewhere: a Nest bought on the website still appears
PEER_REFRESH = 20  # the Nest conversation that is open: status and approval cards
PANEL_CARDS = 3  # approval cards shown in a conversation; the Nest's page lists them all


class NestsMixin:
    # ---- lifecycle -----------------------------------------------------------------------------

    def reset_nests(self):
        self.nest_state = {"packages": None, "snapshot": None, "loaded_at": 0, "loading": False, "message": None,
                           "selected": None, "detail": None, "files": None, "cards": None, "usage": None,
                           "org": None, "org_nests": None, "org_directory": {}, "peer": None, "panel_sig": None, "busy": False}
        if getattr(self, "products_content", None) is not None:
            self.render_products()
        if getattr(self, "nest_badge", None) is not None:
            self.hide_nest_chrome()

    def nests_on(self):
        return nc.nests_on(self.features) and self.lease is not None

    def nests_features_changed(self):
        on = self.nests_on()
        self.products_button.set_visible(on)
        if not on:
            if self.page() in ("products", "nest"):
                self.main_stack.set_visible_child_name("conversation")
            if getattr(self, "nest_badge", None) is not None:
                self.hide_nest_chrome()
            return
        if self.nest_state["snapshot"] is None and not self.nest_state["loading"]:
            self.load_nests()

    def nests_tick(self):
        if not self.nests_on():
            return
        wait = REFRESH if self.page() in ("products", "nest") else IDLE_REFRESH
        if not self.nest_state["loading"] and time.monotonic() - self.nest_state["loaded_at"] > wait:
            self.load_nests()
        self.refresh_peer_nest()

    def page_shown_products(self):
        self.render_products()
        if self.nests_on():
            self.load_nests()

    def page_shown_nest(self):
        self.render_nest()

    def nest_api(self):
        return nc.NestsAPI(self.api_for(self.lease))

    def nest_action_failed(self, error):
        """A refused action: on the Nest's page in its message area, elsewhere as a toast."""
        if self.page() == "nest":
            self.nest_failed(error)
        else:
            self.toast(nc.explain(error))

    def nest_failed(self, error):
        # loaded_at moves too, so a failing load is retried at the normal pace, not on every tick.
        self.nest_state.update(loading=False, busy=False, message=nc.explain(error), loaded_at=time.monotonic())
        # Only a lost credential or a closed feature empties the page. Other refusals, such as a
        # recipient the destination policy does not allow, are answers about one action.
        if getattr(error, "status", 0) == 401 or getattr(error, "code", "") in ("feature_disabled", "credential_not_accepted"):
            self.nest_state.update(snapshot=None, selected=None)
        self.render_products()
        self.render_nest()

    # ---- loading -------------------------------------------------------------------------------

    def load_nests(self):
        if not self.nests_on() or self.nest_state["loading"]:
            return
        state, lease = self.nest_state, self.lease
        state["loading"] = True
        api = self.nest_api()
        orgs = [o["id"] for o in self.nest_orgs()]

        def work():
            directory = {}
            for org in orgs:
                try:
                    data = api.org_nests(org)
                except Exception:  # one organization's answer never hides the person's own Nests
                    continue
                directory.update({n["address"]: n for n in data["nests"] if n["address"]})
            return api.packages(), api.snapshot(), directory

        def loaded(result):
            if state is not self.nest_state:
                return
            packages, snapshot, directory = result
            state.update(packages=packages, snapshot=snapshot, org_directory=directory, loading=False,
                         loaded_at=time.monotonic(), message=None)
            self.render_products()
            self.render_conversation_chrome()
            if state["org"]:
                self.load_org_nests(state["org"])
        self.bound_work(lease, work, loaded, lambda e: state is self.nest_state and self.nest_failed(e))

    def nest_for_peer(self, peer):
        state = getattr(self, "nest_state", None)
        if not state or not peer:
            return None
        keys = nc.peer_addresses(peer, self.messages)
        own = next((n for n in (state["snapshot"] or {}).get("nests", ()) if n["address"] in keys), None)
        return own or next((state["org_directory"][k] for k in keys if k in state["org_directory"]), None)

    # ---- conversation header (B3) --------------------------------------------------------------

    def hide_nest_chrome(self):
        for widget in (self.nest_badge, self.nest_manage_button, self.nest_file_button, self.nest_panel_holder):
            widget.set_visible(False)
        clear(self.nest_panel)
        self.nest_state["panel_sig"] = None

    def nest_chrome(self):
        """Called from render_conversation_chrome: a Nest's status beside its name, its cards below."""
        entry = self.nest_for_peer(self.peer) if self.nests_on() else None
        if entry is None:
            self.hide_nest_chrome()
            return
        peer = self.nest_state["peer"]
        live = peer if peer is not None and peer["nest_id"] == entry["nest_id"] else None
        nest = (live and live["detail"]) or entry
        if nest.get("status"):  # an organization's list has no status; its detail arrives next
            text, reason = nc.status_line(nest)
            self.nest_badge.set_text(text)
            self.nest_badge.set_tooltip_text(reason or text)
            self.nest_badge.update_property([Gtk.AccessibleProperty.LABEL], [f"Nest status: {text}" + (f". {reason}" if reason else "")])
        self.nest_badge.set_visible(bool(nest.get("status")))
        self.nest_manage_button.set_visible(True)
        self.nest_file_button.set_visible(True)
        self.render_nest_panel(nest, live["cards"] if live else None)
        self.refresh_peer_nest()

    def render_nest_panel(self, nest, cards):
        """What needs the person in this conversation. Rebuilt only when it changes, so a poll
        never moves focus or resets the scroll position of a card being read."""
        state = self.nest_state
        pending = nc.pending_cards(cards)
        reason = nc.status_line(nest)[1] if nest.get("status") else None
        notice = nc.deletion_notice(nest)
        sig = (nest["nest_id"], reason, notice, tuple((c["approval_id"], c["args_hash"]) for c in pending))
        if sig == state["panel_sig"]:
            return
        state["panel_sig"] = sig
        clear(self.nest_panel)
        for line in (reason, notice):
            if line:
                self.nest_panel.append(label(line, wrap=True))
        for card in pending[:PANEL_CARDS]:
            self.nest_panel.append(self.approval_card(nest["nest_id"], card))
        if len(pending) > PANEL_CARDS:
            self.nest_panel.append(button(f"See all {len(pending)} approvals", lambda n=nest["nest_id"]: self.open_nest(n)))
        self.nest_panel_holder.set_visible(bool(reason or notice or pending))

    def refresh_peer_nest(self, force=False):
        """The open Nest conversation's status and cards, on their own short timer."""
        if not self.nests_on() or self.page() != "conversation":
            return
        entry = self.nest_for_peer(self.peer)
        if entry is None:
            return
        state, nest_id = self.nest_state, entry["nest_id"]
        peer = state["peer"]
        if peer is None or peer["nest_id"] != nest_id:
            peer = state["peer"] = {"nest_id": nest_id, "detail": None, "cards": None, "at": 0, "loading": False, "again": False}
        if peer["loading"]:
            peer["again"] = peer["again"] or force
            return
        if not force and time.monotonic() - peer["at"] < PEER_REFRESH:
            return
        peer["loading"] = True
        api = self.nest_api()

        def done(result):
            peer.update(detail=result[0], cards=result[1], loading=False, again=False, at=0 if peer["again"] else time.monotonic())
            if state is self.nest_state:
                self.nest_chrome()

        def failed(_error):  # the header keeps what it knew; the next tick asks again
            peer.update(loading=False, again=False, at=time.monotonic())
        self.bound_work(self.lease, lambda: (api.nest(nest_id), api.approvals(nest_id)), done, failed)

    def open_nest_for_peer(self):
        nest = self.nest_for_peer(self.peer)
        if nest:
            self.open_nest(nest["nest_id"])

    def nest_add_file_for_peer(self):
        nest = self.nest_for_peer(self.peer)
        if nest:
            self.nest_add_file(nest["nest_id"])

    # ---- Products page (B2) --------------------------------------------------------------------

    def build_products_page(self):
        page = box(0)
        page.add_css_class("pp-wash")
        self.products_tabs = Gtk.Notebook()
        self.products_tabs.set_hexpand(True)
        self.products_tabs.set_vexpand(True)
        self.products_inbox = box(12, margin=24)
        self.products_content = box(12, margin=24)
        self.products_services = box(12, margin=24)
        for child, title in ((self.products_inbox, "Buy an Inbox"), (self.products_content, "Buy Nests"),
                             (self.products_services, "Services")):
            tab = label(title)
            tab.update_property([Gtk.AccessibleProperty.LABEL], [title])
            self.products_tabs.append_page(scroll(child), tab)
        page.append(self.products_tabs)
        self.products_inbox.append(label("Buy an Inbox", "title-2"))
        self.products_inbox.append(label("A name and its inbox, with message storage. Register a name or manage your "
                                         "name plans on the website.", wrap=True))
        self.products_inbox.append(button("Register or manage names on the website",
                                          lambda: self.open_url("https://pigeonpost.dev/account"), style="suggested-action"))
        self.products_services.append(label("Services", "title-2"))
        self.products_services.append(label("Ready-to-use applications run by a Nest. Not available yet.", wrap=True))
        for name, text in nc.SERVICES:
            card = box(4)
            card.add_css_class("card")
            card.append(label(name, "heading"))
            card.append(label(text, wrap=True))
            card.append(label("Coming soon", "dim-label"))
            card.update_property([Gtk.AccessibleProperty.LABEL], [f"{name}. {text}. Coming soon"])
            self.products_services.append(card)
        return page

    def render_products(self):
        content = getattr(self, "products_content", None)
        if content is None:
            return
        clear(content)
        state = self.nest_state
        content.append(label("Buy Nests", "title-2"))
        content.append(label("A Nest is your own AI agent with a private, encrypted workspace. You talk to it in an "
                             "ordinary conversation. Buying and changing a package happens on the website.", wrap=True))
        if state["message"]:
            content.append(label(state["message"], "error", wrap=True))
        if state["snapshot"] is None:
            content.append(label("Loading…" if state["loading"] else "Your Nests could not be loaded.", "dim-label"))
            content.append(button("Refresh", self.load_nests))
            return
        snap = state["snapshot"]
        context = self.nest_context_picker()
        if context is not None:
            content.append(context)
        if state["org"]:
            self.render_org_nests(content)
            return
        if snap["package"]:
            content.append(label(f"Your package: {self.package_name(snap['package'])}. "
                                 f"{snap['used']} of {snap['slots']} Nest{'s' if snap['slots'] != 1 else ''} in use.", wrap=True))
        if snap["duplicates"]:
            content.append(label("You pay for more than one Nest package. Cancel the extra one where you bought it; it does "
                                 "not add Nests.", "error", wrap=True))
        for setup in snap["setups"]:
            row = box(8, horizontal=True)
            text = f"{setup['label']}: " + ("setup failed" if setup["status"] == "failed" else "paid, setting up…")
            row.append(label(text, wrap=True))
            row.append(button("Retry", lambda s=setup: self.retry_nest_setup(s["setup_id"])))
            content.append(row)
        if snap["nests"]:
            content.append(label("Your Nests", "heading"))
            for nest in snap["nests"]:
                row = box(8, horizontal=True)
                text, _ = nc.status_line(nest)
                name = label(f"{nest['label']} — {text}")
                name.set_hexpand(True)
                row.append(name)
                row.append(button("Open", lambda n=nest: self.open_nest(n["nest_id"])))
                content.append(row)
        if nc.can_add_nest(snap):
            content.append(button("New Nest…", self.new_nest, "list-add-symbolic"))
        content.append(label("Packages", "heading"))
        for package in (state["packages"] or {}).get("packages", []):
            content.append(self.package_card(package, snap))
        content.append(button("Buy, upgrade or manage on the website", lambda: self.open_url(nc.WEB_PRODUCTS),
                              style="suggested-action"))

    def new_nest(self):
        api = self.nest_api()

        def submitted(value):
            problem = nc.label_problem(value)
            if problem:
                raise ValueError(problem)
            self.toast("Setting up your Nest…")
            self.bound_work(self.lease, lambda: api.create(value), lambda _: (self.toast("Nest created"), self.load_nests()), self.nest_failed)
        self.form_dialog("New Nest", "Name", submitted)

    def package_name(self, code):
        packages = (self.nest_state["packages"] or {}).get("packages", [])
        return next((p["name"] for p in packages if p["code"] == code), code)

    def package_card(self, p, snap):
        card = box(4)
        card.add_css_class("card")
        card.append(label(p["name"], "heading"))
        ai = (f"AI included: {p['input_tokens']:,} input and {p['output_tokens']:,} output tokens a month"
              if p["model"] == "managed" else "No AI included: connect a model later")
        details = (f"{p['slots']} Nest{'s' if p['slots'] != 1 else ''} · {p['storage_bytes'] // (1 << 30)} GiB files · "
                   f"{p['active_hours']} hours a month · {ai}. Shared by the package’s Nests.")
        card.append(label(details, wrap=True))
        current = snap["package"] == p["code"]
        card.append(label("Your current package" if current else "See the price on the website", "dim-label"))
        card.update_property([Gtk.AccessibleProperty.LABEL], [f"{p['name']}. {details}"])
        return card

    def retry_nest_setup(self, setup_id):
        state = self.nest_state
        api = self.nest_api()

        def done(snapshot):
            state.update(snapshot=snapshot, message=None)
            self.render_products()
        self.bound_work(self.lease, lambda: api.retry_setup(setup_id), done, self.nest_failed)

    # ---- company context (B7) ------------------------------------------------------------------

    def nest_orgs(self):
        catalog = getattr(self, "org_state", {}).get("catalog") if hasattr(self, "org_state") else None
        return [o for o in (catalog or {}).get("orgs", []) if o.get("status") in ("active", "past_due", "read_only")]

    def nest_context_picker(self):
        orgs = self.nest_orgs()
        if not orgs:
            return None
        names = ["Personal"] + [o["name"] for o in orgs]
        picker = Gtk.DropDown.new_from_strings(names)
        picker.set_tooltip_text("Whose Nests")
        picker.update_property([Gtk.AccessibleProperty.LABEL], ["Show Nests for"])
        selected = self.nest_state["org"]
        index = next((i + 1 for i, o in enumerate(orgs) if o["id"] == selected), 0)
        picker.set_selected(index)

        def changed(widget, _param):
            i = widget.get_selected()
            org = orgs[i - 1]["id"] if i > 0 else None
            if org != self.nest_state["org"]:
                self.nest_state.update(org=org, org_nests=None)
                if org:
                    self.load_org_nests(org)
                self.render_products()
        picker.connect("notify::selected", changed)
        row = box(8, horizontal=True)
        row.append(label("Show Nests for"))
        row.append(picker)
        return row

    def load_org_nests(self, org):
        state, api = self.nest_state, self.nest_api()

        def done(result):
            if state is self.nest_state and state["org"] == org:
                state["org_nests"] = result
                self.render_products()
        self.bound_work(self.lease, lambda: api.org_nests(org), done, self.nest_failed)

    def render_org_nests(self, content):
        data = self.nest_state["org_nests"]
        if data is None:
            content.append(label("Loading…", "dim-label"))
            return
        content.append(label(f"{data['used']} of {data['slots']} organization Nests in use.", wrap=True))
        for nest in data["nests"]:
            row = box(8, horizontal=True)
            name = label(f"{nest['label']} ({'admin' if nest['role'] == 'owner' else 'operator'})")
            name.set_hexpand(True)
            row.append(name)
            row.append(button("Open", lambda n=nest: self.open_nest(n["nest_id"])))
            content.append(row)
        if not data["nests"]:
            content.append(label("No Nests you can use in this organization yet.", "dim-label"))
        org = self.nest_state["org"]
        if nc.org_can_manage(next((o for o in self.nest_orgs() if o["id"] == org), None)):
            content.append(button("New organization Nest…", lambda: self.create_org_nest(org)))
            content.append(button("Buy a package for the organization on the website",
                                  lambda: self.open_url("https://enterprise.pigeonpost.dev/")))

    def create_org_nest(self, org):
        window, content = self.dialog("New organization Nest", 420)
        handle = Gtk.Entry(placeholder_text="Address in the organization, e.g. hr")
        handle.update_property([Gtk.AccessibleProperty.LABEL], ["Address"])
        name = Gtk.Entry(placeholder_text="Name")
        name.update_property([Gtk.AccessibleProperty.LABEL], ["Name"])
        error = label("", "error", wrap=True)
        for w in (handle, name, error):
            content.append(w)
        api = self.nest_api()

        def create():
            h, n = handle.get_text().strip(), name.get_text().strip()
            if not h or nc.label_problem(n):
                error.set_text("Give the Nest an address and a name of 1 to 64 characters.")
                return

            def done(_):
                window.close()
                self.toast("Nest created")
                self.load_org_nests(org)
            self.bound_work(self.lease, lambda: api.create_org_nest(org, h, n), done, lambda e: error.set_text(nc.explain(e)))
        content.append(button("Create", create, style="suggested-action"))
        window.present()

    # ---- a Nest's page (B4/B5) -----------------------------------------------------------------

    def build_nest_page(self):
        self.nest_content = box(12, margin=24)
        self.nest_content.set_size_request(420, -1)
        page = box(0)
        page.add_css_class("pp-wash")
        page.append(scroll(self.nest_content))
        return page

    def open_nest(self, nest_id, keep_message=False):
        state = self.nest_state
        state.update(selected=nest_id, detail=None, files=None, cards=None, usage=None,
                     message=state["message"] if keep_message else None)
        self.show_page("nest")
        api = self.nest_api()

        def work():
            detail = api.nest(nest_id)
            files = api.files(nest_id)
            cards = api.approvals(nest_id)
            try:
                usage = api.usage(nest_id)
            except Exception:  # usage needs the Nests host; the rest of the page still works
                usage = None
            return detail, files, cards, usage

        def done(result):
            if state is self.nest_state and state["selected"] == nest_id:
                state.update(detail=result[0], files=result[1], cards=result[2], usage=result[3])
                self.render_nest()
        self.bound_work(self.lease, work, done, self.nest_failed)

    def render_nest(self):
        content = getattr(self, "nest_content", None)
        if content is None:
            return
        clear(content)
        state = self.nest_state
        content.append(button("Back to Products", lambda: self.show_page("products"), "go-previous-symbolic", "flat"))
        if state["message"]:
            content.append(label(state["message"], "error", wrap=True))
        nest = state["detail"]
        if nest is None:
            if not state["message"]:
                content.append(label("Loading…", "dim-label"))
            return
        text, reason = nc.status_line(nest)
        content.append(label(nest["label"], "title-2"))
        status = label(f"{text}" + (f" — {reason}" if reason else ""), wrap=True)
        status.update_property([Gtk.AccessibleProperty.LABEL], [f"Status: {text}" + (f". {reason}" if reason else "")])
        content.append(status)
        notice = nc.deletion_notice(nest)
        if notice:
            content.append(label(notice, "error", wrap=True))
        if nest["address"]:
            content.append(button("Open conversation", lambda: self.open_peer_conversation(nest["address"]), "mail-message-new-symbolic"))
        owner = nest["role"] == "owner"
        if owner:
            actions = box(8, horizontal=True)
            if nc.can_resume(nest):
                actions.append(button("Resume", lambda: self.nest_pause(False)))
            elif nc.can_pause(nest):
                actions.append(button("Pause", lambda: self.nest_pause(True)))
            actions.append(button("Rename…", self.nest_rename))
            actions.append(button("Export…", self.nest_export))
            actions.append(button("Delete…", self.nest_delete, style="destructive-action"))
            content.append(actions)
        # Usage
        content.append(label("Usage", "heading"))
        for line in state["usage"] or ["Usage is not available right now."]:
            content.append(label(line, wrap=True))
        # Files
        files = state["files"]
        content.append(label("Files", "heading"))
        if files:
            content.append(label(f"{files['used_bytes'] / 1048576:.1f} MiB of {files['quota_bytes'] / 1073741824:.0f} GiB used", "dim-label"))
            for f in files["files"]:
                row = box(8, horizontal=True)
                info = label(f"{f['name']} — {nc.coverage_text(f)}", wrap=True)
                info.set_hexpand(True)
                if f["warnings"]:
                    info.set_tooltip_text("\n".join(f["warnings"]))
                row.append(info)
                row.append(button("Delete file", lambda f=f: self.nest_delete_file(f), "user-trash-symbolic", "flat"))
                content.append(row)
        content.append(button("Add a file…", self.nest_add_file, "list-add-symbolic"))
        # Approvals
        content.append(label("Approvals", "heading"))
        cards = nc.pending_cards(state["cards"])
        if not cards:
            content.append(label("Nothing is waiting for your approval.", "dim-label"))
        for card in cards:
            content.append(self.approval_card(nest["nest_id"], card))

    def approval_card(self, nest_id, card):
        """One proposal, shown in full. Approve is offered only for a call that can be shown."""
        text = nc.card_text(card)
        panel = box(6)
        panel.add_css_class("card")
        panel.append(label(text, wrap=True))
        expiry = nc.expiry_text(card)
        if expiry:
            panel.append(label(expiry, "dim-label"))
        row = box(8, horizontal=True)
        if nc.can_approve(card):
            row.append(button("Approve", lambda: self.nest_decide(nest_id, card, True, row), style="suggested-action"))
        row.append(button("Reject", lambda: self.nest_decide(nest_id, card, False, row)))
        panel.append(row)
        panel.update_property([Gtk.AccessibleProperty.LABEL], ["Approval: " + text])
        return panel

    def open_peer_conversation(self, address):
        opener = getattr(self, "open_conversation", None)
        if callable(opener):
            opener(address)
        else:
            self.peer = address
            self.show_page("conversation")
            self.render_all()

    def nest_reload(self):
        if self.nest_state["selected"]:
            self.open_nest(self.nest_state["selected"])

    def nest_changed(self):
        """After an action: the Nest's page, the snapshot behind the header, and the open conversation."""
        self.nest_reload()
        self.load_nests()
        self.refresh_peer_nest(force=True)

    def nest_pause(self, pause):
        nest_id, api = self.nest_state["selected"], self.nest_api()
        self.bound_work(self.lease, lambda: api.pause(nest_id, resume=not pause),
                        lambda _: (self.toast("Paused" if pause else "Resumed"), self.nest_changed()), self.nest_failed)

    def nest_rename(self):
        nest = self.nest_state["detail"]
        api = self.nest_api()

        def submitted(value):
            problem = nc.label_problem(value)
            if problem:
                raise ValueError(problem)
            self.bound_work(self.lease, lambda: api.rename(nest["nest_id"], value),
                            lambda _: (self.toast("Renamed"), self.nest_changed()), self.nest_failed)
        self.form_dialog("Rename Nest", "Name", submitted, initial=nest["label"])

    def nest_delete(self):
        nest = self.nest_state["detail"]
        window, content = self.dialog("Delete Nest", 440)
        content.append(label(f"This deletes “{nest['label']}”, its files and its conversations for good. Export first if you "
                             "want to keep them. Type the Nest’s name to confirm.", wrap=True))
        entry = Gtk.Entry(placeholder_text=nest["label"])
        entry.update_property([Gtk.AccessibleProperty.LABEL], ["Type the Nest’s name to confirm"])
        content.append(entry)
        error = label("", "error", wrap=True)
        content.append(error)
        api = self.nest_api()

        def delete():
            typed = entry.get_text()
            if not nc.delete_confirmed(nest["label"], typed):
                error.set_text("Type the Nest’s name exactly to delete it.")
                return

            def done(_):
                window.close()
                self.toast("Nest deleted")
                self.nest_state["selected"] = None
                self.show_page("products")
                self.load_nests()
            self.bound_work(self.lease, lambda: api.delete(nest["nest_id"], nest["label"], typed), done,
                            lambda e: error.set_text(nc.explain(e)))
        content.append(button("Delete Nest", delete, style="destructive-action"))
        window.present()

    def nest_export(self):
        nest = self.nest_state["detail"]
        chooser = Gtk.FileChooserNative.new("Export Nest", self, Gtk.FileChooserAction.SAVE, "Save", "Cancel")
        chooser.set_current_name("".join(c for c in nest["label"] if c.isalnum() or c in " -_")[:40].strip() + ".zip")
        api = self.nest_api()

        def chosen(dialog, response):
            file = dialog.get_file() if response == Gtk.ResponseType.ACCEPT else None
            dialog.destroy()
            if not file:
                return

            def save():
                data = api.export(nest["nest_id"])
                file.replace_contents(data, None, False, Gio.FileCreateFlags.REPLACE_DESTINATION, None)
            self.bound_work(self.lease, save, lambda _: self.toast("Nest exported"), self.nest_failed)
        chooser.connect("response", chosen)
        chooser.show()

    def nest_add_file(self, nest_id=None):
        nest_id, api = nest_id or self.nest_state["selected"], self.nest_api()
        chooser = Gtk.FileChooserNative.new("Add a file to the Nest", self, Gtk.FileChooserAction.OPEN, "Add", "Cancel")

        def chosen(dialog, response):
            file = dialog.get_file() if response == Gtk.ResponseType.ACCEPT else None
            dialog.destroy()
            if not file:
                return

            def upload():
                path = file.get_path()
                with open(path, "rb") as handle:
                    data = handle.read(nc.MAX_UPLOAD + 1)
                return api.upload(nest_id, file.get_basename() or "file", data)
            self.toast("Uploading…")
            self.bound_work(self.lease, upload, lambda result: self.nest_file_added(nest_id, result), self.nest_action_failed)
        chooser.connect("response", chosen)
        chooser.show()

    def nest_file_added(self, nest_id, result):
        try:
            self.toast(nc.upload_outcome(result))
        except nc.NestError:
            self.toast("File added")
        if self.page() == "nest" and self.nest_state["selected"] == nest_id:
            self.nest_reload()

    def nest_delete_file(self, f):
        nest_id, api = self.nest_state["selected"], self.nest_api()
        self.confirm(f"Delete {f['name']}?", "The file and the text read from it are removed from the Nest.",
                     lambda: self.bound_work(self.lease, lambda: api.delete_file(nest_id, f["file_id"]),
                                             lambda _: (self.toast("File deleted"), self.nest_reload()), self.nest_failed))

    def nest_decide(self, nest_id, card, approve, row=None):
        """Decide on exactly the card shown. A refusal stays on screen while the cards reload."""
        api = self.nest_api()
        if row is not None:
            row.set_sensitive(False)  # the server executes a card once; this only stops a second click

        def reload(keep_message):
            if self.page() == "nest" and self.nest_state["selected"] == nest_id:
                self.open_nest(nest_id, keep_message=keep_message)
            self.refresh_peer_nest(force=True)

        def done(_):
            self.toast("Approved and done" if approve else "Rejected")
            reload(False)

        def failed(error):
            if row is not None:
                row.set_sensitive(True)
            self.nest_action_failed(error)
            reload(True)
        self.bound_work(self.lease, lambda: api.decide(nest_id, card, approve), done, failed)
