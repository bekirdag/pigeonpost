"""Phase 7 Meeting notes, read in the app (docs/meeting-summaries.md, site-inbox/meeting-notes.js).

Meeting notes belong to the account, not to a mailbox. Every request and every use of this
computer's notes key runs under the account's login lease (binding.Lease): when the person signs
out, signs in to another account or the login is otherwise retired, unfinished work is dropped and
nothing from the earlier login is shown or stored. Each list or brief view also carries a view
number, so a slower, older answer never replaces a newer screen.

The calls themselves open in the browser on Linux (WebKitGTK has no WebRTC); consent and capture
happen there. This page only lists the notes and decrypts them on this computer.
"""

import datetime

from gi.repository import Gtk

from . import meeting_notes as mn
from .widgets import box, button, clear, label, scroll


class NotesMixin:
    def reset_notes(self, keep_login=True):
        """Mailbox switches keep the account's registration; sign-out (keep_login=False) does not."""
        previous = getattr(self, "notes_state", None)
        keep = (keep_login and previous is not None and previous.get("lease") is not None
                and previous["lease"] is self.lease and self.lease is not None and self.lease.current)
        self.notes_state = {"lease": previous["lease"] if keep else None,
                            "key_id": previous["key_id"] if keep else None,
                            "view": 0, "showing": None, "meetings": None, "error": None}
        if getattr(self, "notes_content", None) is not None:
            clear(self.notes_content)

    def notes_keys(self):
        return mn.NotesKeys(self.protected)

    def notes_on(self):
        return mn.MEETING_SUMMARIES in self.features and self.lease is not None and self.lease.current

    def notes_features_changed(self):
        on = self.notes_on()
        self.notes_button.set_visible(on)
        if not on:
            if self.page() == "notes":
                self.main_stack.set_visible_child_name("conversation")
            return
        self.notes_register()

    def notes_register(self):
        """Register this computer's notes key for the account, once per login, so notes made from
        now on can be opened here (the web inbox does the same while the feature is on)."""
        lease, state = self.lease, self.notes_state
        if not self.notes_on() or state["lease"] is lease or not mn.crypto_available():
            return
        state["lease"] = lease
        keys, api = self.notes_keys(), self.api_for(lease)

        def run():
            key = keys.get(lease)
            mn.register(api, key)
            return key.key_id

        def done(key_id):
            if self.notes_state is state:
                state["key_id"] = key_id

        def failed(_error):
            if self.notes_state is state and state["lease"] is lease:
                state["lease"] = None  # tried again on the next capability check or page visit
        self.bound_work(lease, run, done, failed)

    # ---- the page --------------------------------------------------------------------------------

    def build_notes_page(self):
        self.notes_content = box(12, margin=24)
        self.notes_content.set_size_request(420, -1)
        page = box(0)
        page.add_css_class("pp-wash")
        page.append(scroll(self.notes_content))
        return page

    def page_shown_notes(self):
        self.notes_register()
        self.notes_list()

    def notes_header(self, title, back=False):
        content = self.notes_content
        clear(content)
        if back:
            content.append(button("← All meeting notes", self.notes_list, style="flat"))
            content.get_last_child().set_halign(Gtk.Align.START)
        heading = label(title, "title-2", wrap=True)
        content.append(heading)
        self.notes_title = heading
        return content

    def notes_begin(self):
        """A new view: answers for an older one are ignored."""
        self.notes_state["view"] += 1
        return self.notes_state["view"], self.notes_state

    def notes_current(self, view, state):
        return self.notes_state is state and state["view"] == view and self.page() == "notes" and not self.closed

    def notes_list(self):
        content = self.notes_header("Meeting notes")
        content.append(label(mn.INTRO, "dim-label", wrap=True))
        if not mn.crypto_available():
            content.append(label("Meeting notes need the python3-cryptography package on this computer.", wrap=True))
            return
        if not getattr(self.protected, "available", False):
            content.append(label("Your desktop keyring isn’t available, so this computer’s notes key lasts only until the app "
                                 "closes. Notes made while it was set up can be opened until then.", "pp-status", wrap=True))
        status = label("Loading…", "pp-status")
        content.append(status)
        lease = self.lease
        if not self.notes_on():
            status.set_text("Meeting notes aren’t available for this account.")
            return
        view, state = self.notes_begin()
        api = self.api_for(lease)

        def done(body):
            if not self.notes_current(view, state):
                return
            rows = mn.meetings(body)
            state["meetings"] = rows
            content.remove(status)
            if not rows:
                content.append(label("No meeting notes yet. Notes appear here after a call where you agreed to them.", wrap=True))
                return
            listing = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE)
            listing.add_css_class("boxed-list")
            for row in rows:
                listing.append(self.notes_row(row))
            content.append(listing)

        def failed(_error):
            if self.notes_current(view, state):
                status.set_text("Couldn’t load your meeting notes.")
        self.bound_work(lease, lambda: api.call("GET", "/v1/meetings"), done, failed)

    @staticmethod
    def notes_when(stamp):
        if not stamp:
            return "Meeting"
        return datetime.datetime.fromtimestamp(stamp).strftime("%d %B %Y, %H:%M").lstrip("0")

    def notes_row(self, m):
        from .widgets import action_row
        detail = f"{mn.STATE.get(m['state'], m['state'])} · host {m['host']}"
        if m["state"] == "ready" and m["expires_at"]:
            detail += " · kept until " + datetime.date.fromtimestamp(m["expires_at"]).strftime("%d %B %Y").lstrip("0")
        row = action_row(title=self.notes_when(m["created_at"]), subtitle=detail, activatable=True)
        row.meeting = m["id"]
        row.connect("activated", lambda _: self.notes_open(m["id"]))
        return row

    def notes_open(self, mid):
        content = self.notes_header("Meeting notes", back=True)
        status = label("Opening…", "pp-status")
        content.append(status)
        lease = self.lease
        if not self.notes_on():
            return
        view, state = self.notes_begin()
        state["showing"] = mid
        keys, api = self.notes_keys(), self.api_for(lease)

        def run():
            meeting = api.call("GET", "/v1/meetings/" + mid)
            if meeting.get("state") != "ready":
                return meeting, None, None
            try:
                return meeting, mn.open_brief(meeting, keys.get(lease)), None
            except mn.NotesError as error:
                return meeting, None, error

        def done(result):
            if not self.notes_current(view, state):
                return
            meeting, brief, error = result
            content.remove(status)
            if brief is None:
                if meeting.get("state") == "failed":
                    text = mn.FAILURE.get(meeting.get("failure"), "The notes couldn’t be made.")
                elif error is not None:
                    text = mn.NO_KEY if error.code == "no_key_for_device" else "This computer couldn’t open these notes."
                else:
                    text = "The notes are still being prepared. Check back in a few minutes."
                content.append(label(text, wrap=True))
                content.append(self.notes_actions(mid, None))
                return
            self.notes_title.set_text(brief["title"] or "Meeting notes")
            self.notes_render(content, brief)
            content.append(self.notes_actions(mid, brief))

        def failed(_error):
            if self.notes_current(view, state):
                status.set_text("These notes are no longer available.")
        self.bound_work(lease, run, done, failed)

    def notes_render(self, content, b):
        meta = []
        if b["started_at"]:
            try:
                meta.append(datetime.datetime.fromisoformat(b["started_at"].replace("Z", "+00:00")).astimezone().strftime("%d %B %Y, %H:%M").lstrip("0"))
            except ValueError:
                pass
        if b["duration_s"]:
            meta.append(f"{max(1, round(b['duration_s'] / 60))} min")
        if b["participants"]:
            meta.append(", ".join(b["participants"]))
        if meta:
            content.append(label(" · ".join(meta), "dim-label", wrap=True))
        if b["draft"]:
            content.append(label("A summary couldn’t be generated for this meeting. The transcript is below.", "pp-status", wrap=True))

        def section(title, lines):
            if not lines:
                return
            content.append(label(title, "heading"))
            for line in lines:
                text = label("• " + line, wrap=True)
                text.set_selectable(True)
                content.append(text)
        if b["summary"]:
            content.append(label("Summary", "heading"))
            summary = label(b["summary"], wrap=True)
            summary.set_selectable(True)
            content.append(summary)
        section("Decisions", b["decisions"])
        section("Action items", [mn.action_text(a) for a in b["action_items"]])
        section("Open questions", b["open_questions"])
        if b["transcript"]:
            content.append(label("Transcript", "heading"))
            for t in b["transcript"]:
                line = label(f"{t['at']}  {t['speaker']}: {t['text']}", wrap=True)
                line.set_selectable(True)
                content.append(line)
        content.append(label(mn.footer(b), "pp-status", wrap=True))

    def notes_actions(self, mid, brief):
        row = box(8, horizontal=True)
        if brief is not None:
            def copied():
                self.get_clipboard().set(mn.as_text(brief))
                self.toast("Copied.")
            row.append(button("Copy as text", copied))
        row.append(button("Delete my copy", lambda: self.confirm(
            "Delete your copy of these notes?", "Other attendees keep theirs.", lambda: self.notes_delete(mid)), style="destructive-action"))
        return row

    def notes_delete(self, mid):
        lease = self.lease
        if not self.notes_on():
            return
        api = self.api_for(lease)

        def done(_):
            self.toast("Deleted.")
            if self.page() == "notes":
                self.notes_list()

        self.bound_work(lease, lambda: api.call("DELETE", "/v1/meetings/" + mid), done, lambda _e: self.toast("Couldn’t delete."))
