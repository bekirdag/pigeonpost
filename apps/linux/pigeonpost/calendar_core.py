"""Calendar and scheduled meetings (SDS §14B, docs/calendar.md): the rules, no GUI.

* Server data is validated before use: identifiers in their exact shapes (§1), bounded strings,
  windows of at most 62 days, at most 1,000 occurrences, no duplicates. Anything else is
  refused as an unexpected response instead of being shown half-trusted.
* Wall times are checked in their IANA zone before sending: a time that does not exist (a
  spring-forward gap) or happens twice (fall-back) is surfaced to the person, never shifted.
* Creating routes (`POST /v1/calendars`, `POST /v1/calendar/events`) carry a `request_key`. Their
  exact request bytes and key are saved in the protected store (protected.py) before the first
  POST. An uncertain outcome (no answer, 5xx) stays saved for an explicit Retry with the same
  bytes and key, which the postbox answers with the original result, or an explicit Discard. A
  definite refusal clears it. Saved work belongs to the login and mailbox that made it.
* Calendar data is not end-to-end encrypted (§0): nothing here claims it is.
* Joining a scheduled meeting opens the web meeting page in the system browser (WebKitGTK has no
  WebRTC). The app first checks the exact occurrence against a fresh copy of the event and the
  calls service's scheduled status; the page then asks which occurrence when windows overlap.
"""

import base64
import datetime
import hashlib
import json
import re
import secrets
import time
import urllib.parse

from .api import APIError
from .binding import Retired

try:
    from zoneinfo import ZoneInfo, ZoneInfoNotFoundError, available_timezones
except ImportError:  # pragma: no cover - Python < 3.9
    ZoneInfo = None
    ZoneInfoNotFoundError = Exception

    def available_timezones():
        return set()

CALENDAR = "calendar"
CALENDAR_EMAIL = "calendar_email"  # contact-address invitations (flag name is the server's)
CALLS = "https://inbox.pigeonpost.dev/calls"
MEETING_BASE = "https://inbox.pigeonpost.dev/m/"

CALENDAR_ID = re.compile(r"cal_[a-z2-7]{26}")
EVENT_ID = re.compile(r"evt_[a-z2-7]{26}")
ATTENDEE_ID = re.compile(r"att_[a-z2-7]{26}")
OCCURRENCE_ID = re.compile(r"occ_[a-z2-7]{26}_(\d{8}T\d{6}|\d{8})")
CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789"
CODE = re.compile(r"[ABCDEFGHJKMNPQRSTUVWXYZ23456789]{10}")
REQUEST_KEY = re.compile(r"[A-Za-z0-9_-]{1,64}")
WALL = re.compile(r"(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})")
DATE = re.compile(r"(\d{4})-(\d{2})-(\d{2})")
COLORS = ("blue", "green", "orange", "red", "purple", "teal", "gray")
RSVP = ("accepted", "tentative", "declined")
SCOPES = ("this", "following", "all")
MAX_WINDOW = 62 * 86400
MAX_OCCURRENCES = 1000
MAX_IMPORT = 512 * 1024
RETRY_WINDOW = 86400
REMINDER_LATE = 300            # a reminder more than five minutes late is skipped, not shown
WEEKDAYS = ("MO", "TU", "WE", "TH", "FR", "SA", "SU")
WEEKDAY_NAMES = ("Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday")
FREQS = ("DAILY", "WEEKLY", "MONTHLY", "YEARLY")
RRULE_PARTS = ("FREQ", "INTERVAL", "COUNT", "UNTIL", "BYDAY", "BYMONTHDAY", "BYMONTH", "BYSETPOS", "WKST")


class CalendarError(APIError):
    """A local refusal shown to the person (never sent)."""

    def __init__(self, text, code="calendar_invalid"):
        super().__init__(0, code, text)


def bad_response():
    return APIError(0, "invalid_response")


# ---- identifiers and server data ----------------------------------------------------------------

