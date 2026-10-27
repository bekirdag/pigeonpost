"""Linux calendar code against a real loopback postbox (calendar mode of tests/e2e-messages/postbox.mjs).

    tests/e2e-messages/linux.sh /absolute/pigeonpost-postbox calendar

Alice organizes, Bob is invited. Everything goes through the app's calendar_core with lease-bound
postboxes; creating writes go through the response-loss proxy to prove exact explicit recovery.
"""

import datetime
import time
import urllib.error
import urllib.request

from live_support import Checks, StaticSession, attempts, fixture, loss, postbox

from pigeonpost.api import APIError
from pigeonpost.binding import Lease, Retired
from pigeonpost.calendar_core import (Attempts, CalendarClient, build_rrule, create_event_body, event_fields, patch_body, wire,
                                      rsvp_body, uncertain)
from pigeonpost.protected import MemoryProtectedStore


def main():
    setup = fixture()
    check = Checks("Linux calendar against the real postbox")
    alice_api, alice_session = postbox(setup, "alice", via_proxy=True)
    bob_api, _ = postbox(setup, "bob")
    alice_box, bob_box = setup["people"]["alice"]["address"], setup["people"]["bob"]["address"]
    lease = Lease(alice_session, alice_box)
    alice = CalendarClient(alice_api.bound(lease), alice_box)
    bob = CalendarClient(bob_api, bob_box)
    store = MemoryProtectedStore()
    saved = Attempts(store)

    check.ok("calendar" in alice_api.capabilities(), "calendar capability is on")
    check.ok(alice.calendars() == [], "GET /v1/calendars creates nothing")

    def create(kind, body):
        raw = saved.save(lease, alice_box, kind, body)
        try:
            result = alice.post_exact(kind, raw)
        except APIError as error:
            if uncertain(error):
                raise
            saved.remove(lease, alice_box, kind, body["request_key"])
            raise
        saved.remove(lease, alice_box, kind, body["request_key"])
        return result

    # A calendar whose response is lost after the postbox committed it.
    loss(setup, "after")
    body = {"request_key": "lnx_cal_fixture_1", "name": "Work", "color": "green"}
    try:
        create("calendar", body)
        check.ok(False, "lost calendar response surfaces")
    except APIError as error:
        check.ok(uncertain(error), "lost calendar response is uncertain, not a refusal")
    pending = saved.pending(lease, alice_box)
    check.ok(len(pending) == 1 and pending[0]["request_key"] == body["request_key"], "uncertain calendar write stays saved")
    loss(setup, "off")
    retried = alice.post_exact("calendar", pending[0]["wire"])
    saved.remove(lease, alice_box, "calendar", body["request_key"])
    calendars = alice.calendars()
    work = [c for c in calendars if c["name"] == "Work"]
    check.ok(len(work) == 1 and retried["calendar"]["id"] == work[0]["id"], "explicit exact retry returns the one calendar")
    check.ok(saved.pending(lease, alice_box) == [], "confirmed retry clears the saved write")
    try:
        saved.save(lease, alice_box, "calendar", {"request_key": "lnx_cal_fixture_2", "name": "A"})
        saved.save(lease, alice_box, "calendar", {"request_key": "lnx_cal_fixture_3", "name": "B"})
        check.ok(False, "second pending write of a kind refused")
    except APIError as error:
        check.ok(error.code == "attempt_conflict", "a second pending calendar write is refused, not replaced")
    saved.remove(lease, alice_box, "calendar", "lnx_cal_fixture_2")

    # A weekly meeting with Bob, lost after commit, retried exactly.
    tz = "Europe/Istanbul"
    first = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=2)).date()
    fields = event_fields({"title": "Design review", "calendar_id": work[0]["id"], "all_day": False,
                           "start": f"{first}T09:00", "end": f"{first}T10:00", "tzid": tz,
                           "rrule": build_rrule("WEEKLY", count=4), "attendees": [{"handle": bob_box}],
                           "meeting": True, "reminders": [10]})
    body = create_event_body(fields, "lnx_evt_fixture_1")
    loss(setup, "after")
    try:
        create("event", body)
    except APIError as error:
        check.ok(uncertain(error), "lost event response is uncertain")
    loss(setup, "off")
    pending = saved.pending(lease, alice_box)
    check.ok(len(pending) == 1 and pending[0]["kind"] == "event", "uncertain event stays saved with its exact bytes")
    result = alice.post_exact("event", pending[0]["wire"])
    saved.remove(lease, alice_box, "event", body["request_key"])
    event_id = result["event"]["id"]
    sent = [a for a in attempts(setup) if a.get("requestKey") == body["request_key"]]
    check.ok(len(sent) == 2 and len({a["sha"] for a in sent}) == 1, "the retry sent the same bytes and request key")
    start = int(time.time())
    occurrences = alice.occurrences(start, start + 40 * 86400)
    mine = [o for o in occurrences if o["event_id"] == event_id]
    check.ok(len(mine) == 4 and len({o["event_id"] for o in occurrences if o["title"] == "Design review"}) == 1,
             "exactly one series with four occurrences")
    check.ok(all(o["meeting"] and o["meeting"]["code"] for o in mine), "occurrences carry the meeting code")

    detail = alice.event(event_id, mine[1]["occurrence_id"])
    revision = detail["event"]["revision"]
    alice.update_event(event_id, patch_body({"title": "Design review (moved room)"}, revision, "this", mine[1]["occurrence_id"]))
    try:
        alice.update_event(event_id, patch_body({"title": "Stale"}, revision, "all"))
        check.ok(False, "stale revision refused")
    except APIError as error:
        check.ok(error.status == 409 and error.code == "revision_conflict", "a stale expected_revision is refused with 409")
    changed = alice.occurrences(start, start + 40 * 86400)
    check.ok(next(o for o in changed if o["occurrence_id"] == mine[1]["occurrence_id"])["title"] == "Design review (moved room)"
             and next(o for o in changed if o["occurrence_id"] == mine[0]["occurrence_id"])["title"] == "Design review",
             "scope this changed one occurrence, kept its id")
    revision = alice.event(event_id)["event"]["revision"]
    alice.update_event(event_id, patch_body({"title": "Design review"}, revision, "all"))
    revision = alice.event(event_id)["event"]["revision"]
    split = alice.update_event(event_id, patch_body({"title": "Design review v2"}, revision, "following", mine[2]["occurrence_id"]))
    check.ok(split.get("split_from", {}).get("event_id") == event_id and split["event"]["id"] != event_id,
             "scope following splits the series into a new event")

    invites = bob.invitations()
    check.ok(any(i["event_id"] == event_id for i in invites), "Bob sees the pending invitation")
    before = alice.event(event_id)["event"]["revision"]
    answer = bob.rsvp(event_id, rsvp_body("accepted"))
    check.ok(answer["rsvp"] == "accepted" and answer["revision"] is not None, "RSVP returns the attendee revision")
    check.ok(alice.event(event_id)["event"]["revision"] == before, "an RSVP does not change the organizer's revision")
    bob.set_reminders(event_id, [5, 30])
    check.ok(sorted(r["minutes_before"] for r in bob.event(event_id)["event"]["reminders"]) == [5, 30], "personal reminders saved")

    ics = bob.ics(event_id).decode()
    lines = [l for l in ics.replace("\r\n ", "").splitlines() if l.startswith("ATTENDEE")]
    check.ok("BEGIN:VCALENDAR" in ics and ("UID:" + event_id) in ics and lines and all(bob_box in l for l in lines),
             ".ics is the caller's own projection (only Bob's own attendee line)")

    sample = ("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//fixture//EN\r\nBEGIN:VEVENT\r\nUID:one@fixture.invalid\r\n"
              f"DTSTART:{first:%Y%m%d}T120000Z\r\nDTEND:{first:%Y%m%d}T130000Z\r\nSUMMARY:Imported lunch\r\n"
              "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\n"
              f"UID:two@fixture.invalid\r\nDTSTART;VALUE=DATE:{first:%Y%m%d}\r\nSUMMARY:Imported holiday\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
    preview = alice.import_preview(sample.encode(), work[0]["id"])
    check.ok(len(preview["events"]) == 2, "import preview lists both events")
    first_count = alice.import_confirm(preview["draft_id"], [0], work[0]["id"])
    second_count = alice.import_confirm(preview["draft_id"], [0, 0], work[0]["id"])
    imported = [o for o in alice.occurrences(start, start + 40 * 86400) if o["title"] == "Imported lunch"]
    check.ok(first_count == second_count == 1 and len(imported) == 1, "a repeated confirmation replays, never duplicates")
    try:
        alice.import_confirm(preview["draft_id"], [1], work[0]["id"])
        check.ok(False, "changed selection refused")
    except APIError as error:
        check.ok(error.code == "idempotency_conflict", "a changed selection is refused")

    feed = alice.set_feed(work[0]["id"], True, "busy")
    check.ok(feed["url"].startswith(setup["postbox"]) or feed["url"].startswith("http"), "feed link shown once")
    check.ok(alice.feed(work[0]["id"])["enabled"], "feed status is on (without the link)")
    local = setup["postbox"] + "/v1/calendar/feed/" + feed["url"].rsplit("/", 1)[1]
    with urllib.request.urlopen(local, timeout=10) as response:
        body_text = response.read().decode()
    check.ok("BEGIN:VCALENDAR" in body_text and "Design review" not in body_text, "busy feed hides titles")
    alice.set_feed(work[0]["id"], False)
    try:
        urllib.request.urlopen(local, timeout=10)
        check.ok(False, "revoked feed stops")
    except urllib.error.HTTPError as error:
        check.ok(error.code in (404, 410), "a revoked feed link stops working")

    settings = alice.settings()
    alice.save_settings({"timezone": tz, "week_start": 1, "previews": False})
    check.ok(alice.settings()["timezone"] == tz and settings is not None, "settings round trip")

    # Work of an earlier login never runs under a newer one.
    loss(setup, "before")
    late = create_event_body(event_fields({"title": "Never twice", "all_day": False, "start": f"{first}T15:00",
                                           "end": f"{first}T16:00", "tzid": tz}), "lnx_evt_fixture_2")
    try:
        create("event", late)
    except APIError as error:
        check.ok(uncertain(error), "unanswered create is uncertain")
    loss(setup, "off")
    alice_session.switch(setup["people"]["carol"]["token"])
    try:
        alice.post_exact("event", wire(late))
        check.ok(False, "old login cannot retry")
    except Retired:
        check.ok(True, "an old login's saved write cannot be retried after another account signs in")
    try:
        saved.pending(lease, alice_box)
        check.ok(False, "old login cannot read")
    except Retired:
        check.ok(True, "an old login's saved work is not opened for the newer login")
    newer = Lease(alice_session, alice_box)
    check.ok(saved.pending(newer, alice_box) == [], "the newer account sees none of the earlier account's work")
    check.done()


if __name__ == "__main__":
    main()
