"""Calendar page with real GTK widgets and a fake postbox. Not installed in release packages."""
import copy
import datetime
import json
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

from pigeonpost.api import APIError
from pigeonpost.calendar_core import Attempts
from pigeonpost.protected import MemoryProtectedStore
from pigeonpost.theme import Preferences
from pigeonpost.ui import Adw, Gtk, Window

from test_ui import pump

EVT = "evt_" + "a" * 26
CAL = "cal_" + "c" * 26
TODAY = datetime.datetime.now(datetime.timezone.utc).replace(hour=10, minute=0, second=0, microsecond=0)
KEY = TODAY.strftime("%Y%m%dT%H%M%S")
OCC = f"occ_{'a' * 26}_{KEY}"


class FakeTransport:
    def __init__(self, owner):
        self.owner = owner

    def request(self, method, url, **kwargs):
        self.owner.calls.append((method, url, None, None))
        return self.owner.scheduled


class FakeCalendarAPI:
    def __init__(self):
        self.calls = []
        self.lock = threading.Lock()
        self.fail_next = {}
        self.hold = None
        self.transport = FakeTransport(self)
        start = int(TODAY.timestamp())
        self.occurrence = {"occurrence_id": OCC, "event_id": EVT, "calendar_id": CAL, "all_day": False, "starts_at": start,
                           "ends_at": start + 3600, "title": "Design review", "status": "confirmed", "role": "attendee",
                           "my_rsvp": "accepted", "invited": False, "meeting": {"code": "ABCDEFGHJK"}, "revision": 3}
        self.event = {"id": EVT, "revision": 3, "status": "confirmed", "title": "Design review", "all_day": False,
                      "start": TODAY.strftime("%Y-%m-%dT%H:%M"), "end": (TODAY + datetime.timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M"),
                      "tzid": "UTC", "starts_at": start, "ends_at": start + 3600, "meeting": {"code": "ABCDEFGHJK"},
                      "attendees": [], "my": {"role": "attendee", "attendee_id": "att_" + "d" * 26, "rsvp": "accepted"},
                      "reminders": [{"minutes_before": 10, "channel": "push"}], "can_edit": True, "organizer": {"handle": "/bekir"}}
        self.scheduled = {"state": "open", "occurrences": [{"occurrence_id": OCC, "starts_at": start, "ends_at": start + 3600,
                                                            "opens_at": 0, "closes_at": 4_000_000_000}]}

    def _token(self, rejected=None):
        return "token"

    def capabilities(self):
        return frozenset({"calendar"})

    def identities(self):
        return [{"address": "/k/one", "handle": "/demo/main"}, {"address": "/k/two", "handle": "/demo/agent"}]

    def inbox(self, identity, wait=0, actions=False):
        return []

    def call(self, method, path, identity=None, data=None, **kwargs):
        if path == "/v1/contacts":
            return {"contacts": [], "vocabulary": {}}
        return {"threads": [], "archived": []} if path in ("/v1/threads", "/v1/archive") else {}

    def scoped(self, method, path, identity, data=None, **kwargs):
        raw = kwargs.get("raw")
        with self.lock:
            self.calls.append((method, path, identity, raw))
            failure = self.fail_next.pop((method, path.split("?")[0]), None)
        if method == "POST" and path == "/v1/calendar/events" and self.hold is not None:
            self.hold.wait(5)
        if failure:
            raise failure
        if path.startswith("/v1/calendar/settings"):
            return {"previews": True, "timezone": "UTC", "week_start": 1}
        if path == "/v1/calendars":
            return {"calendars": [{"id": CAL, "name": "Work", "color": "green", "is_default": True, "revision": 1}]}
        if path.startswith("/v1/calendar/events?"):
            return {"occurrences": [copy.deepcopy(self.occurrence)] if identity == "/k/one" else []}
        if path.startswith("/v1/calendar/invitations"):
            return {"invitations": [{"event_id": EVT, "occurrence_id": None, "title": "Offsite", "starts_at": 1, "ends_at": 2,
                                     "organizer": {"handle": "/carol"}}] if "pending" in path and identity == "/k/one" else []}
        if path.startswith("/v1/calendar/events/" + EVT) and method == "GET":
            occ = {"occurrence_id": OCC, "starts_at": self.event["starts_at"], "ends_at": self.event["ends_at"]} if "occurrence_id" in path else None
            return {"event": copy.deepcopy(self.event), "occurrence": occ}
        if path == "/v1/calendar/events" and method == "POST":
            return {"event": {"id": EVT}, "delivery_status": "queued"}
        if path.endswith("/rsvp"):
            return {"rsvp": json.loads(raw)["status"], "revision": 11}
        if path.startswith("/v1/calendar/import/preview"):
            return {"draft_id": "draft_1", "events": [{"index": 0, "title": "Lunch", "start": "2026-11-02T12:00"},
                                                      {"index": 1, "title": "Holiday", "all_day": True, "start_date": "2026-11-03"}]}
        if "/confirm" in path:
            return {"created": [{"id": EVT}]}
        return {}

    def posted(self, path):
        return [c for c in self.calls if c[0] in ("POST", "PATCH", "PUT") and c[1].split("?")[0] == path]


class CalendarPageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = Adw.Application(application_id="dev.pigeonpost.CalendarTests")
        cls.app.register(None)

    def setUp(self):
        self.callback_errors = []
        self.previous_hook = sys.excepthook
        sys.excepthook = lambda kind, error, trace: self.callback_errors.append(error)
        self.api = FakeCalendarAPI()
        self.store = MemoryProtectedStore()
        self.config = tempfile.TemporaryDirectory(prefix="pigeonpost-cal-")
        self.window = Window(self.app, api=self.api, restore=False, preferences=Preferences(Path(self.config.name) / "p.json"),
                             protected=self.store)
        self.notices, self.opened = [], []
        self.window.notice = lambda title, body: self.notices.append((title, body))
        self.window.open_url = self.opened.append
        self.window.present()
        self.window.account_loaded(self.api.identities())
        pump(lambda: self.window.calendar_button.get_visible(), timeout=3)
        self.window.show_page("calendar")
        pump(lambda: self.window.cal["occurrences"], timeout=3)

    def tearDown(self):
        self.window.close()
        pump(timeout=0.05)
        sys.excepthook = self.previous_hook
        self.config.cleanup()
        self.assertEqual(self.callback_errors, [], "GTK callbacks raised exceptions")

    def chips(self, root=None):
        found, stack = [], [root or self.window.cal_body]
        while stack:
            widget = stack.pop()
            if hasattr(widget, "occurrence"):
                found.append(widget)
            child = widget.get_first_child()
            while child:
                stack.append(child)
                child = child.get_next_sibling()
        return found

    def test_views_render_the_occurrence_and_invitations(self):
        window = self.window
        self.assertTrue(window.calendar_button.get_visible())
        self.assertEqual(window.page(), "calendar")
        self.assertEqual(len(self.chips()), 1)
        self.assertIn("Design review", self.chips()[0].get_tooltip_text())
        self.assertEqual(window.cal_invites_button.get_label(), "Invitations (1)")
        for view in ("day", "month", "agenda"):
            window.cal_view_buttons[view].set_active(True)
            pump(lambda: window.cal.get("shown", (None,))[0] == view and not window.cal["loading"], timeout=3)
            self.assertEqual(len(self.chips()), 1, view)
        window.cal_view_buttons["week"].set_active(True)
        pump(lambda: not window.cal["loading"], timeout=2)

    def test_create_is_saved_first_and_an_uncertain_answer_can_be_retried_exactly(self):
        window = self.window
        editor = window.cal_editor()
        fields = editor.cal_editor
        fields["title"].set_text("Planning")
        self.api.fail_next[("POST", "/v1/calendar/events")] = APIError()  # no answer
        fields["submit"]()
        pump(lambda: self.notices, timeout=3)
        self.assertEqual(self.notices[-1][0], "The event was not confirmed")
        pump(lambda: window.cal["pending"], timeout=3)
        first = self.api.posted("/v1/calendar/events")
        self.assertEqual(len(first), 1)
        self.assertIn(b'"request_key"', first[0][3])
        retry = window.cal_banner.get_first_child()
        self.assertIsNotNone(retry)
        window.cal_retry(window.cal["pending"][0])
        pump(lambda: not window.cal["pending"] and len(self.api.posted("/v1/calendar/events")) == 2, timeout=3)
        sent = self.api.posted("/v1/calendar/events")
        self.assertEqual(sent[0][3], sent[1][3], "the retry is the same bytes and request key")
        self.assertEqual(Attempts(self.store).pending(window.mailbox_lease, "/k/one"), [])

    def test_a_definite_refusal_clears_the_saved_write_and_keeps_the_form(self):
        editor = self.window.cal_editor()
        editor.cal_editor["title"].set_text("Too many")
        self.api.fail_next[("POST", "/v1/calendar/events")] = APIError(422, "attendee_limit")
        editor.cal_editor["submit"]()
        pump(lambda: editor.cal_editor["error"].get_text(), timeout=3)
        self.assertIn("Too many guests", editor.cal_editor["error"].get_text())
        self.assertEqual(Attempts(self.store).pending(self.window.mailbox_lease, "/k/one"), [])
        self.assertTrue(editor.get_visible())

    def test_a_write_finishing_after_a_mailbox_switch_never_lands_in_the_other_mailbox(self):
        window = self.window
        self.api.hold = threading.Event()
        editor = window.cal_editor()
        editor.cal_editor["title"].set_text("Slow")
        editor.cal_editor["submit"]()
        pump(lambda: self.api.posted("/v1/calendar/events"), timeout=3)
        window.mailbox_picker.set_selected(1)
        pump(lambda: window.identity == "/k/two", timeout=2)
        self.api.hold.set()
        pump(timeout=0.5)
        self.assertEqual(window.cal["occurrences"], [])
        self.assertEqual(self.chips(), [])
        old = window.lease.child("/k/one")
        self.assertEqual(Attempts(self.store).pending(old, "/k/one"), [], "the confirmed write was still cleaned up")

    def test_a_stale_revision_reloads_and_never_overwrites(self):
        window = self.window
        detail = {"event": dict(window.cal_client().event(EVT)["event"]), "occurrence": None}
        editor = window.cal_editor(detail)
        self.api.event["revision"] = 7
        self.api.fail_next[("PATCH", "/v1/calendar/events/" + EVT)] = APIError(409, "revision_conflict")
        editor.cal_editor["title"].set_text("Mine")
        editor.cal_editor["submit"]()
        pump(lambda: editor.cal_editor["state"]["revision"] == 7, timeout=3)
        self.assertIn("changed elsewhere", editor.cal_editor["error"].get_text())
        patches = self.api.posted("/v1/calendar/events/" + EVT)
        self.assertEqual(json.loads(patches[0][3])["expected_revision"], 3)
        self.assertEqual(len(patches), 1, "nothing was resent automatically")

    def test_a_time_that_happens_twice_is_asked_not_sent(self):
        editor = self.window.cal_editor()
        fields = editor.cal_editor
        fields["title"].set_text("Night")
        fields["zone"].set_selected(fields["zones"].index("America/New_York"))
        fields["start_date"].set_text("2026-11-01")
        fields["end_date"].set_text("2026-11-01")
        fields["start_time"].set_text("01:30")
        fields["end_time"].set_text("03:30")
        fields["submit"]()
        pump(timeout=0.2)
        self.assertEqual(self.api.posted("/v1/calendar/events"), [])
        fields["state"]["fold"] = "later"
        fields["submit"]()
        pump(lambda: self.api.posted("/v1/calendar/events"), timeout=3)
        self.assertEqual(json.loads(self.api.posted("/v1/calendar/events")[0][3])["fold"], "later")

    def test_join_checks_the_occurrence_and_opens_the_browser(self):
        window = self.window
        window.cal_join(EVT, OCC)
        pump(lambda: self.opened, timeout=3)
        self.assertEqual(self.opened, ["https://inbox.pigeonpost.dev/m/ABCDEFGHJK"])
        self.assertTrue(any(c[1].endswith("/v1/meetings/scheduled/ABCDEFGHJK") for c in self.api.calls))
        self.api.event["my"]["rsvp"] = "declined"
        window.cal_join(EVT, OCC)
        pump(lambda: self.notices, timeout=3)
        self.assertIn("cannot join", self.notices[-1][1])
        self.assertEqual(len(self.opened), 1)

    def test_rsvp_uses_the_attendee_revision_never_the_events(self):
        window = self.window
        window.cal_rsvp(EVT, "tentative", OCC, False)
        pump(lambda: window.cal["rsvp_revisions"].get(EVT) == 11, timeout=3)
        window.cal_rsvp(EVT, "accepted", OCC, True)
        pump(lambda: len(self.api.posted(f"/v1/calendar/events/{EVT}/rsvp")) == 2, timeout=3)
        first, second = [json.loads(c[3]) for c in self.api.posted(f"/v1/calendar/events/{EVT}/rsvp")]
        self.assertNotIn("expected_revision", first)
        self.assertEqual((second["expected_revision"], second["scope"], second["occurrence_id"]), (11, "this", OCC))

    def test_detail_dialog_and_reminder_notification(self):
        window = self.window
        detail = window.cal_client().event(EVT, OCC)
        dialog = window.cal_detail_dialog(detail)
        texts, stack = [], [dialog]
        while stack:
            widget = stack.pop()
            if isinstance(widget, Gtk.Label):
                texts.append(widget.get_text())
            child = widget.get_first_child()
            while child:
                stack.append(child)
                child = child.get_next_sibling()
        self.assertTrue(any("ABCDE-FGHJK" in t for t in texts))
        self.assertTrue(any("not end-to-end encrypted" in t for t in texts))
        self.assertFalse(any("end-to-end encrypted" in t and "not" not in t for t in texts))
        dialog.close()
        sent = []
        window.get_application = lambda: type("App", (), {"send_notification": lambda _s, key, n: sent.append(key),
                                                          "lookup_action": lambda _s, n: True})()
        o = dict(window.cal["occurrences"][0], starts_at=int(time.time()) + 600 - 2)
        window.cal["upcoming"], window.cal["upcoming_at"] = [o], time.time()
        window.cal["reminders"] = {EVT: (3, [10])}
        window.calendar_tick()
        window.calendar_tick()
        self.assertEqual(len(sent), 1, "one notification per reminder")
        del window.get_application

    def test_import_preview_and_replay_safe_confirmation(self):
        window = self.window
        path = Path(self.config.name) / "sample.ics"
        path.write_bytes(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
        shown = []
        original = window.cal_import_dialog
        window.cal_import_dialog = lambda preview: shown.append(original(preview))
        window.cal_import_path(str(path))
        pump(lambda: shown, timeout=3)
        dialog = shown[0].cal_import
        dialog["checks"][1][1].set_active(False)
        self.api.fail_next[("POST", "/v1/calendar/import/draft_1/confirm")] = APIError()
        dialog["confirm"]()
        pump(lambda: dialog["error"].get_text(), timeout=3)
        self.assertIn("never added twice", dialog["error"].get_text())
        dialog["confirm"]()
        pump(lambda: len(self.api.posted("/v1/calendar/import/draft_1/confirm")) == 2, timeout=3)
        bodies = [c[3] for c in self.api.posted("/v1/calendar/import/draft_1/confirm")]
        self.assertEqual(bodies[0], bodies[1])
        self.assertEqual(json.loads(bodies[0])["select"], [0])
        dialog["checks"][1][1].set_active(True)
        dialog["confirm"]()
        self.assertIn("another choice", dialog["error"].get_text())

    def test_sign_out_hides_the_calendar_and_retires_saved_work(self):
        window = self.window
        lease = window.mailbox_lease
        Attempts(self.store).save(lease, "/k/one", "event", {"request_key": "k1", "title": "x"})
        window.clear_account()
        self.assertFalse(window.calendar_button.get_visible())
        self.assertFalse(lease.current)
        self.assertEqual(window.cal["occurrences"], [])


if __name__ == "__main__":
    unittest.main()