def valid(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def text(value, limit=500):
    """A bounded plain string for display (control characters other than newlines removed)."""
    if not isinstance(value, str):
        return ""
    return "".join(c for c in value[:limit] if c == "\n" or ord(c) >= 32)


def number(value):
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def parse_code(value):
    """A meeting code from `XXXXX-XXXXX` or ten characters, upper-cased; None when malformed."""
    if not isinstance(value, str) or len(value) not in (10, 11) or (len(value) == 11 and value[5] != "-"):
        return None
    code = (value[:5] + value[6:] if len(value) == 11 else value).upper()
    return code if CODE.fullmatch(code) else None


def display_code(code):
    return code[:5] + "-" + code[5:] if isinstance(code, str) and len(code) == 10 else ""


def meeting_url(code):
    code = parse_code(code)
    return MEETING_BASE + code if code else None


def check_calendars(result):
    rows = result.get("calendars") if isinstance(result, dict) else None
    if not isinstance(rows, list) or len(rows) > 20:
        raise bad_response()
    seen, out = set(), []
    for row in rows:
        if not isinstance(row, dict) or not valid(CALENDAR_ID, row.get("id")) or row["id"] in seen:
            raise bad_response()
        seen.add(row["id"])
        out.append({"id": row["id"], "name": text(row.get("name"), 80) or "Calendar",
                    "color": row.get("color") if row.get("color") in COLORS else "blue",
                    "is_default": row.get("is_default") is True, "revision": number(row.get("revision"))})
    return out


def check_occurrence(row):
    if not isinstance(row, dict) or not valid(EVENT_ID, row.get("event_id")) or not valid(OCCURRENCE_ID, row.get("occurrence_id")):
        raise bad_response()
    if row["occurrence_id"][4:30] != row["event_id"][4:]:
        raise bad_response()  # an occurrence always names its own event (§1)
    if row.get("calendar_id") is not None and not valid(CALENDAR_ID, row.get("calendar_id")):
        raise bad_response()
    all_day = row.get("all_day") is True
    starts, ends = number(row.get("starts_at")), number(row.get("ends_at"))
    if not all_day and (starts is None or ends is None or ends < starts):
        raise bad_response()
    if all_day and not (valid(DATE, row.get("start_date")) and valid(DATE, row.get("end_date"))):
        raise bad_response()
    meeting = row.get("meeting") if isinstance(row.get("meeting"), dict) else None
    if meeting is not None:
        code = parse_code(meeting.get("code"))
        meeting = {"code": code, "opens_at": number(meeting.get("opens_at")), "closes_at": number(meeting.get("closes_at"))} if code else None
    organizer = row.get("organizer") if isinstance(row.get("organizer"), dict) else {}
    return {
        "occurrence_id": row["occurrence_id"], "event_id": row["event_id"],
        "recurrence_key": text(row.get("recurrence_key"), 20), "calendar_id": row.get("calendar_id"),
        "color": row.get("color") if row.get("color") in COLORS else "blue",
        "all_day": all_day, "starts_at": starts, "ends_at": ends, "tzid": text(row.get("tzid"), 64),
        "start_date": row.get("start_date") if all_day else None, "end_date": row.get("end_date") if all_day else None,
        "title": text(row.get("title"), 300) or "(No title)", "location": text(row.get("location"), 300),
        "status": row.get("status") if row.get("status") in ("confirmed", "cancelled", "tentative") else "confirmed",
        "recurring": row.get("recurring") is True, "overridden": row.get("overridden") is True,
        "organizer": {"address": text(organizer.get("address"), 512), "handle": text(organizer.get("handle"), 512)},
        "role": row.get("role") if row.get("role") in ("organizer", "attendee", "cohost") else "attendee",
        "my_rsvp": row.get("my_rsvp") if row.get("my_rsvp") in (*RSVP, "needs_action") else None,
        "invited": row.get("invited") is True, "meeting": meeting,
        "transparency": "transparent" if row.get("transparency") == "transparent" else "opaque",
        "revision": number(row.get("revision")),
    }


def check_occurrences(result):
    rows = result.get("occurrences") if isinstance(result, dict) else None
    if not isinstance(rows, list) or len(rows) > MAX_OCCURRENCES:
        raise bad_response()
    out = [check_occurrence(row) for row in rows]
    if len({o["occurrence_id"] for o in out}) != len(out):
        raise bad_response()
    return out


def check_event(result, event_id, occurrence_id=None):
    """`GET /v1/calendar/events/{id}`: the event asked for (and occurrence), or a bad response."""
    event = result.get("event") if isinstance(result, dict) else None
    if not isinstance(event, dict) or event.get("id") != event_id or not valid(EVENT_ID, event_id):
        raise bad_response()
    occurrence = result.get("occurrence") if isinstance(result.get("occurrence"), dict) else None
    if occurrence_id is not None and (occurrence is None or occurrence.get("occurrence_id") != occurrence_id):
        raise bad_response()
    if number(event.get("revision")) is None:
        raise bad_response()
    attendees = []
    for a in event.get("attendees") or []:
        if not isinstance(a, dict) or not valid(ATTENDEE_ID, a.get("attendee_id")):
            raise bad_response()
        attendees.append({"attendee_id": a["attendee_id"], "kind": "external" if a.get("kind") == "external" else "handle",
                          "handle": text(a.get("handle"), 512), "address": text(a.get("address"), 512),
                          "name": text(a.get("name"), 120), "role": text(a.get("role"), 20), "optional": a.get("optional") is True,
                          "rsvp": a.get("rsvp") if a.get("rsvp") in (*RSVP, "needs_action") else "needs_action",
                          "delivery": text(a.get("delivery"), 20), "proof": text(a.get("proof"), 20)})
    if len(attendees) > 600:
        raise bad_response()
    meeting = event.get("meeting") if isinstance(event.get("meeting"), dict) else None
    if meeting is not None:
        code = parse_code(meeting.get("code"))
        meeting = {**{k: meeting.get(k) for k in ("waiting", "admit", "allow_knock", "join_before_minutes", "transport", "participant_limit")},
                   "code": code} if code else None
    my = event.get("my") if isinstance(event.get("my"), dict) else {}
    reminders = []
    for r in event.get("reminders") or []:
        if isinstance(r, dict) and number(r.get("minutes_before")) is not None and 0 <= r["minutes_before"] <= 40320:
            reminders.append({"minutes_before": r["minutes_before"], "channel": r.get("channel") if r.get("channel") in ("push", "external") else "push"})
    organizer = event.get("organizer") if isinstance(event.get("organizer"), dict) else {}
    checked = {
        "id": event_id, "calendar_id": event.get("calendar_id") if valid(CALENDAR_ID, event.get("calendar_id")) else None,
        "revision": event["revision"], "sequence": number(event.get("sequence")),
        "status": "cancelled" if event.get("status") == "cancelled" else "confirmed",
        "organizer": {"address": text(organizer.get("address"), 512), "handle": text(organizer.get("handle"), 512)},
        "title": text(event.get("title"), 300), "description": text(event.get("description"), 8000),
        "location": text(event.get("location"), 300), "all_day": event.get("all_day") is True,
        "start": event.get("start") if valid(WALL, event.get("start")) else None,
        "end": event.get("end") if valid(WALL, event.get("end")) else None,
        "tzid": text(event.get("tzid"), 64) or None,
        "starts_at": number(event.get("starts_at")), "ends_at": number(event.get("ends_at")),
        "start_date": event.get("start_date") if valid(DATE, event.get("start_date")) else None,
        "end_date": event.get("end_date") if valid(DATE, event.get("end_date")) else None,
        "rrule": text(event.get("rrule"), 500) or None,
        "exdates": [x for x in event.get("exdates") or [] if isinstance(x, str) and len(x) <= 20],
        "transparency": "transparent" if event.get("transparency") == "transparent" else "opaque",
        "privacy": text(event.get("privacy"), 20) or "default",
        "meeting": meeting, "attendees": attendees,
        "counts": {k: number((event.get("counts") or {}).get(k)) or 0 for k in ("accepted", "tentative", "declined", "needs_action")}
        if isinstance(event.get("counts"), dict) else {},
        "my": {"role": my.get("role") if my.get("role") in ("organizer", "attendee", "cohost") else None,
               "attendee_id": my.get("attendee_id") if valid(ATTENDEE_ID, my.get("attendee_id")) else None,
               "rsvp": my.get("rsvp") if my.get("rsvp") in (*RSVP, "needs_action") else None,
               "calendar_id": my.get("calendar_id") if valid(CALENDAR_ID, my.get("calendar_id")) else None},
        "reminders": reminders[:5], "can_edit": event.get("can_edit") is True,
    }
    occ = None
    if occurrence is not None:
        if not valid(OCCURRENCE_ID, occurrence.get("occurrence_id")) or occurrence["occurrence_id"][4:30] != event_id[4:]:
            raise bad_response()
        occ = {"occurrence_id": occurrence["occurrence_id"], "recurrence_key": text(occurrence.get("recurrence_key"), 20),
               "starts_at": number(occurrence.get("starts_at")), "ends_at": number(occurrence.get("ends_at")),
               "status": "cancelled" if occurrence.get("status") == "cancelled" else "confirmed",
               "my_rsvp": occurrence.get("my_rsvp") if occurrence.get("my_rsvp") in (*RSVP, "needs_action") else None}
    return {"event": checked, "occurrence": occ}


def check_invitations(result):
    rows = result.get("invitations") if isinstance(result, dict) else None
    if not isinstance(rows, list) or len(rows) > MAX_OCCURRENCES:
        raise bad_response()
    out = []
    for row in rows:
        if not isinstance(row, dict) or not valid(EVENT_ID, row.get("event_id")):
            raise bad_response()
        occ = row.get("occurrence_id")
        if occ is not None and not valid(OCCURRENCE_ID, occ):
            raise bad_response()
        organizer = row.get("organizer") if isinstance(row.get("organizer"), dict) else {}
        out.append({"event_id": row["event_id"], "occurrence_id": occ, "title": text(row.get("title"), 300) or "(No title)",
                    "starts_at": number(row.get("starts_at")), "ends_at": number(row.get("ends_at")),
                    "all_day": row.get("all_day") is True, "start_date": row.get("start_date") if valid(DATE, row.get("start_date")) else None,
                    "organizer": text(organizer.get("handle") or organizer.get("address"), 512),
                    "recurring": row.get("recurring") is True, "meeting": row.get("meeting") is True})
    return out


# ---- time ---------------------------------------------------------------------------------------

def zone(name):
    """A ZoneInfo for a valid IANA name, else None."""
    if not isinstance(name, str) or not name or len(name) > 64 or ".." in name or name.startswith("/") or ZoneInfo is None:
        return None
    try:
        return ZoneInfo(name)
    except (ZoneInfoNotFoundError, ValueError, OSError):
        return None


def local_zone_name():
    """The system zone's IANA name when it can be found, else UTC."""
    import os
    name = os.environ.get("TZ", "").lstrip(":")
    if zone(name):
        return name
    try:
        target = os.path.realpath("/etc/localtime")
        if "/zoneinfo/" in target:
            candidate = target.split("/zoneinfo/", 1)[1]
            if zone(candidate):
                return candidate
    except OSError:
        pass
    try:
        with open("/etc/timezone") as handle:
            candidate = handle.read().strip()
            if zone(candidate):
                return candidate
    except OSError:
        pass
    return "UTC"


def parse_wall(value):
    """`YYYY-MM-DDTHH:MM` → naive datetime, or None."""
    m = WALL.fullmatch(value or "")
    if not m:
        return None
    try:
        return datetime.datetime(*(int(x) for x in m.groups()))
    except ValueError:
        return None


def parse_date(value):
    m = DATE.fullmatch(value or "")
    if not m:
        return None
    try:
        return datetime.date(*(int(x) for x in m.groups()))
    except ValueError:
        return None


def wall_status(wall, tzname):
    """'ok', 'nonexistent' (inside a spring-forward gap) or 'ambiguous' (happens twice)."""
    tz = zone(tzname)
    if tz is None:
        return "zone_unknown"
    early, late = wall.replace(tzinfo=tz, fold=0), wall.replace(tzinfo=tz, fold=1)
    round_trip = early.astimezone(datetime.timezone.utc).astimezone(tz).replace(tzinfo=None)
    if round_trip != wall:
        return "nonexistent"
    if early.utcoffset() != late.utcoffset():
        return "ambiguous"
    return "ok"


def to_unix(wall, tzname, fold=0):
    tz = zone(tzname)
    return int(wall.replace(tzinfo=tz, fold=fold).timestamp())


def in_zone(unix, tz):
    return datetime.datetime.fromtimestamp(unix, tz)


def range_for(view, anchor, week_start=1):
    """(first day, number of days) shown by a view around `anchor` (a date)."""
    if view == "day":
        return anchor, 1
    if view == "week":
        offset = (anchor.isoweekday() - week_start) % 7
        return anchor - datetime.timedelta(days=offset), 7
    if view == "month":
        first = anchor.replace(day=1)
        offset = (first.isoweekday() - week_start) % 7
        return first - datetime.timedelta(days=offset), 42
    return anchor, 31  # agenda


def window_for(first, days, tz):
    """Unix (from, to) of whole local days; refused above the 62-day server window."""
    start = datetime.datetime.combine(first, datetime.time(0), tz)
    end = datetime.datetime.combine(first + datetime.timedelta(days=days), datetime.time(0), tz)
    window = (int(start.timestamp()), int(end.timestamp()))
    if window[1] <= window[0] or window[1] - window[0] > MAX_WINDOW:
        raise CalendarError("Choose a calendar window of at most 62 days.")
    return window


def shift(view, anchor, step):
    if view == "day":
        return anchor + datetime.timedelta(days=step)
    if view == "week":
        return anchor + datetime.timedelta(days=7 * step)
    if view == "month":
        month = anchor.month - 1 + step
        year = anchor.year + month // 12
        return datetime.date(year, month % 12 + 1, 1)
    return anchor + datetime.timedelta(days=31 * step)


def occurrence_days(o, tz):
    """The local dates an occurrence covers (all-day end dates are exclusive)."""
    if o["all_day"]:
        start, end = parse_date(o["start_date"]), parse_date(o["end_date"])
        if not start or not end or end <= start:
            return [start] if start else []
        return [start + datetime.timedelta(days=i) for i in range(min(62, (end - start).days))]
    start, end = in_zone(o["starts_at"], tz), in_zone(max(o["ends_at"] - 1, o["starts_at"]), tz)
    return [start.date() + datetime.timedelta(days=i) for i in range(min(62, (end.date() - start.date()).days + 1))]


def day_segments(occurrences, day, tz, slot_minutes=30):
    """Timed occurrences on one local day as grid rows with side-by-side columns for overlaps:
    [(occurrence, first slot, slot count, column, columns)]."""
    start_of_day = datetime.datetime.combine(day, datetime.time(0), tz)
    end_of_day = datetime.datetime.combine(day + datetime.timedelta(days=1), datetime.time(0), tz)
    slots = 24 * 60 // slot_minutes
    items = []
    for o in occurrences:
        if o["all_day"] or o["starts_at"] >= end_of_day.timestamp() or o["ends_at"] <= start_of_day.timestamp():
            continue
        top = max(0, int((o["starts_at"] - start_of_day.timestamp()) // (slot_minutes * 60)))
        bottom = min(slots, max(top + 1, -int(-(o["ends_at"] - start_of_day.timestamp()) // (slot_minutes * 60))))
        items.append([o, top, bottom])
    items.sort(key=lambda x: (x[1], -x[2], x[0]["occurrence_id"]))
    out, cluster, columns, cluster_end = [], [], [], -1
    for o, top, bottom in items:
        if cluster and top >= cluster_end:
            width = len(columns)
            out.extend((c[0], c[1], c[2] - c[1], c[3], width) for c in cluster)
            cluster, columns = [], []
        col = next((i for i, end in enumerate(columns) if end <= top), len(columns))
        if col == len(columns):
            columns.append(bottom)
        else:
            columns[col] = bottom
        cluster.append((o, top, bottom, col))
        cluster_end = max(cluster_end, bottom) if len(cluster) > 1 else bottom
    if cluster:
        width = len(columns)
        out.extend((c[0], c[1], c[2] - c[1], c[3], width) for c in cluster)
    return out


def when_text(o, tz):
    if o.get("all_day"):
        start, end = parse_date(o.get("start_date")), parse_date(o.get("end_date"))
        if start and end and (end - start).days > 1:
            return f"{start:%a %d %b} – {(end - datetime.timedelta(days=1)):%a %d %b} · all day"
        return f"{start:%a %d %b} · all day" if start else "All day"
    if o.get("starts_at") is None:
        return ""
    start, end = in_zone(o["starts_at"], tz), in_zone(o["ends_at"] or o["starts_at"], tz)
    if start.date() == end.date():
        return f"{start:%a %d %b} · {start:%H:%M}–{end:%H:%M}"
    return f"{start:%a %d %b %H:%M} – {end:%a %d %b %H:%M}"


# ---- recurrence ---------------------------------------------------------------------------------

def parse_rrule(rule):
    """The rule's parts as a dict; ValueError for anything the server would refuse."""
    if not rule:
        return {}
    if not isinstance(rule, str) or len(rule) > 500 or rule.upper().startswith("RRULE:"):
        raise ValueError("recurrence")
    parts = {}
    for piece in rule.split(";"):
        name, _, value = piece.partition("=")
        if name not in RRULE_PARTS or not value or name in parts:
            raise ValueError("recurrence")
        parts[name] = value
    if parts.get("FREQ") not in FREQS:
        raise ValueError("recurrence")
    for name in ("INTERVAL", "COUNT"):
        if name in parts and (not parts[name].isdigit() or not 1 <= int(parts[name]) <= 1000):
            raise ValueError("recurrence")
    if "COUNT" in parts and "UNTIL" in parts:
        raise ValueError("recurrence")
    if "BYDAY" in parts and not all(re.fullmatch(r"[+-]?\d{0,2}(MO|TU|WE|TH|FR|SA|SU)", d) for d in parts["BYDAY"].split(",")):
        raise ValueError("recurrence")
    if "UNTIL" in parts and not re.fullmatch(r"\d{8}(T\d{6}Z?)?", parts["UNTIL"]):
        raise ValueError("recurrence")
    return parts


def build_rrule(freq, interval=1, count=None, until=None, byday=None):
    """A rule body from the editor's choices. `until` is a date (inclusive, end of that day)."""
    if not freq:
        return None
    if freq not in FREQS:
        raise CalendarError("Choose how often it repeats.")
    parts = ["FREQ=" + freq]
    if interval and int(interval) != 1:
        if not 1 <= int(interval) <= 1000:
            raise CalendarError("Repeat every 1–1000 periods.")
        parts.append(f"INTERVAL={int(interval)}")
    if byday:
        days = [d for d in WEEKDAYS if d in byday]
        if days:
            parts.append("BYDAY=" + ",".join(days))
    if count:
        if not 1 <= int(count) <= 1000:
            raise CalendarError("Repeat 1–1000 times.")
        parts.append(f"COUNT={int(count)}")
    elif until:
        parts.append("UNTIL=" + until.strftime("%Y%m%d") + "T235959Z")
    rule = ";".join(parts)
    parse_rrule(rule)
    return rule


def describe_rrule(rule):
    try:
        parts = parse_rrule(rule)
    except ValueError:
        return "Repeats (custom)"
    if not parts:
        return "Does not repeat"
    n = int(parts.get("INTERVAL", "1"))
    unit = {"DAILY": "day", "WEEKLY": "week", "MONTHLY": "month", "YEARLY": "year"}[parts["FREQ"]]
    words = {"DAILY": "Daily", "WEEKLY": "Weekly", "MONTHLY": "Monthly", "YEARLY": "Yearly"}[parts["FREQ"]]
    out = words if n == 1 else f"Every {n} {unit}s"
    if parts.get("BYDAY"):
        days = parts["BYDAY"].split(",")
        if days == ["MO", "TU", "WE", "TH", "FR"] and n == 1 and parts["FREQ"] == "WEEKLY":
            out = "Every weekday"
        else:
            out += " on " + ", ".join(WEEKDAY_NAMES[WEEKDAYS.index(d[-2:])] for d in days)
    if parts.get("COUNT"):
        out += f", {parts['COUNT']} times"
    elif parts.get("UNTIL"):
        u = parts["UNTIL"]
        out += f", until {u[6:8]}.{u[4:6]}.{u[:4]}"
    return out


# ---- requests -----------------------------------------------------------------------------------

def new_request_key():
    return "lnx_" + secrets.token_urlsafe(24).replace("-", "_")[:32]


def event_fields(form):
    """Validated event fields from the editor. Raises CalendarError with a person-readable reason.

    `form`: title, description, location, all_day, start/end ("YYYY-MM-DDTHH:MM") with tzid, or
    start_date/end_date (end inclusive in the form, exclusive on the wire), rrule, fold,
    attendees [{handle}|{contact, name}], meeting (bool), reminders [minutes], calendar_id."""
    title = (form.get("title") or "").strip()
    if not title or len(title) > 300:
        raise CalendarError("Give the event a title of up to 300 characters.")
    out = {"title": title}
    for name, limit in (("description", 8000), ("location", 300)):
        value = (form.get(name) or "").strip()
        if len(value) > limit:
            raise CalendarError(f"The {name} is too long.")
        if value or form.get("editing"):
            out[name] = value
    if form.get("calendar_id"):
        if not valid(CALENDAR_ID, form["calendar_id"]):
            raise CalendarError("Choose a calendar.")
        out["calendar_id"] = form["calendar_id"]
    if form.get("all_day"):
        start, end = parse_date(form.get("start_date")), parse_date(form.get("end_date") or form.get("start_date"))
        if not start or not end:
            raise CalendarError("Enter dates as YYYY-MM-DD.")
        if end < start:
            raise CalendarError("The event must end on or after the day it starts.")
        out.update(all_day=True, start_date=start.isoformat(), end_date=(end + datetime.timedelta(days=1)).isoformat())
    else:
        tzid = form.get("tzid") or ""
        if zone(tzid) is None:
            raise CalendarError("Choose a known time zone, such as Europe/Istanbul.", "zone_unknown")
        start, end = parse_wall(form.get("start")), parse_wall(form.get("end"))
        if not start or not end:
            raise CalendarError("Enter times as YYYY-MM-DD and HH:MM.")
        fold = form.get("fold")
        for wall in (start, end):
            status = wall_status(wall, tzid)
            if status == "nonexistent":
                raise CalendarError(f"{wall:%d %b %H:%M} does not exist in {tzid}: the clocks skip it. Choose another time.",
                                    "time_nonexistent")
            if status == "ambiguous" and fold not in ("earlier", "later"):
                raise CalendarError(f"{wall:%d %b %H:%M} happens twice in {tzid}: the clocks go back. Choose the earlier or later one.",
                                    "time_ambiguous")
        index = 1 if fold == "later" else 0
        if to_unix(end, tzid, index) <= to_unix(start, tzid, index):
            raise CalendarError("The event must end after it starts.")
        out.update(all_day=False, start=start.strftime("%Y-%m-%dT%H:%M"), end=end.strftime("%Y-%m-%dT%H:%M"), tzid=tzid)
        if fold in ("earlier", "later"):
            out["fold"] = fold
    rule = form.get("rrule")
    if rule:
        try:
            parse_rrule(rule)
        except ValueError:
            raise CalendarError("This repeat rule is not supported.", "recurrence_invalid") from None
        out["rrule"] = rule
    elif form.get("editing"):
        out["rrule"] = None
    attendees = []
    for item in form.get("attendees") or []:
        if item.get("handle"):
            handle = item["handle"].strip()
            if not handle.startswith("/") or len(handle) > 512 or any(c.isspace() for c in handle):
                raise CalendarError(f"{handle} is not a Pigeonpost address.")
            attendees.append({"handle": handle})
        elif item.get("contact"):
            contact = item["contact"].strip()
            if "@" not in contact or len(contact) > 254 or any(c.isspace() for c in contact):
                raise CalendarError(f"{contact} is not a contact address.")
            entry = {"contact": contact}
            if item.get("name"):
                entry["name"] = item["name"].strip()[:120]
            attendees.append(entry)
    if len([a for a in attendees if "handle" in a]) > 500 or len([a for a in attendees if "contact" in a]) > 50:
        raise CalendarError("Too many guests for one event.")
    if attendees and not form.get("editing"):
        out["attendees"] = attendees
    if form.get("meeting") is not None and not form.get("editing"):
        if form["meeting"] and out.get("all_day"):
            raise CalendarError("An all-day event cannot have a scheduled call.")
        out["meeting"] = {"enabled": bool(form["meeting"])}
    if form.get("reminders") is not None and not form.get("editing"):
        out["reminders"] = reminder_list(form["reminders"])
    return out


def reminder_list(minutes):
    rows = []
    for value in minutes:
        if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= 40320:
            raise CalendarError("Reminders are 0 minutes to 4 weeks before.")
        if value not in [r["minutes_before"] for r in rows]:
            rows.append({"minutes_before": value, "channel": "push"})
    if len(rows) > 5:
        raise CalendarError("Choose at most five reminders.")
    return rows


def create_event_body(fields, request_key=None):
    body = {"request_key": request_key or new_request_key(), **fields, "locale": "en"}
    body.setdefault("transparency", "opaque")
    return body


def patch_body(fields, expected_revision, scope, occurrence_id=None):
    if scope not in SCOPES:
        raise CalendarError("Choose which occurrences to change.")
    if scope != "all" and not valid(OCCURRENCE_ID, occurrence_id):
        raise CalendarError("Choose the occurrence to change.")
    if number(expected_revision) is None:
        raise CalendarError("Reload the event before changing it.")
    body = {"expected_revision": expected_revision, "scope": scope, **fields}
    if scope != "all":
        body["occurrence_id"] = occurrence_id
    if scope == "this":
        body.pop("rrule", None)  # one occurrence cannot change the series' rule
    return body


def rsvp_body(status, scope="all", occurrence_id=None, note=None, attendee_revision=None):
    if status not in RSVP:
        raise CalendarError("Choose Yes, Maybe or No.")
    body = {"status": status, "scope": "this" if scope == "this" else "all"}
    if scope == "this":
        if not valid(OCCURRENCE_ID, occurrence_id):
            raise CalendarError("Choose the occurrence to answer.")
        body["occurrence_id"] = occurrence_id
    if note:
        body["note"] = note.strip()[:300]
    if number(attendee_revision) is not None:
        body["expected_revision"] = attendee_revision  # the attendee's own revision, never the event's
    return body


def wire(body):
    return json.dumps(body, separators=(",", ":"), ensure_ascii=False, sort_keys=True).encode()


# ---- protected creating writes -------------------------------------------------------------------

KINDS = {"calendar": "/v1/calendars", "event": "/v1/calendar/events"}


def uncertain(error):
    """No answer, or a server failure: the write may have happened."""
    if isinstance(error, Retired):
        return True
    status = getattr(error, "status", 0)
    return status == 0 and getattr(error, "code", "") in ("network_error", "invalid_response") or status >= 500 or status == 408


class Attempts:
    """At most one saved creating write per kind ("calendar", "event") per account and mailbox,
    inside the protected record's "calendar" entry."""

    def __init__(self, store):
        self.store = store

    @staticmethod
    def _entries(record):
        section = record.setdefault("calendar", {})
        entries = section.setdefault("attempts", [])
        if not isinstance(entries, list) or len(entries) > len(KINDS):
            raise ValueError("calendar attempts")
        return entries

    @staticmethod
    def _check(entry):
        raw = base64.b64decode(entry["wire"], validate=True)
        body = json.loads(raw)
        if (entry.get("kind") not in KINDS or hashlib.sha256(raw).hexdigest() != entry.get("sha")
                or not isinstance(body, dict) or body.get("request_key") != entry.get("request_key")
                or not valid(REQUEST_KEY, entry.get("request_key")) or wire(body) != raw):
            raise ValueError("calendar attempt")
        return raw, body

    def pending(self, lease, mailbox):
        record = self.store.read(lease, mailbox)
        out = []
        for entry in self._entries(record):
            raw, body = self._check(entry)
            out.append({"kind": entry["kind"], "request_key": entry["request_key"], "body": body, "wire": raw,
                        "created_at": entry.get("created_at") or 0, "label": text(body.get("title") or body.get("name"), 120)})
        return out

    def save(self, lease, mailbox, kind, body):
        raw = wire(body)

        def change(record):
            entries = self._entries(record)
            for entry in entries:
                if entry.get("kind") == kind or entry.get("request_key") == body["request_key"]:
                    if entry.get("sha") == hashlib.sha256(raw).hexdigest():
                        return
                    raise CalendarError("A calendar change is waiting for review. Retry or discard it first.", "attempt_conflict")
            entries.append({"kind": kind, "request_key": body["request_key"], "wire": base64.b64encode(raw).decode(),
                            "sha": hashlib.sha256(raw).hexdigest(), "created_at": int(time.time())})
        self.store.update(lease, mailbox, change)
        return raw

    def remove(self, lease, mailbox, kind, request_key):
        """Conditional on the request key: never removes a newer write of the same kind."""
        def change(record):
            entries = self._entries(record)
            before = len(entries)
            entries[:] = [e for e in entries if not (e.get("kind") == kind and e.get("request_key") == request_key)]
            return before != len(entries)
        return self.store.update(lease, mailbox, change)


class CalendarClient:
    """One mailbox's calendar through a lease-bound postbox (api.Postbox.bound)."""

    def __init__(self, api, mailbox, calls_base=CALLS):
        if not isinstance(mailbox, str) or not mailbox.startswith("/"):
            raise CalendarError("Choose an inbox first.")
        self.api, self.mailbox, self.calls_base = api, mailbox, calls_base.rstrip("/")

    def _get(self, path, binary=False):
        return self.api.scoped("GET", path, self.mailbox, binary=binary)

    def _send(self, method, path, body):
        return self.api.scoped(method, path, self.mailbox, raw=wire(body), content_type="application/json")

    @staticmethod
    def _segment(pattern, value):
        if not valid(pattern, value):
            raise CalendarError("This calendar item is invalid.")
        return value

    def calendars(self):
        return check_calendars(self._get("/v1/calendars"))

    def occurrences(self, start, end, calendars=None):
        if end <= start or end - start > MAX_WINDOW:
            raise CalendarError("Choose a calendar window of at most 62 days.")
        query = {"from": str(int(start)), "to": str(int(end)), "include": "invitations"}
        if calendars:
            query["calendars"] = ",".join(self._segment(CALENDAR_ID, c) for c in calendars)
        return check_occurrences(self._get("/v1/calendar/events?" + urllib.parse.urlencode(query)))

    def event(self, event_id, occurrence_id=None):
        path = "/v1/calendar/events/" + self._segment(EVENT_ID, event_id)
        if occurrence_id:
            path += "?occurrence_id=" + self._segment(OCCURRENCE_ID, occurrence_id)
        return check_event(self._get(path), event_id, occurrence_id)

    def invitations(self, filtered=False):
        return check_invitations(self._get("/v1/calendar/invitations?state=" + ("filtered" if filtered else "pending")))

    def settings(self):
        result = self._get("/v1/calendar/settings")
        tz = result.get("timezone")
        week = number(result.get("week_start"))
        return {"previews": result.get("previews") is True, "timezone": tz if zone(tz) else None,
                "week_start": week if week in range(1, 8) else 1,
                "working_hours": result.get("working_hours") if isinstance(result.get("working_hours"), dict) else None}

    def save_settings(self, changes):
        allowed = {k: v for k, v in changes.items() if k in ("previews", "timezone", "week_start")}
        if "timezone" in allowed and allowed["timezone"] is not None and not zone(allowed["timezone"]):
            raise CalendarError("Choose a known time zone.", "zone_unknown")
        return self._send("PUT", "/v1/calendar/settings", allowed)

    # creating writes (exact bytes)
    def post_exact(self, kind, raw):
        return self.api.scoped("POST", KINDS[kind], self.mailbox, raw=raw, content_type="application/json")

    def update_event(self, event_id, body):
        return self._send("PATCH", "/v1/calendar/events/" + self._segment(EVENT_ID, event_id), body)

    def cancel_event(self, event_id, expected_revision, scope="all", occurrence_id=None):
        body = patch_body({}, expected_revision, scope, occurrence_id)
        return self._send("POST", "/v1/calendar/events/" + self._segment(EVENT_ID, event_id) + "/cancel", body)

    def change_attendees(self, event_id, expected_revision, add=(), remove=()):
        body = {"expected_revision": expected_revision, "add": list(add),
                "remove": [self._segment(ATTENDEE_ID, a) for a in remove], "scope": "all"}
        return self._send("POST", "/v1/calendar/events/" + self._segment(EVENT_ID, event_id) + "/attendees", body)

    def rsvp(self, event_id, body):
        result = self._send("POST", "/v1/calendar/events/" + self._segment(EVENT_ID, event_id) + "/rsvp", body)
        return {"rsvp": result.get("rsvp") if result.get("rsvp") in RSVP else body["status"],
                "revision": number(result.get("revision"))}

    def set_reminders(self, event_id, minutes):
        return self._send("PUT", "/v1/calendar/events/" + self._segment(EVENT_ID, event_id) + "/reminders",
                          {"reminders": reminder_list(minutes)})

    def ics(self, event_id):
        data = self._get("/v1/calendar/events/" + self._segment(EVENT_ID, event_id) + ".ics", binary=True)
        data = bytes(data)
        if len(data) > MAX_IMPORT * 4 or not data.lstrip().startswith(b"BEGIN:VCALENDAR"):
            raise bad_response()
        return data

    def update_calendar(self, calendar_id, expected_revision, **fields):
        body = {"expected_revision": expected_revision}
        if "name" in fields:
            name = (fields["name"] or "").strip()
            if not name or len(name) > 80:
                raise CalendarError("Name the calendar (up to 80 characters).")
            body["name"] = name
        if "color" in fields:
            if fields["color"] not in COLORS:
                raise CalendarError("Choose a color.")
            body["color"] = fields["color"]
        if fields.get("is_default"):
            body["is_default"] = True
        return self._send("PATCH", "/v1/calendars/" + self._segment(CALENDAR_ID, calendar_id), body)

    def delete_calendar(self, calendar_id, expected_revision):
        return self._send("DELETE", "/v1/calendars/" + self._segment(CALENDAR_ID, calendar_id), {"expected_revision": expected_revision})

    def import_preview(self, data, calendar_id=None):
        if not isinstance(data, (bytes, bytearray)) or not data or len(data) > MAX_IMPORT:
            raise CalendarError("Choose an .ics file of at most 512 KiB.")
        path = "/v1/calendar/import/preview"
        if calendar_id:
            path += "?calendar_id=" + self._segment(CALENDAR_ID, calendar_id)
        result = self.api.scoped("POST", path, self.mailbox, raw=bytes(data), content_type="text/calendar")
        draft = result.get("draft_id")
        if not isinstance(draft, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", draft):
            raise bad_response()
        events = []
        for row in result.get("events") or []:
            if not isinstance(row, dict) or number(row.get("index")) is None:
                raise bad_response()
            events.append({"index": row["index"], "title": text(row.get("title"), 300) or "(No title)",
                           "all_day": row.get("all_day") is True, "start": text(row.get("start"), 32),
                           "start_date": text(row.get("start_date"), 16), "tzid": text(row.get("tzid"), 64),
                           "rrule": text(row.get("rrule"), 500),
                           "warnings": [text(w, 200) for w in row.get("warnings") or [] if isinstance(w, str)][:10]})
        if len(events) > 1000:
            raise bad_response()
        return {"draft_id": draft, "events": events,
                "warnings": [text(w, 200) for w in result.get("warnings") or [] if isinstance(w, str)][:20]}

    def import_confirm(self, draft_id, select, calendar_id=None):
        """Replay-safe: the same selection and destination return the original result (§4)."""
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", draft_id or ""):
            raise CalendarError("This import expired. Choose the file again.")
        chosen = sorted({int(i) for i in select})
        if not chosen or len(chosen) > 200:
            raise CalendarError("Choose 1–200 events to import.")
        body = {"select": chosen}
        if calendar_id:
            body["calendar_id"] = self._segment(CALENDAR_ID, calendar_id)
        result = self._send("POST", "/v1/calendar/import/" + draft_id + "/confirm", body)
        created = result.get("created")
        if not isinstance(created, list):
            raise bad_response()
        return len(created)

    def feed(self, calendar_id):
        result = self._get("/v1/calendar/feeds/" + self._segment(CALENDAR_ID, calendar_id))
        return {"enabled": result.get("enabled") is True, "projection": text(result.get("projection"), 30),
                "expires_at": number(result.get("expires_at"))}

    def set_feed(self, calendar_id, enabled, projection="busy"):
        if projection not in ("busy", "personal_redacted"):
            raise CalendarError("Choose what the feed shows.")
        body = {"enabled": bool(enabled)}
        if enabled:
            body["projection"] = projection
        result = self._send("POST", "/v1/calendar/feeds/" + self._segment(CALENDAR_ID, calendar_id), body)
        if enabled:
            url = result.get("url")
            parts = urllib.parse.urlsplit(url) if isinstance(url, str) else None
            if not parts or parts.scheme not in ("https", "http") or (parts.scheme == "http" and parts.hostname != "127.0.0.1") \
                    or not parts.path.endswith(".ics") or len(url) > 1024:
                raise bad_response()
            return {"url": url, "expires_at": number(result.get("expires_at"))}
        return {"url": None}

    def scheduled_status(self, code):
        """The calls service's view of a meeting code for this signed-in member (§7)."""
        code = parse_code(code)
        if not code:
            raise CalendarError("This meeting code is not valid.")
        token = self.api._token()
        result = self.api.transport.request("GET", self.calls_base + "/v1/meetings/scheduled/" + code, token=token, timeout=15)
        state = result.get("state") if result.get("state") in ("open", "not_open", "unavailable") else "unavailable"
        windows = []
        for o in result.get("occurrences") or []:
            if isinstance(o, dict) and valid(OCCURRENCE_ID, o.get("occurrence_id")):
                windows.append({"occurrence_id": o["occurrence_id"], "starts_at": number(o.get("starts_at")),
                                "ends_at": number(o.get("ends_at")), "opens_at": number(o.get("opens_at")),
                                "closes_at": number(o.get("closes_at"))})
        return {"state": state, "occurrences": windows[:20]}


# ---- joining --------------------------------------------------------------------------------------

def join_plan(detail, status, now, tz):
    """What to do for 'Join' on one occurrence: {"ok", "url", "message"}.

    `detail` is a fresh check_event() result for the exact occurrence; `status` the calls
    service's scheduled status or None when it could not be asked."""
    event, occ = detail["event"], detail.get("occurrence")
    code = (event.get("meeting") or {}).get("code")
    if event["status"] != "confirmed" or (occ and occ["status"] != "confirmed") or event["all_day"] or not code:
        return {"ok": False, "message": "This event has no available scheduled call."}
    role = event["my"]["role"]
    rsvp = (occ or {}).get("my_rsvp") or event["my"]["rsvp"]
    if role not in ("organizer", "attendee", "cohost") or rsvp == "declined":
        return {"ok": False, "message": "This inbox cannot join the meeting. Review its invitation first."}
    url = meeting_url(code)
    if status is None:
        return {"ok": True, "url": url, "message": "Opening the meeting in your browser."}
    if status["state"] == "unavailable":
        return {"ok": False, "message": "This meeting is not available."}
    selected = occ["occurrence_id"] if occ else None
    windows = status["occurrences"]
    open_now = [w for w in windows if (w["opens_at"] or 0) <= now < (w["closes_at"] or 0)]
    mine = next((w for w in windows if w["occurrence_id"] == selected), None)
    if mine and not (mine["opens_at"] or 0) <= now < (mine["closes_at"] or 0):
        when = in_zone(mine["opens_at"], tz).strftime("%a %d %b %H:%M") if mine["opens_at"] else "shortly before it starts"
        return {"ok": False, "message": f"This meeting opens {when}."}
    if status["state"] == "not_open" and not open_now:
        return {"ok": False, "message": "This meeting isn’t open yet. It opens shortly before it starts."}
    message = "The meeting opens in your browser, where the call runs."
    if len(open_now) > 1 and occ and occ.get("starts_at"):
        start = in_zone(occ["starts_at"], tz)
        message = (f"Two meetings in this series are open. In your browser choose the one at {start:%H:%M} "
                   f"({start:%a %d %b}).")
    return {"ok": True, "url": url, "message": message}


# ---- reminders -------------------------------------------------------------------------------------

def due_reminders(occurrences, reminders, fired, now, horizon=60):
    """[(key, occurrence, minutes)] whose time has come (within the last five minutes) or comes in
    the next `horizon` seconds. Declined and cancelled occurrences have no reminders; invitations
    still waiting for an answer do not either (they are not on the calendar yet, §3.2)."""
    out = []
    for o in occurrences:
        if o["all_day"] or o["status"] != "confirmed" or o["my_rsvp"] in ("declined",) or o["invited"] and o["my_rsvp"] == "needs_action":
            continue
        for minutes in reminders.get(o["event_id"], [10]):
            at = o["starts_at"] - minutes * 60
            key = f"{o['occurrence_id']}:{minutes}"
            if key in fired or not (now - REMINDER_LATE <= at <= now + horizon):
                continue
            out.append((key, o, minutes, at))
    return sorted(out, key=lambda x: x[3])


def reminder_text(o, minutes, previews):
    when = "now" if minutes <= 0 else (f"in {minutes} minutes" if minutes < 120 else f"in {minutes // 60} hours")
    title = f"Event starting {when}"
    return (title, o["title"]) if previews else (title, "Open Pigeonpost to see the details.")
