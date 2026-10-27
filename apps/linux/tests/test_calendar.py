"""Calendar rules (calendar_core): no GUI, no network."""
import datetime
import json
import threading
import unittest

from pigeonpost.api import APIError
from pigeonpost.binding import Lease, Retired
from pigeonpost.calendar_core import (Attempts, CalendarClient, CalendarError, build_rrule, check_event, check_occurrences,
                                      create_event_body, day_segments, describe_rrule, display_code, due_reminders,
                                      event_fields, join_plan, meeting_url, parse_code, parse_rrule, patch_body, range_for,
                                      reminder_text, rsvp_body, uncertain, wall_status, window_for, wire, zone)
from pigeonpost.protected import MemoryProtectedStore

EVT = "evt_" + "a" * 26
EVT2 = "evt_" + "b" * 26
CAL = "cal_" + "c" * 26
OCC = "occ_" + "a" * 26 + "_20261022T090000"
OCC2 = "occ_" + "a" * 26 + "_20261029T090000"


class Session:
    def __init__(self):
        self.lock = threading.RLock()
        self.epoch, self.owner = 1, "owner-a"


def occurrence(**extra):
    row = {"occurrence_id": OCC, "event_id": EVT, "calendar_id": CAL, "all_day": False, "starts_at": 1_800_000_000,
           "ends_at": 1_800_003_600, "title": "Design review", "status": "confirmed", "role": "organizer",
           "my_rsvp": None, "invited": False, "meeting": {"code": "ABCDEFGHJK"}}
    row.update(extra)
    return row


class Fake:
    """A scoped() recorder standing in for a lease-bound Postbox."""

    def __init__(self, replies):
        self.replies, self.calls = list(replies), []

    def scoped(self, method, path, identity, data=None, **kwargs):
        self.calls.append((method, path, identity, kwargs.get("raw"), kwargs.get("content_type")))
        reply = self.replies.pop(0)
        if isinstance(reply, Exception):
            raise reply
        return reply


class Validation(unittest.TestCase):
    def test_occurrences_are_checked_strictly(self):
        rows = check_occurrences({"occurrences": [occurrence()]})
        self.assertEqual(rows[0]["meeting"]["code"], "ABCDEFGHJK")
        for bad in ({"event_id": "evt_x"}, {"occurrence_id": "occ_" + "b" * 26 + "_20261022T090000"},
                    {"ends_at": 1}, {"calendar_id": "cal_UPPER"}, {"starts_at": "1"}):
            with self.assertRaises(APIError, msg=bad):
                check_occurrences({"occurrences": [occurrence(**bad)]})
        with self.assertRaises(APIError):
            check_occurrences({"occurrences": [occurrence(), occurrence()]})
        with self.assertRaises(APIError):
            check_occurrences({"occurrences": [occurrence(occurrence_id="occ_" + "a" * 26 + f"_2026{i:04d}") for i in range(1001)]})
        long_title = check_occurrences({"occurrences": [occurrence(title="x" * 5000, meeting={"code": "bad"})]})[0]
        self.assertEqual(len(long_title["title"]), 300)
        self.assertIsNone(long_title["meeting"])

    def test_event_must_be_the_one_requested(self):
        body = {"event": {"id": EVT, "revision": 3, "attendees": [], "my": {"role": "organizer"}},
                "occurrence": {"occurrence_id": OCC}}
        self.assertEqual(check_event(body, EVT, OCC)["event"]["revision"], 3)
        with self.assertRaises(APIError):
            check_event(body, EVT2)
        with self.assertRaises(APIError):
            check_event(body, EVT, OCC2)
        with self.assertRaises(APIError):
            check_event({"event": {"id": EVT, "revision": 1, "attendees": [{"attendee_id": "x"}]}}, EVT)

    def test_meeting_codes(self):
        self.assertEqual(parse_code("abcde-fghjk"), "ABCDEFGHJK")
        self.assertIsNone(parse_code("ABCDEFGHJI"))  # I is not in the alphabet
        self.assertIsNone(parse_code("ABCDE_FGHJK"))
        self.assertEqual(display_code("ABCDEFGHJK"), "ABCDE-FGHJK")
        self.assertEqual(meeting_url("ABCDEFGHJK"), "https://inbox.pigeonpost.dev/m/ABCDEFGHJK")
        self.assertIsNone(meeting_url("../evil"))


