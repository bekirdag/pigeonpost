"""Approving a machine (docs/device-pairing-native.md): the confirmation dialog.

The store (pairing.ApprovalStore) belongs to the accepted login; the dialog only shows its state and
passes the person's explicit decisions to it. Approval needs a strong sign-in, which the Linux
OAuth client cannot do (strong_signin.supported() is False): an approvable request is approved in
the browser (pair.html with the code in the fragment), never automatically. Deny stays native.
"""

import time

import gi
gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk

from . import pairing
from .orgs_core import ORGS
from .strong_signin import StrongSignIn
from .theme import add_css
from .widgets import box, button, clear, label, settings_row

add_css("""
.pp-confirmation { font-family: "Geist Mono", monospace; font-size: 30px; font-weight: 600; letter-spacing: 0.08em; }
.pp-pair-handle { font-family: "Geist Mono", monospace; font-size: 18px; font-weight: 600; }
.pp-hold { min-height: 40px; }
""")

HOLD_SECONDS = 1.5
CHECK_TEXT = ("Check that your terminal shows the same number. If it doesn’t, or you didn’t start this, choose Deny.")
NETWORK_NOTE = "from the network; not proof of anything"


class PairingMixin:
    def reset_pairing(self):
        store = getattr(self, "pairing_store", None)
        if store is not None:
            store.retire()
        self.pairing_store = None
        window = getattr(self, "pairing_window", None)
        self.pairing_window = None
        if window is not None:
            window.close()

    def pairing_settings_rows(self, navigation):
        if ORGS not in self.features or self.lease is None:
            return []
        return [settings_row("Approve a machine", "Let a terminal sign in as one of your organization’s agents",
                             "computer-symbolic", self.approve_device)]

    def strong_signin(self):
        """The system-browser strong sign-in, when this app's OAuth client supports it."""
        flow = getattr(self, "pairing_strong", None)
        if flow is None:
            flow = self.pairing_strong = StrongSignIn(self.session, open_url=self.open_url)
        return flow

    def pairing_store_for_login(self):
        store = self.pairing_store
        if store is None or store.lease is not self.lease or not store.current:
            if store is not None:
                store.retire()
            store = self.pairing_store = pairing.ApprovalStore(
                self.lease, pairing.PairingAPI(self.api_for(self.lease)), strong_available=self.strong_signin().available)
        return store

    def approve_device(self, text=None):
        if self.lease is None or not self.lease.current:
            return None
        store = self.pairing_store_for_login()
        if self.pairing_window is not None:
            self.pairing_window.present()
            return self.pairing_window
        window, content = self.dialog("Approve a machine", 540)
        self.pairing_window = window
        window.pairing = {"content": content, "store": store, "timer": None, "hold": None}

        def closed(_):
            if self.pairing_window is window:
                self.pairing_window = None
                store.clear()  # the code is forgotten with the dialog
            timer = window.pairing.get("timer")
            if timer:
                GLib.source_remove(timer)
                window.pairing["timer"] = None
            return False
        window.connect("close-request", closed)
        window.pairing["timer"] = GLib.timeout_add_seconds(1, lambda: self.pairing_tick(window))
        self.render_pairing_now(window)
        window.present()
        if text:
            self.pairing_resolve(window, text)
        return window

    def pairing_tick(self, window):
        if self.pairing_window is not window:
            return False
        store = window.pairing["store"]
        if store.refresh_expiry() or store.snapshot()["phase"] == pairing.REVIEW:
            self.render_pairing(window, keep_entry=True)
        return True

    def pairing_run(self, window, operation):
        store = window.pairing["store"]
        lease = store.lease
        self.render_pairing(window)

        def done(_):
            if self.pairing_window is window:
                self.render_pairing(window)
        self.bound_work(lease, operation, done, lambda _: done(None))

    def pairing_resolve(self, window, text):
        store = window.pairing["store"]
        if pairing.parse_input(text) is None:
            store.resolve(text)  # sets the message only
            self.render_pairing(window)
            return
        self.pairing_run(window, lambda: store.resolve(text))

    def render_pairing(self, window, keep_entry=False):
        """Coalesced on idle, so a button is never destroyed inside its own click."""
        state = window.pairing
        state["keep"] = state.get("keep", True) and keep_entry if state.get("render_pending") else keep_entry
        if state.get("render_pending"):
            return
        state["render_pending"] = True

        def run():
            state["render_pending"] = False
            if self.pairing_window is window:
                self.render_pairing_now(window, state.pop("keep", False))
            return False
        GLib.idle_add(run)

    def render_pairing_now(self, window, keep_entry=False):
        content = window.pairing["content"]
        store = window.pairing["store"]
        snap = store.snapshot()
        if keep_entry and snap["phase"] == pairing.REVIEW and window.pairing.get("countdown") is not None:
            self.update_countdown(window, snap)
            return
        window.set_focus(None)  # the focused entry or button is about to be replaced
        clear(content)
        window.pairing["countdown"] = None
        phase, preview = snap["phase"], snap["preview"]
        if phase in (pairing.IDLE, pairing.RESOLVING) or (phase == pairing.UNAVAILABLE and not preview):
            content.append(label("Enter the code shown on the machine, or paste its pairing link.", wrap=True))
            entry = Gtk.Entry(placeholder_text="ABCD-EFGH", max_length=4096)
            entry.update_property([Gtk.AccessibleProperty.LABEL], ["Pairing code"])
            entry.set_sensitive(not snap["busy"])
            content.append(entry)
            check = button("Check the request", lambda: self.pairing_resolve(window, entry.get_text()), style="suggested-action")
            check.set_sensitive(not snap["busy"])
            entry.connect("activate", lambda _: self.pairing_resolve(window, entry.get_text()))
            content.append(check)
            if snap["busy"]:
                content.append(label("Checking…", "pp-status"))
            if snap["message"]:
                content.append(self.pairing_message(snap["message"]))
            window.pairing["entry"] = entry
            entry.grab_focus()
            return
        if preview:
            self.render_pairing_preview(content, window, snap)
        if snap["message"]:
            content.append(self.pairing_message(snap["message"]))
        actions = box(8, horizontal=True)
        if phase == pairing.REVIEW and preview:
            self.render_pairing_decisions(actions, window, snap)
        elif phase in (pairing.APPROVING, pairing.DENYING, pairing.VERIFYING):
            actions.append(label({"approving": "Approving…", "denying": "Denying…", "verifying": "Waiting for your browser…"}[phase], "pp-status"))
        elif phase == pairing.UNCONFIRMED:
            actions.append(button("Check status", lambda: self.pairing_run(window, store.check_status), style="suggested-action"))
        elif phase == pairing.APPROVED:
            content.append(label("Approved. You can close this; the machine is signing in.", "heading", wrap=True))
        elif phase == pairing.DENIED:
            content.append(label("Denied. The machine was not signed in.", "heading", wrap=True))
        elif phase == pairing.EXPIRED:
            content.append(label("This request expired. Start again on the machine.", "heading", wrap=True))
        if phase not in (pairing.APPROVING, pairing.DENYING, pairing.VERIFYING):
            actions.append(button("Another code", lambda: (store.clear(), self.render_pairing(window))))
        content.append(actions)

    def pairing_message(self, text):
        message = label(text, "pp-status", wrap=True)
        message.set_name("pairing-message")
        return message

    def render_pairing_preview(self, content, window, snap):
        p = snap["preview"]
        content.append(label(p["org"]["name"], "pp-meta"))
        handle = label(p["handle"], "pp-pair-handle")
        handle.set_selectable(True)
        content.append(handle)
        if p["creates_agent"]:
            content.append(label("Approving creates this agent in your organization.", "heading", wrap=True))
        content.append(label("Machine: " + (p["label"] or "No label"), wrap=True))
        caps = ", ".join(pairing.CAPABILITY_TEXT.get(c, c) for c in p["capabilities"])
        content.append(label("Requested access: " + caps, wrap=True))
        country = p["ip_country"] or "Unknown"
        content.append(label(f"Location: {country} ({NETWORK_NOTE})", "pp-meta", wrap=True))
        number = label(p["confirmation"], "pp-confirmation")
        number.set_selectable(True)
        number.update_property([Gtk.AccessibleProperty.LABEL], ["Confirmation number " + " ".join(p["confirmation"].replace(" ", ""))])
        content.append(number)
        content.append(label(CHECK_TEXT, wrap=True))
        countdown = label("", "pp-meta")
        window.pairing["countdown"] = countdown
        content.append(countdown)
        self.update_countdown(window, snap)

    def update_countdown(self, window, snap):
        countdown = window.pairing.get("countdown")
        if countdown is None or not snap["preview"]:
            return
        left = int(snap["preview"]["expires_at"] - time.time())
        countdown.set_text(f"Expires in {left // 60}:{left % 60:02d}" if left > 0 else "Expired")

    def render_pairing_decisions(self, actions, window, snap):
        store = window.pairing["store"]
        proof = snap["proof"]
        if snap["can_approve"]:
            actions.append(self.hold_button(window, proof))
        elif snap["needs_browser"]:
            code = pairing.parse_code(snap["code"] or "")
            browser = button("Approve in browser", lambda: self.open_url(pairing.browser_link(code)) if code else None,
                             style="suggested-action")
            browser.set_tooltip_text("Approving machines needs a strong sign-in, which the Linux app can’t do yet.")
            actions.append(browser)
        elif snap["challenge"] is not None:
            actions.append(button("Verify sign-in…", lambda: self.pairing_verify(window, proof), style="suggested-action"))
        else:
            disabled = button("Hold to approve", lambda: None)
            disabled.set_sensitive(False)
            actions.append(disabled)
        if snap["needs_browser"]:
            note = label("Approving machines needs a strong sign-in, which the Linux app can’t do yet. Approve it in your browser instead.",
                         "pp-status", wrap=True)
            note.set_name("pairing-browser-note")
            window.pairing["content"].append(note)
        deny = button("Deny", lambda: self.pairing_run(window, lambda: store.deny(proof)), style="destructive-action")
        deny.set_sensitive(snap["can_deny"])
        actions.append(deny)

    def pairing_verify(self, window, proof):
        store = window.pairing["store"]
        lease = store.lease
        flow = self.strong_signin()
        self.pairing_run(window, lambda: store.verify(proof, lambda requirement: flow.run(requirement, lease)))

    def hold_button(self, window, proof):
        """Hold about 1.5 s with a pointer. Keyboard and assistive activation ask to confirm instead."""
        store = window.pairing["store"]
        holder = box(4)
        widget = Gtk.Button(label="Hold to approve")
        widget.add_css_class("suggested-action")
        widget.add_css_class("pp-hold")
        widget.update_property([Gtk.AccessibleProperty.DESCRIPTION],
                               ["Press and hold to approve, or activate to approve after a confirmation."])
        progress = Gtk.ProgressBar(fraction=0)
        holder.append(widget)
        holder.append(progress)
        hold = {"started": None, "timer": None, "pointer": False, "done": False}
        window.pairing["hold"] = hold

        def approve():
            if not hold["done"]:
                hold["done"] = True
                self.pairing_run(window, lambda: store.approve(proof))

        def step():
            if hold["started"] is None:
                hold["timer"] = None
                return False
            fraction = min(1.0, (time.monotonic() - hold["started"]) / HOLD_SECONDS)
            progress.set_fraction(fraction)
            if fraction >= 1.0:
                hold["timer"], hold["started"] = None, None
                approve()
                return False
            return True

        def pressed(*_):
            hold["pointer"] = True
            hold["started"] = time.monotonic()
            if hold["timer"] is None:
                hold["timer"] = GLib.timeout_add(30, step)

        def released(*_):
            hold["started"] = None
            if not hold["done"]:
                progress.set_fraction(0)

        gesture = Gtk.GestureClick()
        gesture.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        gesture.connect("pressed", pressed)
        gesture.connect("released", released)
        gesture.connect("cancel", lambda *_: released())
        widget.add_controller(gesture)

        def clicked(_):
            if hold["pointer"]:
                hold["pointer"] = False
                if not hold["done"]:
                    self.toast("Keep holding to approve.")
                return
            self.confirm("Approve this machine?", CHECK_TEXT, approve, "Approve")
        widget.connect("clicked", clicked)
        widget.hold = hold
        widget.approve_now = approve
        return holder
