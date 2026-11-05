"""Nests on Linux (docs/nests/phase-b.md): the Products page and Nest management.

Products has three tabs: Buy an Inbox (names, as before), Buy Nests and Services. Desktop has no
in-app payment: buying, upgrading and managing a package open the website. Everything shown is
the postbox's answer; each action asks again. The page and the sidebar entry exist only while the
postbox reports the `nests` feature for this account.

A Nest's conversation shows its status (Ready, Working, Needs you, Paused, Unavailable) in the
header; its page lists files, approvals and usage and offers pause, rename, export and delete.
"""

import time

import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gio, Gtk

from . import nests_core as nc
from .widgets import box, button, clear, label, scroll

REFRESH = 60


class NestsMixin:
    # ---- lifecycle -----------------------------------------------------------------------------

    def reset_nests(self):
        self.nest_state = {"packages": None, "snapshot": None, "loaded_at": 0, "loading": False, "message": None,
                           "selected": None, "detail": None, "files": None, "cards": None, "usage": None,
                           "org": None, "org_nests": None, "busy": False}
        if getattr(self, "products_content", None) is not None:
            self.render_products()
        if getattr(self, "nest_badge", None) is not None:
            self.nest_badge.set_visible(False)

    def nests_on(self):
        return nc.nests_on(self.features) and self.lease is not None

    def nests_features_changed(self):
        on = self.nests_on()
        self.products_button.set_visible(on)
        if not on:
            if self.page() in ("products", "nest"):
                self.main_stack.set_visible_child_name("conversation")
            return
        if self.nest_state["snapshot"] is None and not self.nest_state["loading"]:
            self.load_nests()

    def nests_tick(self):
        if self.nests_on() and self.page() in ("products", "nest") and not self.nest_state["loading"] \
                and time.monotonic() - self.nest_state["loaded_at"] > REFRESH:
            self.load_nests()

    def page_shown_products(self):
        self.render_products()
        if self.nests_on():
            self.load_nests()

    def page_shown_nest(self):
        self.render_nest()

    def nest_api(self):
        return nc.NestsAPI(self.api_for(self.lease))

    def nest_failed(self, error):
        self.nest_state.update(loading=False, busy=False, message=nc.explain(error))
        if getattr(error, "status", 0) in (401, 403) or getattr(error, "code", "") == "feature_disabled":
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

        def work():
            return api.packages(), api.snapshot()

        def loaded(result):
            if state is not self.nest_state:
                return
            packages, snapshot = result
            state.update(packages=packages, snapshot=snapshot, loading=False, loaded_at=time.monotonic(), message=None)
            self.render_products()
            self.render_conversation_chrome()
        self.bound_work(lease, work, loaded, lambda e: state is self.nest_state and self.nest_failed(e))

    def nest_for_peer(self, peer):
        snap = self.nest_state.get("snapshot") if hasattr(self, "nest_state") else None
        if not snap or not peer:
            return None
        return next((n for n in snap["nests"] if n["address"] == peer), None)

    # ---- conversation header (B3) --------------------------------------------------------------

    def nest_chrome(self):
        """Called from render_conversation_chrome: a Nest's status beside its name."""
        nest = self.nest_for_peer(self.peer) if self.nests_on() else None
        if nest is None:
            self.nest_badge.set_visible(False)
            self.nest_manage_button.set_visible(False)
            return
        text, reason = nc.status_line(nest)
        self.nest_badge.set_text(text)
        self.nest_badge.set_tooltip_text(reason or text)
        self.nest_badge.update_property([Gtk.AccessibleProperty.LABEL], [f"Nest status: {text}" + (f". {reason}" if reason else "")])
        self.nest_badge.set_visible(True)
        self.nest_manage_button.set_visible(True)

    def open_nest_for_peer(self):
        nest = self.nest_for_peer(self.peer)
        if nest:
            self.open_nest(nest["nest_id"])

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
            text = f"{setup['label']}: paid, setting up" + (" failed" if setup["status"] == "failed" else "…")
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
        content.append(label("Packages", "heading"))
        for package in (state["packages"] or {}).get("packages", []):
            content.append(self.package_card(package, snap))
        content.append(button("Buy, upgrade or manage on the website", lambda: self.open_url(nc.WEB_PRODUCTS),
                              style="suggested-action"))

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

    def nest_context_picker(self):
        catalog = getattr(self, "org_state", {}).get("catalog") if hasattr(self, "org_state") else None
        orgs = [o for o in (catalog or {}).get("orgs", []) if o.get("status") in ("active", "past_due", "read_only")]
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
            if not h or not n:
                error.set_text("Give the Nest an address and a name.")
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

    def open_nest(self, nest_id):
        state = self.nest_state
        state.update(selected=nest_id, detail=None, files=None, cards=None, usage=None, message=None)
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
            content.append(label("Loading…", "dim-label"))
            return
        text, reason = nc.status_line(nest)
        content.append(label(nest["label"], "title-2"))
        status = label(f"{text}" + (f" — {reason}" if reason else ""), wrap=True)
        status.update_property([Gtk.AccessibleProperty.LABEL], [f"Status: {text}" + (f". {reason}" if reason else "")])
        content.append(status)
        if nest["address"]:
            content.append(button("Open conversation", lambda: self.open_peer_conversation(nest["address"]), "mail-message-new-symbolic"))
        owner = nest["role"] == "owner"
        if owner:
            actions = box(8, horizontal=True)
            paused = nest["state"] == "paused"
            actions.append(button("Resume" if paused else "Pause", lambda: self.nest_pause(not paused)))
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
        cards = [c for c in (state["cards"] or []) if c["status"] == "pending"]
        if not cards:
            content.append(label("Nothing is waiting for your approval.", "dim-label"))
        for card in cards:
            panel = box(6)
            panel.add_css_class("card")
            panel.append(label(nc.card_text(card), wrap=True))
            row = box(8, horizontal=True)
            row.append(button("Approve", lambda c=card: self.nest_decide(c, True), style="suggested-action"))
            row.append(button("Reject", lambda c=card: self.nest_decide(c, False)))
            panel.append(row)
            panel.update_property([Gtk.AccessibleProperty.LABEL], ["Approval: " + nc.card_text(card)])
            content.append(panel)

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

    def nest_pause(self, pause):
        nest_id, api = self.nest_state["selected"], self.nest_api()
        self.bound_work(self.lease, lambda: api.pause(nest_id, resume=not pause),
                        lambda _: (self.toast("Paused" if pause else "Resumed"), self.nest_reload()), self.nest_failed)

    def nest_rename(self):
        nest = self.nest_state["detail"]
        api = self.nest_api()

        def submitted(value):
            if not value or len(value) > 64:
                raise ValueError("Use 1 to 64 characters.")
            self.bound_work(self.lease, lambda: api.rename(nest["nest_id"], value),
                            lambda _: (self.toast("Renamed"), self.nest_reload(), self.load_nests()), self.nest_failed)
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

    def nest_add_file(self):
        nest_id, api = self.nest_state["selected"], self.nest_api()
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
            self.bound_work(self.lease, upload, lambda _: (self.toast("File added"), self.nest_reload()), self.nest_failed)
        chooser.connect("response", chosen)
        chooser.show()

    def nest_delete_file(self, f):
        nest_id, api = self.nest_state["selected"], self.nest_api()
        self.confirm(f"Delete {f['name']}?", "The file and the text read from it are removed from the Nest.",
                     lambda: self.bound_work(self.lease, lambda: api.delete_file(nest_id, f["file_id"]),
                                             lambda _: (self.toast("File deleted"), self.nest_reload()), self.nest_failed))

    def nest_decide(self, card, approve):
        nest_id, api = self.nest_state["selected"], self.nest_api()
        self.bound_work(self.lease, lambda: api.decide(nest_id, card, approve),
                        lambda _: (self.toast("Approved and done" if approve else "Rejected"), self.nest_reload()),
                        lambda e: (self.nest_failed(e), self.nest_reload()))