class Time(unittest.TestCase):
    def test_gaps_and_overlaps_are_surfaced_not_shifted(self):
        self.assertEqual(wall_status(datetime.datetime(2026, 3, 8, 2, 30), "America/New_York"), "nonexistent")
        self.assertEqual(wall_status(datetime.datetime(2026, 11, 1, 1, 30), "America/New_York"), "ambiguous")
        self.assertEqual(wall_status(datetime.datetime(2026, 11, 1, 3, 30), "America/New_York"), "ok")
        self.assertEqual(wall_status(datetime.datetime(2026, 11, 1, 3, 30), "Mars/Base"), "zone_unknown")
        base = {"title": "T", "all_day": False, "tzid": "America/New_York"}
        with self.assertRaises(CalendarError) as gap:
            event_fields({**base, "start": "2026-03-08T02:30", "end": "2026-03-08T03:30"})
        self.assertEqual(gap.exception.code, "time_nonexistent")
        with self.assertRaises(CalendarError) as twice:
            event_fields({**base, "start": "2026-11-01T01:30", "end": "2026-11-01T03:30"})
        self.assertEqual(twice.exception.code, "time_ambiguous")
        fields = event_fields({**base, "start": "2026-11-01T01:30", "end": "2026-11-01T03:30", "fold": "later"})
        self.assertEqual((fields["start"], fields["fold"]), ("2026-11-01T01:30", "later"))
        with self.assertRaises(CalendarError):
            event_fields({**base, "start": "2026-11-02T10:00", "end": "2026-11-02T09:00"})
        with self.assertRaises(CalendarError):
            event_fields({**base, "tzid": "Not/AZone", "start": "2026-11-02T10:00", "end": "2026-11-02T11:00"})
        self.assertIsNone(zone("../../etc/passwd"))

    def test_all_day_end_is_exclusive_on_the_wire(self):
        fields = event_fields({"title": "Trip", "all_day": True, "start_date": "2026-10-22", "end_date": "2026-10-24"})
        self.assertEqual((fields["start_date"], fields["end_date"]), ("2026-10-22", "2026-10-25"))
        with self.assertRaises(CalendarError):
            event_fields({"title": "Trip", "all_day": True, "start_date": "2026-10-22", "end_date": "2026-10-24", "meeting": True})

    def test_views_and_windows(self):
        tz = zone("Europe/Istanbul")
        thursday = datetime.date(2026, 10, 22)
        self.assertEqual(range_for("week", thursday, 1), (datetime.date(2026, 10, 19), 7))
        self.assertEqual(range_for("week", thursday, 7), (datetime.date(2026, 10, 18), 7))
        first, days = range_for("month", thursday, 1)
        self.assertEqual((first, days), (datetime.date(2026, 9, 28), 42))
        start, end = window_for(first, days, tz)
        self.assertEqual(end - start, 42 * 86400)
        with self.assertRaises(CalendarError):
            window_for(first, 63, tz)

    def test_overlapping_events_share_the_day(self):
        tz = zone("UTC")
        day = datetime.date(2027, 1, 15)
        base = int(datetime.datetime(2027, 1, 15, tzinfo=datetime.timezone.utc).timestamp())
        rows = check_occurrences({"occurrences": [
            occurrence(occurrence_id="occ_" + "a" * 26 + "_20270115T090000", starts_at=base + 9 * 3600, ends_at=base + 10 * 3600),
            occurrence(occurrence_id="occ_" + "a" * 26 + "_20270115T093000", starts_at=base + 9 * 3600 + 1800, ends_at=base + 11 * 3600),
            occurrence(occurrence_id="occ_" + "a" * 26 + "_20270115T140000", starts_at=base + 14 * 3600, ends_at=base + 14 * 3600 + 900),
        ]})
        segments = day_segments(rows, day, tz)
        self.assertEqual([(top, span, col, cols) for _, top, span, col, cols in segments], [(18, 2, 0, 2), (19, 3, 1, 2), (28, 1, 0, 1)])


class Recurrence(unittest.TestCase):
    def test_rules(self):
        self.assertEqual(build_rrule("WEEKLY", byday=["TH", "MO"], count=6), "FREQ=WEEKLY;BYDAY=MO,TH;COUNT=6")
        self.assertEqual(build_rrule("DAILY", interval=2, until=datetime.date(2026, 12, 1)), "FREQ=DAILY;INTERVAL=2;UNTIL=20261201T235959Z")
        self.assertIsNone(build_rrule(None))
        for bad in ("RRULE:FREQ=DAILY", "FREQ=HOURLY", "FREQ=DAILY;COUNT=0", "FREQ=DAILY;BYHOUR=9", "FREQ=DAILY;COUNT=2;UNTIL=20261201",
                    "FREQ=WEEKLY;BYDAY=XX"):
            with self.assertRaises(ValueError, msg=bad):
                parse_rrule(bad)
        self.assertEqual(describe_rrule("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"), "Every weekday")
        self.assertEqual(describe_rrule("FREQ=WEEKLY;INTERVAL=2;BYDAY=TH;COUNT=6"), "Every 2 weeks on Thursday, 6 times")


class Requests(unittest.TestCase):
    def test_bodies(self):
        fields = event_fields({"title": " Review ", "all_day": False, "tzid": "Europe/Istanbul", "start": "2026-10-22T09:00",
                               "end": "2026-10-22T10:00", "attendees": [{"handle": "/alice"}, {"contact": "guest@example.invalid", "name": "G"}],
                               "meeting": True, "reminders": [10, 10, 30]})
        body = create_event_body(fields, "k1")
        self.assertEqual(body["request_key"], "k1")
        self.assertEqual(body["reminders"], [{"minutes_before": 10, "channel": "push"}, {"minutes_before": 30, "channel": "push"}])
        self.assertEqual(body["attendees"][1], {"contact": "guest@example.invalid", "name": "G"})
        with self.assertRaises(CalendarError):
            event_fields({"title": "x", "all_day": True, "start_date": "2026-10-22", "attendees": [{"handle": "alice bob"}]})
        patch = patch_body({"title": "New", "rrule": "FREQ=DAILY"}, 4, "this", OCC)
        self.assertEqual((patch["scope"], patch["occurrence_id"], patch["expected_revision"], "rrule" in patch), ("this", OCC, 4, False))
        with self.assertRaises(CalendarError):
            patch_body({}, 4, "following", None)
        with self.assertRaises(CalendarError):
            patch_body({}, None, "all")
        self.assertEqual(rsvp_body("accepted", "this", OCC, attendee_revision=7),
                         {"status": "accepted", "scope": "this", "occurrence_id": OCC, "expected_revision": 7})
        with self.assertRaises(CalendarError):
            rsvp_body("maybe")

    def test_client_routes_and_exact_bytes(self):
        fake = Fake([{"calendars": []}, {"event": {"id": EVT}}, b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", {"calendar": {"id": CAL}}])
        client = CalendarClient(fake, "/k/" + "a" * 26)
        client.calendars()
        client.update_event(EVT, patch_body({"title": "x"}, 1, "all"))
        client.ics(EVT)
        raw = wire({"request_key": "k", "name": "Work"})
        client.post_exact("calendar", raw)
        self.assertEqual(fake.calls[0][:2], ("GET", "/v1/calendars"))
        self.assertEqual(fake.calls[1][:2], ("PATCH", "/v1/calendar/events/" + EVT))
        self.assertEqual(fake.calls[2][:2], ("GET", "/v1/calendar/events/" + EVT + ".ics"))
        self.assertEqual(fake.calls[3][3], raw)
        with self.assertRaises(CalendarError):
            client.event("evt_../../v1/send")
        with self.assertRaises(CalendarError):
            client.occurrences(0, 63 * 86400)


class Protected(unittest.TestCase):
    def setUp(self):
        self.session = Session()
        self.lease = Lease(self.session, "/k/box")
        self.attempts = Attempts(MemoryProtectedStore())

    def test_one_saved_write_per_kind_bound_to_its_login(self):
        body = create_event_body({"title": "T"}, "k1")
        raw = self.attempts.save(self.lease, "/k/box", "event", body)
        self.assertEqual(self.attempts.save(self.lease, "/k/box", "event", body), raw)  # same bytes: no-op
        with self.assertRaises(CalendarError):
            self.attempts.save(self.lease, "/k/box", "event", create_event_body({"title": "U"}, "k2"))
        self.attempts.save(self.lease, "/k/box", "calendar", {"request_key": "k3", "name": "Work"})
        self.assertEqual(sorted(p["kind"] for p in self.attempts.pending(self.lease, "/k/box")), ["calendar", "event"])
        self.assertEqual(next(p for p in self.attempts.pending(self.lease, "/k/box") if p["kind"] == "event")["wire"], raw)
        self.assertFalse(self.attempts.remove(self.lease, "/k/box", "event", "other"))  # conditional on the key
        self.assertTrue(self.attempts.remove(self.lease, "/k/box", "event", "k1"))
        self.assertEqual(self.attempts.pending(self.lease, "/k/other"), [])  # per mailbox
        self.session.epoch += 1
        self.session.owner = "owner-b"
        with self.assertRaises(Retired):
            self.attempts.pending(self.lease, "/k/box")
        self.assertEqual(self.attempts.pending(Lease(self.session, "/k/box"), "/k/box"), [])

    def test_tampered_entries_are_refused(self):
        self.attempts.save(self.lease, "/k/box", "event", create_event_body({"title": "T"}, "k1"))
        store = self.attempts.store
        key = next(iter(store.rows))
        record = json.loads(store.rows[key])
        record["calendar"]["attempts"][0]["request_key"] = "k9"
        store.rows[key] = json.dumps(record)
        with self.assertRaises(Exception):
            self.attempts.pending(self.lease, "/k/box")

    def test_uncertain_outcomes(self):
        self.assertTrue(uncertain(APIError()))
        self.assertTrue(uncertain(APIError(502, "bad_gateway")))
        self.assertFalse(uncertain(APIError(409, "revision_conflict")))
        self.assertFalse(uncertain(APIError(403, "human_required")))


class Joining(unittest.TestCase):
    def detail(self, **event):
        base = {"event": {"id": EVT, "revision": 1, "status": "confirmed", "meeting": {"code": "ABCDEFGHJK"},
                          "my": {"role": "attendee", "attendee_id": "att_" + "d" * 26, "rsvp": "accepted"}, "attendees": []},
                "occurrence": {"occurrence_id": OCC, "starts_at": 1000, "ends_at": 4600}}
        base["event"].update(event)
        return check_event(base, EVT, OCC)

    def test_plan(self):
        tz = zone("UTC")
        window = {"occurrence_id": OCC, "starts_at": 1000, "ends_at": 4600, "opens_at": 400, "closes_at": 6400}
        plan = join_plan(self.detail(), {"state": "open", "occurrences": [window]}, 500, tz)
        self.assertTrue(plan["ok"])
        self.assertEqual(plan["url"], "https://inbox.pigeonpost.dev/m/ABCDEFGHJK")
        self.assertFalse(join_plan(self.detail(), {"state": "open", "occurrences": [window]}, 100, tz)["ok"])
        declined = self.detail(my={"role": "attendee", "attendee_id": "att_" + "d" * 26, "rsvp": "declined"})
        self.assertFalse(join_plan(declined, None, 500, tz)["ok"])
        self.assertFalse(join_plan(self.detail(status="cancelled"), None, 500, tz)["ok"])
        other = dict(window, occurrence_id=OCC2)
        both = join_plan(self.detail(), {"state": "open", "occurrences": [window, other]}, 500, tz)
        self.assertTrue(both["ok"])
        self.assertIn("choose the one at", both["message"])
        self.assertTrue(join_plan(self.detail(), None, 500, tz)["ok"])  # calls service unreachable: the page decides


class Reminders(unittest.TestCase):
    def test_due_and_text(self):
        rows = check_occurrences({"occurrences": [occurrence(starts_at=10_000, ends_at=13_600),
                                                  occurrence(occurrence_id=OCC2, starts_at=10_000, ends_at=13_600, my_rsvp="declined")]})
        due = due_reminders(rows, {EVT: [10, 60]}, set(), now=10_000 - 600)
        self.assertEqual([(k, m) for k, _, m, _ in due], [(OCC + ":10", 10)])
        self.assertEqual(due_reminders(rows, {EVT: [10]}, {OCC + ":10"}, now=9_400), [])
        self.assertEqual(due_reminders(rows, {EVT: [10]}, set(), now=9_400 + 301), [])  # too late: skipped
        self.assertEqual(reminder_text(rows[0], 10, False), ("Event starting in 10 minutes", "Open Pigeonpost to see the details."))
        self.assertEqual(reminder_text(rows[0], 10, True)[1], "Design review")


if __name__ == "__main__":
    unittest.main()
