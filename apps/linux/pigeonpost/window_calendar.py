"""Calendar page (SDS §14B, docs/calendar.md): week, day, month and agenda views, the event
editor, invitations and answers, personal reminders as desktop notifications, .ics import and
export, feeds and settings, and joining a scheduled meeting in the browser.

Ownership rules (as on Windows and Android):
* reads belong to the selected mailbox and are dropped when the selection changes (_work);
* writes take their own lease for the account and mailbox that started them (bound_work), so a
  result never lands in another mailbox's view or another login; creating writes are saved first
  (calendar_core.Attempts) and an uncertain one is offered for explicit Retry or Discard;
* nothing is ever retried or approved automatically; a stale revision reloads, never overwrites.
Calendar data is not end-to-end encrypted, and the page never says it is.
"""

import datetime
import math
import time

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gio, GLib, Gtk

from .api import APIError
from .binding import Retired
from .calendar_core import (CALENDAR, CALENDAR_EMAIL, COLORS, KINDS, MAX_IMPORT, RETRY_WINDOW, RSVP, WEEKDAYS, Attempts,
                            CalendarClient, CalendarError, available_timezones, build_rrule, create_event_body,
                            day_segments, describe_rrule, display_code, due_reminders, event_fields, in_zone, join_plan,
                            local_zone_name, meeting_url, occurrence_days, parse_rrule, patch_body, range_for, reminder_text,
                            rsvp_body, shift, uncertain, when_text, window_for, zone)
from .model import display_name
from .theme import add_css
from .widgets import box, button, clear, label, scroll

SLOT_HEIGHT = 22
SLOTS = 48
UPCOMING_SECONDS = 60
REMINDER_CHOICES = [(None, "No reminder"), (0, "At the start"), (5, "5 minutes before"), (10, "10 minutes before"),
                    (30, "30 minutes before"), (60, "1 hour before"), (1440, "1 day before")]
VIEWS = (("day", "Day"), ("week", "Week"), ("month", "Month"), ("agenda", "Agenda"))
COLOR_VALUES = {"blue": "@pp_accent", "green": "@pp_green", "orange": "@pp_amber", "red": "@pp_rose",
                "purple": "#8B5CF6", "teal": "#14998B", "gray": "@pp_muted"}

add_css("".join(f".pp-cal-{name} {{ border-left: 3px solid {value}; background-color: alpha({value}, 0.14); }}\n"
                f".pp-cal-dot-{name} {{ color: {value}; }}\n" for name, value in COLOR_VALUES.items()) + """
.pp-cal-event { border-radius: 6px; padding: 1px 6px; font-size: 12px; color: @pp_ink; min-height: 0; box-shadow: none; }
.pp-cal-event label { font-size: 12px; }
.pp-cal-invited { border-style: dashed; border-left-width: 3px; }
.pp-cal-cancelled label { text-decoration: line-through; color: @pp_muted; }
.pp-cal-declined { opacity: 0.55; }
.pp-cal-slot { border-top: 1px solid alpha(@pp_rule, 0.5); }
.pp-cal-hour { border-top: 1px solid @pp_rule; }
.pp-cal-time { font-size: 11px; color: @pp_muted; font-family: "Geist Mono", monospace; }
.pp-cal-dayhead { font-size: 12px; font-weight: 600; color: @pp_quiet; padding: 4px; }
.pp-cal-today { color: @pp_accent; }
.pp-cal-cell { border: 1px solid alpha(@pp_rule, 0.7); padding: 3px; min-height: 92px; }
.pp-cal-outside { opacity: 0.5; }
.pp-cal-pending { background-color: alpha(@pp_amber, 0.08); border: 1px solid alpha(@pp_amber, 0.4); border-radius: 10px; padding: 6px 6px 6px 12px; }
.pp-cal-code { font-family: "Geist Mono", monospace; font-size: 15px; font-weight: 600; }
""")


def lcm(values):
    out = 1
    for value in values:
        out = out * value // math.gcd(out, value)
    return out


class CalendarMixin:
    # ---- lifecycle -----------------------------------------------------------------------------

    def reset_calendar(self):
        """Mailbox or account change: forget everything that belonged to the previous one."""
        self.cal = {"calendars": [], "occurrences": [], "settings": None, "invitations": [], "filtered_invitations": [],
                    "pending": [], "loading": False, "error": None, "version": getattr(self, "cal", {}).get("version", 0) + 1,
                    "reminders": {}, "fired": set(), "upcoming": [], "upcoming_at": 0, "rsvp_revisions": {}, "hidden": set(),
                    "detail_fetching": set()}
        if not hasattr(self, "cal_view"):
            self.cal_view, self.cal_anchor = "week", datetime.date.today()
        if getattr(self, "cal_page", None) is not None and self.page() == "calendar":
            self.render_calendar()

    def calendar_on(self):
        return CALENDAR in self.features and bool(self.identity)

    def calendar_features_changed(self):
        on = self.calendar_on()
        self.calendar_button.set_visible(on)
        app = self.get_application()
        if on and app and not app.lookup_action("open-calendar"):
            action = Gio.SimpleAction.new("open-calendar", GLib.VariantType.new("s"))
            # Resolved at activation: the application's current window, never a closed one.
            action.connect("activate", lambda _a, parameter: getattr(getattr(app, "window", None) or self, "open_calendar_notification")(
                parameter.get_string()))
            app.add_action(action)
        if not on and self.page() == "calendar":
            self.main_stack.set_visible_child_name("conversation")
            self.sync_side_nav()
        if on and self.page() == "calendar":
            self.load_calendar()

    def open_calendar_notification(self, identity):
        if self.closed:
            return
        self.present()
        if identity == self.identity and self.calendar_on():
            self.show_page("calendar")

    def page_shown_calendar(self):
        if not self.calendar_on():
            return
        self.render_calendar()
        self.load_calendar()

    def cal_zone(self):
        settings = self.cal.get("settings") or {}
        return zone(settings.get("timezone")) or zone(local_zone_name()) or datetime.timezone.utc

    def cal_client(self, lease=None):
        lease = lease or self.mailbox_lease
        return CalendarClient(self.api_for(lease), self.identity if lease is None or lease.mailbox is None else lease.mailbox)

    def cal_write_lease(self):
        """A lease for one write: this account and mailbox, independent of later selection."""
        if self.lease is None or not self.identity:
            raise Retired()
        return self.lease.child(self.identity)

    # ---- loading -------------------------------------------------------------------------------

    def load_calendar(self):
        if not self.calendar_on() or self.cal["loading"]:
            return
        self.cal["loading"] = True
        version, mailbox = self.cal["version"], self.identity
        client = self.cal_client()
        attempts = Attempts(self.protected)
        lease = self.mailbox_lease
        view, anchor = self.cal_view, self.cal_anchor

        def read():
            settings = client.settings()
            tz = zone(settings.get("timezone")) or zone(local_zone_name()) or datetime.timezone.utc
            first, days = range_for(view, anchor, settings.get("week_start") or 1)
            start, end = window_for(first, days, tz)
            calendars = client.calendars()
            occurrences = client.occurrences(start, end)
            invitations = client.invitations()
            try:
                filtered = client.invitations(filtered=True)
            except APIError:
                filtered = []
            try:
                pending = attempts.pending(lease, mailbox) if lease is not None else []
            except Retired:
                raise
            except Exception:
                pending = None
            return settings, calendars, occurrences, invitations, filtered, pending, (view, anchor)

        def loaded(result):
            self.cal["loading"] = False
            if version != self.cal["version"] or mailbox != self.identity:
                return
            settings, calendars, occurrences, invitations, filtered, pending, shown = result
            self.cal.update(settings=settings, calendars=calendars, occurrences=occurrences, invitations=invitations,
                            filtered_invitations=filtered, error=None, pending=pending if pending is not None else [],
                            pending_error=pending is None, shown=shown)
            self.render_calendar()
            if shown != (self.cal_view, self.cal_anchor):
                self.load_calendar()

        def failed(error):
            self.cal["loading"] = False
            if version != self.cal["version"]:
                return
            self.cal["error"] = self.cal_error_text(error)
            self.render_calendar()
        self._work(read, loaded, failed)

    def cal_error_text(self, error):
        code = getattr(error, "code", "")
        texts = {
            "feature_disabled": "The calendar is not available for this account.",
            "revision_conflict": "This changed elsewhere since you opened it. The latest version was loaded; review your changes and save again.",
            "human_required": "Inviting people by contact address needs a person’s signed-in session with a verified address.",
            "time_invalid": "Check the date and time.", "zone_unknown": "Choose a known time zone.",
            "recurrence_invalid": "This repeat rule is not supported.", "window_too_large": "Choose a shorter period.",
            "expansion_limit": "Too many events in this period. Choose a shorter view.",
            "attendee_limit": "Too many guests for one event.", "invite_limit": "You have sent too many invitations today.",
            "meeting_capacity": "Too many people for this kind of call. Make some guests optional or choose fewer.",
            "event_cancelled": "This event was cancelled.", "not_found": "This event is no longer available to you.",
            "forbidden": "Only the organizer can change this event.", "rate_limited": "Too many requests. Wait a moment and try again.",
            "idempotency_conflict": "This request was already used differently. Start again.",
        }
        if code == "time_invalid" and isinstance(getattr(error, "body", None), dict):
            kind = error.body.get("kind")
            if kind == "nonexistent":
                return "That time does not exist in this time zone (the clocks skip it). Choose another time."
            if kind == "ambiguous":
                return "That time happens twice in this time zone. Choose the earlier or later one."
            if kind == "order":
                return "The event must end after it starts."
        return texts.get(code) or self.error_text(error)

    # ---- page ----------------------------------------------------------------------------------

    def build_calendar_page(self):
        page = box(8, margin=12)
        toolbar = box(6, horizontal=True)
        toolbar.append(button("Today", self.cal_today, style="pp-pill-button"))
        toolbar.append(button("Previous", lambda: self.cal_move(-1), "go-previous-symbolic", "flat"))
        toolbar.append(button("Next", lambda: self.cal_move(1), "go-next-symbolic", "flat"))
        self.cal_title = label("Calendar", "pp-title")
        self.cal_title.set_hexpand(True)
        toolbar.append(self.cal_title)
        strip = box(0, horizontal=True)
        strip.add_css_class("pp-segmented")
        self.cal_view_buttons = {}
        first = None
        for name, title in VIEWS:
            toggle = Gtk.ToggleButton(label=title)
            toggle.update_property([Gtk.AccessibleProperty.LABEL], [title + " view"])
            if first:
                toggle.set_group(first)
            first = first or toggle
            toggle.connect("toggled", lambda t, value=name: self.cal_set_view(value) if t.get_active() else None)
            self.cal_view_buttons[name] = toggle
            strip.append(toggle)
        toolbar.append(strip)
        self.cal_invites_button = button("Invitations", self.cal_invitations_dialog, style="pp-pill-button")
        toolbar.append(self.cal_invites_button)
        toolbar.append(button("New event", lambda: self.cal_editor(), "list-add-symbolic", "suggested-action"))
        actions = Gio.SimpleActionGroup()
        for name, handler in (("import", self.cal_import), ("calendars", self.cal_calendars_dialog),
                              ("settings", self.cal_settings_dialog), ("refresh", self.load_calendar)):
            action = Gio.SimpleAction.new(name, None)
            action.connect("activate", lambda *_, run=handler: run())
            actions.add_action(action)
        page.insert_action_group("calendar", actions)
        menu = Gio.Menu()
        menu.append("Import an .ics file…", "calendar.import")
        menu.append("Calendars and feeds…", "calendar.calendars")
        menu.append("Calendar settings…", "calendar.settings")
        menu.append("Refresh", "calendar.refresh")
        more = Gtk.MenuButton(icon_name="view-more-symbolic", menu_model=menu)
        more.add_css_class("flat")
        more.set_tooltip_text("Calendar menu")
        more.update_property([Gtk.AccessibleProperty.LABEL], ["Calendar menu"])
        toolbar.append(more)
        page.append(toolbar)
        self.cal_banner = box(6)
        page.append(self.cal_banner)
        self.cal_status = label("", "pp-status", wrap=True)
        page.append(self.cal_status)
        body = box(10, horizontal=True)
        body.set_vexpand(True)
        self.cal_list = box(4)
        self.cal_list.set_size_request(150, -1)
        body.append(self.cal_list)
        self.cal_body = box(0)
        self.cal_body.set_hexpand(True)
        self.cal_scroll = scroll(self.cal_body)
        self.cal_scroll.set_hexpand(True)
        body.append(self.cal_scroll)
        page.append(body)
        self.cal_page = page
        return page

    def cal_today(self):
        self.cal_anchor = datetime.date.today()
        self.cal_reload_view()

    def cal_move(self, step):
        self.cal_anchor = shift(self.cal_view, self.cal_anchor, step)
        self.cal_reload_view()

    def cal_set_view(self, view):
        if view != self.cal_view:
            self.cal_view = view
            self.cal_reload_view()

    def cal_reload_view(self):
        self.render_calendar()
        self.load_calendar()

    def render_calendar(self):
        if getattr(self, "cal_page", None) is None:
            return
        for name, toggle in self.cal_view_buttons.items():
            if toggle.get_active() != (name == self.cal_view):
                toggle.set_active(name == self.cal_view)
        settings = self.cal.get("settings") or {}
        tz = self.cal_zone()
        first, days = range_for(self.cal_view, self.cal_anchor, settings.get("week_start") or 1)
        last = first + datetime.timedelta(days=days - 1)
        if self.cal_view == "month":
            self.cal_title.set_text(self.cal_anchor.strftime("%B %Y"))
        elif days == 1:
            self.cal_title.set_text(first.strftime("%A %d %B %Y"))
        else:
            self.cal_title.set_text(f"{first:%d %b} – {last:%d %b %Y}")
        count = len(self.cal["invitations"])
        self.cal_invites_button.set_label(f"Invitations ({count})" if count else "Invitations")
        self.cal_invites_button.update_property([Gtk.AccessibleProperty.LABEL], [f"Invitations, {count} waiting"])
        self.render_calendar_banner()
        status = self.cal.get("error") or ("Loading…" if self.cal["loading"] and not self.cal["occurrences"] else "")
        self.cal_status.set_text(status)
        self.cal_status.set_visible(bool(status))
        clear(self.cal_list)
        self.cal_list.append(label("Calendars", "pp-overline"))
        for c in self.cal["calendars"]:
            check = Gtk.CheckButton(label=c["name"], active=c["id"] not in self.cal["hidden"])
            check.add_css_class("pp-cal-dot-" + c["color"])
            check.set_tooltip_text(("Default calendar · " if c["is_default"] else "") + c["color"])
            check.connect("toggled", lambda b, cid=c["id"]: self.cal_toggle_calendar(cid, b.get_active()))
            self.cal_list.append(check)
        if not self.cal["calendars"]:
            self.cal_list.append(label("Your calendar is made with your first event.", "dim-label", wrap=True))
        clear(self.cal_body)
        shown = [o for o in self.cal["occurrences"] if o.get("calendar_id") not in self.cal["hidden"]]
        if self.cal_view in ("week", "day"):
            self.cal_body.append(self.cal_time_grid(shown, first, days, tz))
            adj = self.cal_scroll.get_vadjustment()
            GLib.idle_add(lambda: (adj.set_value(max(0, 2 * 8 * SLOT_HEIGHT)), False)[1])
        elif self.cal_view == "month":
            self.cal_body.append(self.cal_month(shown, first, tz))
        else:
            self.cal_body.append(self.cal_agenda(shown, first, days, tz))

    def cal_toggle_calendar(self, calendar_id, visible):
        (self.cal["hidden"].discard if visible else self.cal["hidden"].add)(calendar_id)
        self.render_calendar()

    def render_calendar_banner(self):
        clear(self.cal_banner)
        if self.cal.get("pending_error"):
            self.cal_banner.append(label("Saved calendar changes could not be opened. Unlock your desktop keyring, then refresh.", "error", wrap=True))
        for attempt in self.cal["pending"]:
            row = box(8, horizontal=True)
            row.add_css_class("pp-cal-pending")
            what = "calendar" if attempt["kind"] == "calendar" else "event"
            old = time.time() - attempt["created_at"] > RETRY_WINDOW
            text = (f"Creating the {what} “{attempt['label'] or 'untitled'}” was not confirmed. "
                    + ("It is too old to retry safely: check your calendar, then discard it." if old
                       else "Retry sends exactly the same request; it is never created twice. Discard forgets it here only."))
            note = label(text, wrap=True)
            note.set_hexpand(True)
            row.append(note)
            retry = button("Retry", lambda a=attempt: self.cal_retry(a), style="suggested-action")
            retry.set_sensitive(not old)
            row.append(retry)
            row.append(button("Discard", lambda a=attempt: self.cal_discard(a)))
            self.cal_banner.append(row)
        if self.cal["pending"] and not self.protected.available:
            self.cal_banner.append(label("Unconfirmed changes are kept only until Pigeonpost closes on this computer.", "dim-label", wrap=True))

    # ---- views ---------------------------------------------------------------------------------

    def cal_chip(self, o, tz, compact=False):
        start = "" if o["all_day"] else in_zone(o["starts_at"], tz).strftime("%H:%M") + " "
        widget = Gtk.Button()
        text = label((start if compact else "") + o["title"], None)
        widget.set_child(text)
        widget.add_css_class("flat")
        widget.add_css_class("pp-cal-event")
        widget.add_css_class("pp-cal-" + o["color"])
        if o["invited"] and o["my_rsvp"] == "needs_action":
            widget.add_css_class("pp-cal-invited")
        if o["status"] == "cancelled":
            widget.add_css_class("pp-cal-cancelled")
        if o["my_rsvp"] == "declined":
            widget.add_css_class("pp-cal-declined")
        extra = []
        if o["invited"] and o["my_rsvp"] == "needs_action":
            extra.append("invitation, not answered")
        if o["status"] == "cancelled":
            extra.append("cancelled")
        if o["meeting"]:
            extra.append("scheduled call")
        description = ", ".join([o["title"], when_text(o, tz)] + ([o["location"]] if o["location"] else []) + extra)
        widget.set_tooltip_text(description)
        widget.update_property([Gtk.AccessibleProperty.LABEL], [description])
        widget.occurrence = o
        widget.connect("clicked", lambda _: self.cal_open_event(o["event_id"], o["occurrence_id"]))
        return widget

    def cal_time_grid(self, occurrences, first, days, tz):
        grid = Gtk.Grid(column_spacing=2, row_spacing=0)
        grid.add_css_class("pp-cal-grid")
        grid.set_hexpand(True)
        today = datetime.date.today()
        for hour in range(24):
            text = label(f"{hour:02d}:00", "pp-cal-time")
            text.set_valign(Gtk.Align.START)
            grid.attach(text, 0, 2 + hour * 2, 1, 2)
        for index in range(days):
            day = first + datetime.timedelta(days=index)
            head = label(day.strftime("%a %d"), "pp-cal-dayhead")
            if day == today:
                head.add_css_class("pp-cal-today")
            head.set_hexpand(True)
            grid.attach(head, index + 1, 0, 1, 1)
            all_day = box(2)
            for o in occurrences:
                if o["all_day"] and day in occurrence_days(o, tz):
                    all_day.append(self.cal_chip(o, tz))
            grid.attach(all_day, index + 1, 1, 1, 1)
            column = Gtk.Grid(row_homogeneous=True, column_homogeneous=True)
            column.set_hexpand(True)
            segments = day_segments(occurrences, day, tz)
            widths = sorted({cols for *_, cols in segments}) or [1]
            span = min(60, lcm(widths))
            for slot in range(SLOTS):
                spacer = box(0)
                spacer.set_size_request(-1, SLOT_HEIGHT)
                spacer.add_css_class("pp-cal-hour" if slot % 2 == 0 else "pp-cal-slot")
                column.attach(spacer, 0, slot, span, 1)
            for o, top, rows, col, cols in segments:
                width = max(1, span // cols)
                chip = self.cal_chip(o, tz, compact=True)
                chip.set_valign(Gtk.Align.FILL)
                column.attach(chip, min(span - 1, col * width), top, width, rows)
            grid.attach(column, index + 1, 2, 1, SLOTS)
        return grid

    def cal_month(self, occurrences, first, tz):
        grid = Gtk.Grid(column_homogeneous=True, row_spacing=0, column_spacing=0)
        grid.set_hexpand(True)
        today = datetime.date.today()
        by_day = {}
        for o in occurrences:
            for day in occurrence_days(o, tz):
                by_day.setdefault(day, []).append(o)
        for index in range(7):
            grid.attach(label((first + datetime.timedelta(days=index)).strftime("%a"), "pp-cal-dayhead"), index, 0, 1, 1)
        for index in range(42):
            day = first + datetime.timedelta(days=index)
            cell = box(2)
            cell.add_css_class("pp-cal-cell")
            if day.month != self.cal_anchor.month:
                cell.add_css_class("pp-cal-outside")
            number = label(str(day.day), "pp-cal-dayhead")
            if day == today:
                number.add_css_class("pp-cal-today")
            cell.append(number)
            rows = sorted(by_day.get(day, []), key=lambda o: (not o["all_day"], o["starts_at"] or 0))
            for o in rows[:3]:
                cell.append(self.cal_chip(o, tz, compact=True))
            if len(rows) > 3:
                more = button(f"+{len(rows) - 3} more", lambda d=day: self.cal_open_day(d), style="flat")
                cell.append(more)
            grid.attach(cell, index % 7, 1 + index // 7, 1, 1)
        return grid

    def cal_open_day(self, day):
        self.cal_view, self.cal_anchor = "day", day
        self.cal_reload_view()

    def cal_agenda(self, occurrences, first, days, tz):
        content = box(10)
        by_day = {}
        for o in occurrences:
            for day in occurrence_days(o, tz):
                if first <= day < first + datetime.timedelta(days=days):
                    by_day.setdefault(day, []).append(o)
        for day in sorted(by_day):
            content.append(label(day.strftime("%A %d %B"), "heading"))
            for o in sorted(by_day[day], key=lambda o: (not o["all_day"], o["starts_at"] or 0)):
                row = box(8, horizontal=True)
                row.append(label(when_text(o, tz).split(" · ")[-1], "pp-cal-time"))
                chip = self.cal_chip(o, tz)
                chip.set_hexpand(True)
                row.append(chip)
                if o["meeting"] and not o["all_day"] and o["status"] == "confirmed" and o["my_rsvp"] != "declined":
                    row.append(button("Join", lambda x=o: self.cal_join(x["event_id"], x["occurrence_id"]), style="pp-pill-button"))
                content.append(row)
        if not by_day:
            content.append(label("Nothing scheduled in the next 31 days.", "pp-status"))
        return content

    # ---- event detail --------------------------------------------------------------------------

    def cal_open_event(self, event_id, occurrence_id=None):
        client, version = self.cal_client(), self.cal["version"]

        def shown(detail):
            if version == self.cal["version"]:
                self.cal_detail_dialog(detail)
        self._work(lambda: client.event(event_id, occurrence_id), shown, lambda e: self.toast(self.cal_error_text(e)))

    def cal_detail_dialog(self, detail):
        event, occ = detail["event"], detail["occurrence"]
        tz = self.cal_zone()
        window, content = self.dialog(event["title"] or "Event", 560)
        window.cal_detail = detail
        shown = dict(event, starts_at=(occ or {}).get("starts_at") or event["starts_at"], ends_at=(occ or {}).get("ends_at") or event["ends_at"])
        content.append(label(event["title"] or "(No title)", "title-2"))
        if event["status"] == "cancelled" or (occ and occ["status"] == "cancelled"):
            content.append(label("Cancelled", "error"))
        content.append(label(when_text(shown, tz), "heading", wrap=True))
        if event["tzid"] and not event["all_day"] and zone(event["tzid"]) and str(tz) != event["tzid"]:
            content.append(label(f"Scheduled in {event['tzid']}", "dim-label"))
        if event["rrule"]:
            content.append(label(describe_rrule(event["rrule"]), "dim-label"))
        if event["location"]:
            content.append(label("Location: " + event["location"], wrap=True))
        if event["description"]:
            text = label(event["description"], wrap=True)
            text.set_selectable(True)
            content.append(text)
        organizer = event["organizer"]["handle"] or event["organizer"]["address"]
        content.append(label("Organizer: " + display_name(organizer) if organizer else "Organizer", "dim-label", wrap=True))
        content.append(label("Calendar data is stored by Pigeonpost and is not end-to-end encrypted.", "dim-label", wrap=True))
        buttons = box(6, horizontal=True)
        role = event["my"]["role"]
        rsvp = (occ or {}).get("my_rsvp") or event["my"]["rsvp"]
        if event["meeting"] and not event["all_day"]:
            content.append(label("Scheduled call · code " + display_code(event["meeting"]["code"]), "pp-cal-code"))
            buttons.append(button("Join", lambda: self.cal_join(event["id"], (occ or {}).get("occurrence_id")), style="suggested-action"))
            buttons.append(button("Copy meeting link", lambda: (self.get_clipboard().set(meeting_url(event["meeting"]["code"])),
                                                                 self.toast("Meeting link copied"))))
        if role in ("attendee", "cohost") and event["status"] == "confirmed":
            answer = box(6, horizontal=True)
            answer.append(label("Going?  " + {"accepted": "Yes", "tentative": "Maybe", "declined": "No"}.get(rsvp, "Not answered"), "heading"))
            only_this = Gtk.CheckButton(label="Only this occurrence") if event["rrule"] and occ else None
            for status, title in (("accepted", "Yes"), ("tentative", "Maybe"), ("declined", "No")):
                answer.append(button(title, lambda s=status: self.cal_rsvp(event["id"], s, (occ or {}).get("occurrence_id"),
                                                                           bool(only_this and only_this.get_active()), window)))
            if only_this:
                answer.append(only_this)
            content.append(answer)
        if event["attendees"]:
            content.append(label("Guests", "heading"))
            counts = event.get("counts") or {}
            if counts:
                content.append(label(f"{counts.get('accepted', 0)} yes · {counts.get('tentative', 0)} maybe · "
                                     f"{counts.get('declined', 0)} no · {counts.get('needs_action', 0)} waiting", "dim-label"))
            for a in event["attendees"][:200]:
                row = box(6, horizontal=True)
                name = display_name(a["handle"] or a["address"]) if a["kind"] == "handle" else (a["name"] or "Guest")
                text = label(f"{name} · {a['rsvp'].replace('needs_action', 'not answered')}" + (" · optional" if a["optional"] else ""), wrap=True)
                text.set_hexpand(True)
                row.append(text)
                if event["can_edit"] and event["status"] == "confirmed":
                    row.append(button("Remove " + name, lambda att=a: self.cal_remove_guest(event, att, window), "list-remove-symbolic", "flat"))
                content.append(row)
        if role is not None and event["status"] == "confirmed":
            buttons.append(button("Reminders…", lambda: self.cal_reminders_dialog(event)))
        buttons.append(button("Export .ics", lambda: self.cal_export(event)))
        if event["can_edit"] and event["status"] == "confirmed":
            buttons.append(button("Edit", lambda: (window.close(), self.cal_editor(detail))))
            buttons.append(button("Cancel event…", lambda: self.cal_cancel(detail, window), style="destructive-action"))
        content.append(buttons)
        window.present()
        return window

    def cal_write(self, operation, done, failed=None, reload=True):
        """One write for this account and mailbox; its result never lands elsewhere."""
        try:
            lease = self.cal_write_lease()
        except Retired:
            return
        mailbox = lease.mailbox
        client = self.cal_client(lease)

        def finished(result):
            if mailbox == self.identity:
                done(result)
                if reload:
                    self.load_calendar()

        def refused(error):
            if mailbox == self.identity:
                (failed or (lambda e: self.notice("Calendar", self.cal_error_text(e))))(error)
        self.bound_work(lease, lambda: operation(client), finished, refused)

    def cal_rsvp(self, event_id, status, occurrence_id, only_this, window=None):
        revision = self.cal["rsvp_revisions"].get(event_id)
        try:
            body = rsvp_body(status, "this" if only_this else "all", occurrence_id, attendee_revision=revision)
        except CalendarError as error:
            self.toast(str(error))
            return

        def done(result):
            if result["revision"] is not None:
                self.cal["rsvp_revisions"][event_id] = result["revision"]
            self.toast({"accepted": "You’re going", "tentative": "Answered maybe", "declined": "Declined"}[status])
            if window:
                window.close()

        def failed(error):
            if getattr(error, "code", "") == "revision_conflict":
                self.cal["rsvp_revisions"].pop(event_id, None)
            self.notice("Your answer was not saved", self.cal_error_text(error))
        self.cal_write(lambda c: c.rsvp(event_id, body), done, failed)

    def cal_remove_guest(self, event, attendee, window):
        def remove():
            self.cal_write(lambda c: c.change_attendees(event["id"], event["revision"], remove=[attendee["attendee_id"]]),
                           lambda _: (window.close(), self.toast("Guest removed")))
        self.confirm("Remove this guest?", "They are told the event was cancelled for them.", remove, "Remove")

    def cal_reminders_dialog(self, event):
        window, content = self.dialog("Reminders", 420)
        content.append(label("Your own reminders for this event, as desktop notifications while Pigeonpost runs.", wrap=True))
        current = {r["minutes_before"] for r in event["reminders"]}
        checks = []
        for minutes, title in REMINDER_CHOICES[1:]:
            check = Gtk.CheckButton(label=title, active=minutes in current)
            checks.append((minutes, check))
            content.append(check)
        error = label("", "error", wrap=True)
        content.append(error)

        def save():
            chosen = [m for m, c in checks if c.get_active()]
            if len(chosen) > 5:
                error.set_text("Choose at most five reminders.")
                return
            self.cal_write(lambda c: c.set_reminders(event["id"], chosen),
                           lambda _: (self.cal["reminders"].pop(event["id"], None), window.close(), self.toast("Reminders saved")),
                           lambda e: error.set_text(self.cal_error_text(e)))
        content.append(button("Save reminders", save, style="suggested-action"))
        window.present()

    def cal_export(self, event):
        client = self.cal_client()
        chooser = Gtk.FileChooserNative.new("Export event", self, Gtk.FileChooserAction.SAVE, "Save", "Cancel")
        chooser.set_current_name("".join(c for c in (event["title"] or "event") if c.isalnum() or c in " -_")[:60].strip() + ".ics")

        def chosen(dialog, response):
            file = dialog.get_file() if response == Gtk.ResponseType.ACCEPT else None
            dialog.destroy()
            if not file:
                return

            def save():
                data = client.ics(event["id"])
                file.replace_contents(data, None, False, Gio.FileCreateFlags.REPLACE_DESTINATION, None)
            self._work(save, lambda _: self.toast("Event exported"), lambda e: self.toast(self.cal_error_text(e)))
        chooser.connect("response", chosen)
        chooser.show()

    def cal_cancel(self, detail, parent):
        event, occ = detail["event"], detail["occurrence"]
        window, content = self.dialog("Cancel event", 440)
        content.append(label("Guests are told it was cancelled.", wrap=True))
        scopes = self.cal_scope_picker(event, occ)
        if scopes:
            content.append(scopes)
        error = label("", "error", wrap=True)
        content.append(error)

        def cancel():
            scope = scopes.scope() if scopes else "all"
            occurrence = (occ or {}).get("occurrence_id")
            self.cal_write(lambda c: c.cancel_event(event["id"], event["revision"], scope, occurrence),
                           lambda _: (window.close(), parent.close(), self.toast("Event cancelled")),
                           lambda e: error.set_text(self.cal_error_text(e)))
        content.append(button("Cancel the event", cancel, style="destructive-action"))
        window.present()

    def cal_scope_picker(self, event, occ):
        """This / following / all, only for a recurring event opened on one occurrence."""
        if not event["rrule"] or not occ:
            return None
        holder = box(4)
        holder.append(label("Change", "heading"))
        options = [("this", "This event"), ("following", "This and following events"), ("all", "All events")]
        first, toggles = None, {}
        for value, title in options:
            toggle = Gtk.CheckButton(label=title, active=value == "this")
            if first:
                toggle.set_group(first)
            first = first or toggle
            toggles[value] = toggle
            holder.append(toggle)
        holder.scope = lambda: next(v for v, t in toggles.items() if t.get_active())
        holder.toggles = toggles
        return holder

    # ---- joining -------------------------------------------------------------------------------

    def cal_join(self, event_id, occurrence_id):
        """Fresh event + calls-service status for this exact occurrence, then the browser page."""
        client, version, tz = self.cal_client(), self.cal["version"], self.cal_zone()

        def check():
            detail = client.event(event_id, occurrence_id)
            code = (detail["event"].get("meeting") or {}).get("code")
            status = None
            if code:
                try:
                    status = client.scheduled_status(code)
                except APIError:
                    status = None  # the meeting page asks the calls service itself
            return join_plan(detail, status, int(time.time()), tz)

        def planned(plan):
            if version != self.cal["version"]:
                return
            if not plan["ok"]:
                self.notice("Join the meeting", plan["message"])
                return
            self.open_url(plan["url"])
            self.toast(plan["message"])
        self._work(check, planned, lambda e: self.toast(self.cal_error_text(e)))

    # ---- editor --------------------------------------------------------------------------------

    def cal_editor(self, detail=None):
        """New event, or edit `detail` (organizer only). Never overwrites a newer version."""
        if not self.calendar_on():
            return
        editing = detail is not None
        event = detail["event"] if editing else None
        occ = detail.get("occurrence") if editing else None
        tz_name = (event or {}).get("tzid") or ((self.cal.get("settings") or {}).get("timezone")) or local_zone_name()
        window, content = self.dialog("Edit event" if editing else "New event", 600)
        state = {"revision": event["revision"] if editing else None, "fold": None, "request_key": None}
        title = Gtk.Entry(placeholder_text="Title", text=(event or {}).get("title", ""))
        content.append(title)
        calendars = self.cal["calendars"]
        calendar_pick = None
        if calendars:
            calendar_pick = Gtk.DropDown.new_from_strings([c["name"] for c in calendars])
            current = (event or {}).get("calendar_id") or next((c["id"] for c in calendars if c["is_default"]), None)
            calendar_pick.set_selected(next((i for i, c in enumerate(calendars) if c["id"] == current), 0))
            content.append(self.cal_field("Calendar", calendar_pick))
        all_day = Gtk.Switch(active=bool((event or {}).get("all_day")))
        content.append(self.cal_field("All day", all_day))
        now = datetime.datetime.now(zone(tz_name) or datetime.timezone.utc)
        start_default = (now + datetime.timedelta(hours=1)).replace(minute=0, second=0, microsecond=0)
        if editing and occ and occ.get("starts_at") and not event["all_day"]:
            occ_tz = zone(event["tzid"]) or datetime.timezone.utc
            s, e = in_zone(occ["starts_at"], occ_tz), in_zone(occ["ends_at"], occ_tz)
            start_value, end_value = s.strftime("%Y-%m-%dT%H:%M"), e.strftime("%Y-%m-%dT%H:%M")
        elif editing and not event["all_day"]:
            start_value, end_value = event["start"] or "", event["end"] or ""
        else:
            start_value = start_default.strftime("%Y-%m-%dT%H:%M")
            end_value = (start_default + datetime.timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M")
        start_date = Gtk.Entry(placeholder_text="YYYY-MM-DD", text=((event or {}).get("start_date") or start_value[:10]))
        end_default = (event or {}).get("end_date")
        if end_default:
            end_default = (datetime.date.fromisoformat(end_default) - datetime.timedelta(days=1)).isoformat()
        end_date = Gtk.Entry(placeholder_text="YYYY-MM-DD", text=end_default or end_value[:10] or start_value[:10])
        start_time = Gtk.Entry(placeholder_text="HH:MM", text=start_value[11:16])
        end_time = Gtk.Entry(placeholder_text="HH:MM", text=end_value[11:16])
        for entry, name in ((start_date, "Start date"), (start_time, "Start time"), (end_date, "End date"), (end_time, "End time")):
            entry.update_property([Gtk.AccessibleProperty.LABEL], [name])
            entry.set_width_chars(11)
        times = box(6, horizontal=True)
        for widget in (label("Starts"), start_date, start_time, label("Ends"), end_date, end_time):
            times.append(widget)
        content.append(times)
        zones = sorted(z for z in available_timezones() if "/" in z and not z.startswith(("Etc/", "SystemV/", "posix/", "right/"))) or ["UTC"]
        if tz_name not in zones:
            zones.insert(0, tz_name)
        zone_pick = Gtk.DropDown.new_from_strings(zones)
        zone_pick.set_enable_search(True)
        zone_pick.set_expression(Gtk.PropertyExpression.new(Gtk.StringObject, None, "string"))
        zone_pick.set_selected(zones.index(tz_name))
        zone_row = self.cal_field("Time zone", zone_pick)
        content.append(zone_row)

        def timed_visible(*_):
            for widget in (start_time, end_time, zone_row):
                widget.set_visible(not all_day.get_active())
        all_day.connect("notify::active", timed_visible)
        timed_visible()
        # Repeat
        rule = {}
        try:
            rule = parse_rrule((event or {}).get("rrule"))
        except ValueError:
            rule = {"custom": True}
        custom = bool(rule) and (rule.get("custom") or any(k in rule for k in ("BYMONTHDAY", "BYMONTH", "BYSETPOS", "WKST"))
                                 or any(len(d) > 2 for d in rule.get("BYDAY", "").split(",") if d))
        repeat_names = ["Does not repeat", "Daily", "Weekly", "Monthly", "Yearly"] + (["Keep the current rule"] if custom else [])
        repeat = Gtk.DropDown.new_from_strings(repeat_names)
        freq_index = {"DAILY": 1, "WEEKLY": 2, "MONTHLY": 3, "YEARLY": 4}
        repeat.set_selected(len(repeat_names) - 1 if custom else freq_index.get(rule.get("FREQ"), 0))
        content.append(self.cal_field("Repeat", repeat))
        repeat_box = box(6)
        interval = Gtk.SpinButton.new_with_range(1, 1000, 1)
        interval.set_value(int(rule.get("INTERVAL", "1")) if not custom else 1)
        repeat_box.append(self.cal_field("Every", interval))
        days_row = box(2, horizontal=True)
        day_toggles = {}
        chosen_days = [d[-2:] for d in rule.get("BYDAY", "").split(",") if d] if not custom else []
        for code, name in zip(WEEKDAYS, ("Mo", "Tu", "We", "Th", "Fr", "Sa", "Su")):
            toggle = Gtk.ToggleButton(label=name, active=code in chosen_days)
            toggle.update_property([Gtk.AccessibleProperty.LABEL], [code])
            day_toggles[code] = toggle
            days_row.append(toggle)
        repeat_box.append(days_row)
        ends = Gtk.DropDown.new_from_strings(["Never", "After a number of times", "On a date"])
        count = Gtk.SpinButton.new_with_range(1, 1000, 1)
        count.set_value(int(rule.get("COUNT", "10")) if not custom else 10)
        until = Gtk.Entry(placeholder_text="YYYY-MM-DD")
        if rule.get("UNTIL") and not custom:
            u = rule["UNTIL"]
            until.set_text(f"{u[:4]}-{u[4:6]}-{u[6:8]}")
        ends.set_selected(1 if rule.get("COUNT") else 2 if rule.get("UNTIL") else 0)
        repeat_box.append(self.cal_field("Ends", ends))
        repeat_box.append(count)
        repeat_box.append(until)
        content.append(repeat_box)

        def repeat_visible(*_):
            index = repeat.get_selected()
            repeat_box.set_visible(0 < index < 5)
            days_row.set_visible(index == 2)
            count.set_visible(ends.get_selected() == 1)
            until.set_visible(ends.get_selected() == 2)
        repeat.connect("notify::selected", repeat_visible)
        ends.connect("notify::selected", repeat_visible)
        repeat_visible()
        location = Gtk.Entry(placeholder_text="Location", text=(event or {}).get("location", ""))
        content.append(location)
        description = Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR)
        description.get_buffer().set_text((event or {}).get("description", ""))
        description.set_size_request(-1, 70)
        description.update_property([Gtk.AccessibleProperty.LABEL], ["Description"])
        content.append(self.cal_field("Description", description))
        guests = Gtk.Entry(placeholder_text="Add guests: /handle, /k/address" + (", or name@example.org" if True else ""))
        guests.update_property([Gtk.AccessibleProperty.LABEL], ["Guests to add, separated by commas"])
        content.append(guests)
        if CALENDAR_EMAIL not in self.features:
            content.append(label("Guests without a Pigeonpost handle get no invitation message yet: after saving, copy their personal link "
                                 "from the web inbox.", "dim-label", wrap=True))
        meeting = reminder = None
        if not editing:
            meeting = Gtk.Switch(active=False)
            content.append(self.cal_field("Scheduled call (opens in the browser)", meeting))
            reminder = Gtk.DropDown.new_from_strings([t for _, t in REMINDER_CHOICES])
            reminder.set_selected(3)
            content.append(self.cal_field("Reminder", reminder))
        scopes = self.cal_scope_picker(event, occ) if editing else None
        if scopes:
            content.append(scopes)
        error = label("", "error", wrap=True)
        content.append(error)
        content.append(label("Calendar data is stored by Pigeonpost and is not end-to-end encrypted.", "dim-label", wrap=True))
        save_button = button("Save" if editing else "Create event", lambda: submit(), style="suggested-action")
        content.append(save_button)

        def collect():
            index = repeat.get_selected()
            rrule = None
            if index == 5:
                rrule = (event or {}).get("rrule")
            elif index:
                freq = ("DAILY", "WEEKLY", "MONTHLY", "YEARLY")[index - 1]
                end_kind = ends.get_selected()
                until_date = None
                if end_kind == 2:
                    try:
                        until_date = datetime.date.fromisoformat(until.get_text().strip())
                    except ValueError:
                        raise CalendarError("Enter the last date as YYYY-MM-DD.") from None
                rrule = build_rrule(freq, int(interval.get_value()), int(count.get_value()) if end_kind == 1 else None, until_date,
                                    [d for d, t in day_toggles.items() if t.get_active()] if freq == "WEEKLY" else None)
            buffer = description.get_buffer()
            people = []
            for token in [t.strip() for t in guests.get_text().split(",") if t.strip()]:
                people.append({"handle": token} if token.startswith("/") else {"contact": token})
            form = {"title": title.get_text(), "description": buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False),
                    "location": location.get_text(), "all_day": all_day.get_active(), "editing": editing,
                    "calendar_id": calendars[calendar_pick.get_selected()]["id"] if calendar_pick and calendars else None,
                    "start_date": start_date.get_text().strip(), "end_date": end_date.get_text().strip(),
                    "start": start_date.get_text().strip() + "T" + start_time.get_text().strip(),
                    "end": end_date.get_text().strip() + "T" + end_time.get_text().strip(),
                    "tzid": zones[zone_pick.get_selected()], "rrule": rrule, "fold": state["fold"], "attendees": people}
            if not editing:
                form["meeting"] = meeting.get_active()
                minutes = REMINDER_CHOICES[reminder.get_selected()][0]
                form["reminders"] = [] if minutes is None else [minutes]
            fields = event_fields(form)
            return fields, people

        def ask_fold(text):
            dialog = Adw.MessageDialog.new(window, "Which time?", text)
            dialog.add_response("cancel", "Cancel")
            dialog.add_response("earlier", "Earlier")
            dialog.add_response("later", "Later")

            def chosen(_, response):
                if response in ("earlier", "later"):
                    state["fold"] = response
                    submit()
            dialog.connect("response", chosen)
            dialog.present()

        def submit():
            error.set_text("")
            try:
                fields, people = collect()
            except CalendarError as problem:
                if problem.code == "time_ambiguous":
                    ask_fold(str(problem))
                else:
                    error.set_text(str(problem))
                return
            save_button.set_sensitive(False)
            if editing:
                self.cal_save_edit(event, occ, fields, people, scopes, state, window, error, save_button)
            else:
                self.cal_create(fields, window, error, save_button)
        window.cal_editor = {"title": title, "all_day": all_day, "start_date": start_date, "start_time": start_time,
                             "end_date": end_date, "end_time": end_time, "zone": zone_pick, "zones": zones, "repeat": repeat,
                             "days": day_toggles, "ends": ends, "count": count, "until": until, "guests": guests,
                             "meeting": meeting, "reminder": reminder, "scopes": scopes, "error": error, "submit": submit,
                             "state": state, "location": location}
        window.present()
        title.grab_focus()
        return window

    @staticmethod
    def cal_field(title, widget):
        row = box(10, horizontal=True)
        text = label(title)
        text.set_hexpand(True)
        row.append(text)
        row.append(widget)
        return row

    def cal_create(self, fields, window, error, save_button):
        """Saved first, then sent once; an uncertain answer stays saved for an explicit retry."""
        try:
            lease = self.cal_write_lease()
        except Retired:
            return
        mailbox = lease.mailbox
        body = create_event_body(fields)
        attempts = Attempts(self.protected)
        client = self.cal_client(lease)

        def create():
            raw = attempts.save(lease, mailbox, "event", body)
            try:
                result = client.post_exact("event", raw)
            except APIError as problem:
                if not uncertain(problem):
                    attempts.remove(lease, mailbox, "event", body["request_key"])
                raise
            attempts.remove(lease, mailbox, "event", body["request_key"])
            return result

        def created(_):
            if mailbox == self.identity:
                window.close()
                self.toast("Event created")
                self.load_calendar()

        def failed(problem):
            if mailbox != self.identity:
                return
            save_button.set_sensitive(True)
            if isinstance(problem, APIError) and uncertain(problem):
                window.close()
                self.notice("The event was not confirmed",
                            "Pigeonpost did not answer. The request is saved: retry it from the calendar page — it is never created twice.")
                self.load_calendar()
                return
            if getattr(problem, "code", "") == "time_invalid" and (problem.body or {}).get("kind") == "ambiguous":
                error.set_text("That time happens twice in this time zone. Save again and choose Earlier or Later.")
                return
            error.set_text(self.cal_error_text(problem) if isinstance(problem, APIError) and not isinstance(problem, CalendarError)
                           else str(problem))
        self.bound_work(lease, create, created, failed)

    def cal_save_edit(self, event, occ, fields, people, scopes, state, window, error, save_button):
        scope = scopes.scope() if scopes else "all"
        try:
            body = patch_body(fields, state["revision"], scope, (occ or {}).get("occurrence_id"))
        except CalendarError as problem:
            error.set_text(str(problem))
            save_button.set_sensitive(True)
            return
        adding = [p for p in people]

        def operation(client):
            result = client.update_event(event["id"], body)
            if adding and scope == "all":
                fresh = client.event(event["id"])["event"]
                client.change_attendees(event["id"], fresh["revision"], add=adding)
            return result

        def saved(_):
            window.close()
            self.toast("Event saved")

        def failed(problem):
            save_button.set_sensitive(True)
            if getattr(problem, "code", "") == "revision_conflict":
                # Reload: the newer version's revision becomes the base; the person reviews and saves again.
                client = self.cal_client()

                def reloaded(detail):
                    state["revision"] = detail["event"]["revision"]
                    error.set_text(self.cal_error_text(problem))
                self._work(lambda: client.event(event["id"], (occ or {}).get("occurrence_id")), reloaded,
                           lambda e: error.set_text(self.cal_error_text(e)))
                return
            if isinstance(problem, APIError) and uncertain(problem):
                error.set_text("Pigeonpost did not answer. Refresh the calendar to see whether the change was saved before trying again.")
                return
            error.set_text(self.cal_error_text(problem))
        if adding and scope != "all":
            error.set_text("Guests can only be added to all events of a series.")
            save_button.set_sensitive(True)
            return
        self.cal_write(operation, saved, failed)

    # ---- recovery -------------------------------------------------------------------------------

    def cal_retry(self, attempt):
        """Exactly the saved bytes and request key, under the login and mailbox that saved them."""
        try:
            lease = self.cal_write_lease()
        except Retired:
            return
        mailbox, attempts = lease.mailbox, Attempts(self.protected)
        client = self.cal_client(lease)

        def retry():
            saved = next((a for a in attempts.pending(lease, mailbox)
                          if a["kind"] == attempt["kind"] and a["request_key"] == attempt["request_key"]), None)
            if saved is None or saved["wire"] != attempt["wire"]:
                raise CalendarError("This saved change was already resolved.", "attempt_conflict")
            try:
                client.post_exact(saved["kind"], saved["wire"])
            except APIError as problem:
                if not uncertain(problem):
                    attempts.remove(lease, mailbox, saved["kind"], saved["request_key"])
                raise
            attempts.remove(lease, mailbox, saved["kind"], saved["request_key"])

        def done(_):
            if mailbox == self.identity:
                self.toast("Confirmed")
                self.load_calendar()

        def failed(problem):
            if mailbox != self.identity:
                return
            if isinstance(problem, APIError) and uncertain(problem):
                self.toast("Still not confirmed. The request stays saved; try again later.")
            else:
                self.notice("The saved change was refused", self.cal_error_text(problem) if not isinstance(problem, CalendarError) else str(problem))
            self.load_calendar()
        self.bound_work(lease, retry, done, failed)

    def cal_discard(self, attempt):
        def discard():
            try:
                lease = self.cal_write_lease()
            except Retired:
                return
            mailbox, attempts = lease.mailbox, Attempts(self.protected)
            self.bound_work(lease, lambda: attempts.remove(lease, mailbox, attempt["kind"], attempt["request_key"]),
                            lambda _: self.load_calendar() if mailbox == self.identity else None)
        self.confirm("Discard this saved change?", "It may already have been created. Discarding only forgets it on this computer; "
                     "check your calendar afterwards.", discard, "Discard")

    # ---- invitations -----------------------------------------------------------------------------

    def cal_invitations_dialog(self):
        window, content = self.dialog("Invitations", 560)
        tz = self.cal_zone()
        for heading, rows in (("Waiting for your answer", self.cal["invitations"]),
                              ("Filtered", self.cal["filtered_invitations"])):
            content.append(label(heading, "heading"))
            if heading == "Filtered" and rows:
                content.append(label("From people your filters caught. Answering moves it out of Filtered; it does not add a contact.",
                                     "dim-label", wrap=True))
            if not rows:
                content.append(label("None.", "dim-label"))
            for inv in rows:
                row = box(6, horizontal=True)
                shown = dict(inv, end_date=None)
                text = label(f"{inv['title']}\n{when_text(shown, tz)} · from {display_name(inv['organizer'])}", wrap=True)
                text.set_hexpand(True)
                row.append(text)
                row.append(button("Open", lambda i=inv: (window.close(), self.cal_open_event(i["event_id"], i["occurrence_id"]))))
                for status, title in (("accepted", "Yes"), ("tentative", "Maybe"), ("declined", "No")):
                    row.append(button(title, lambda s=status, i=inv: self.cal_rsvp(i["event_id"], s, None, False, window)))
                content.append(row)
        window.present()
        return window

    # ---- import ------------------------------------------------------------------------------

    def cal_import(self):
        if not self.calendar_on():
            return
        chooser = Gtk.FileChooserNative.new("Import calendar file", self, Gtk.FileChooserAction.OPEN, "Open", "Cancel")
        ics_filter = Gtk.FileFilter()
        ics_filter.set_name("Calendar files (.ics)")
        ics_filter.add_pattern("*.ics")
        ics_filter.add_mime_type("text/calendar")
        chooser.add_filter(ics_filter)

        def chosen(dialog, response):
            file = dialog.get_file() if response == Gtk.ResponseType.ACCEPT else None
            dialog.destroy()
            if file:
                self.cal_import_path(file.get_path())
        chooser.connect("response", chosen)
        chooser.show()

    def cal_import_path(self, path):
        client, version = self.cal_client(), self.cal["version"]

        def read():
            with open(path, "rb") as stream:
                data = stream.read(MAX_IMPORT + 1)
            if len(data) > MAX_IMPORT:
                raise CalendarError("Choose an .ics file of at most 512 KiB.")
            return client.import_preview(data)

        def previewed(preview):
            if version == self.cal["version"]:
                self.cal_import_dialog(preview)
        self._work(read, previewed, lambda e: self.notice("Could not read that file", str(e) if isinstance(e, CalendarError) else self.cal_error_text(e)))

    def cal_import_dialog(self, preview):
        window, content = self.dialog("Import events", 560)
        content.append(label("Private copies are added to your calendar. Nothing is fetched from the network and no invitations "
                             "are sent. Alarms, attendees and organizers in the file are left out.", wrap=True))
        checks = []
        for row in preview["events"]:
            when = row["start_date"] if row["all_day"] else row["start"].replace("T", " ")
            check = Gtk.CheckButton(label=f"{row['title']} · {when}" + (" · repeats" if row["rrule"] else ""), active=True)
            checks.append((row["index"], check))
            content.append(check)
            for warning in row["warnings"]:
                content.append(label("  " + warning, "dim-label", wrap=True))
        for warning in preview["warnings"]:
            content.append(label(warning, "dim-label", wrap=True))
        calendars = self.cal["calendars"]
        pick = None
        if calendars:
            pick = Gtk.DropDown.new_from_strings([c["name"] for c in calendars])
            pick.set_selected(next((i for i, c in enumerate(calendars) if c["is_default"]), 0))
            content.append(self.cal_field("Add to", pick))
        error = label("", "error", wrap=True)
        content.append(error)
        state = {"selection": None}

        def confirm():
            selection = (tuple(sorted(i for i, c in checks if c.get_active())), calendars[pick.get_selected()]["id"] if pick else None)
            if state["selection"] is not None and selection != state["selection"]:
                error.set_text("This import was already sent with another choice. Close it and import the file again.")
                return
            state["selection"] = selection  # a retry repeats exactly this selection and destination
            self.cal_write(lambda c: c.import_confirm(preview["draft_id"], selection[0], selection[1]),
                           lambda n: (window.close(), self.toast(f"Imported {n} {'event' if n == 1 else 'events'}")),
                           lambda e: error.set_text(
                               "Pigeonpost did not answer. Import again: the same choice is never added twice."
                               if isinstance(e, APIError) and uncertain(e) else (str(e) if isinstance(e, CalendarError) else self.cal_error_text(e))))
        content.append(button("Import", confirm, style="suggested-action"))
        window.cal_import = {"checks": checks, "confirm": confirm, "error": error}
        window.present()
        return window

    # ---- calendars and feeds --------------------------------------------------------------------

    def cal_calendars_dialog(self):
        if not self.calendar_on():
            return
        window, content = self.dialog("Calendars and feeds", 560)
        for c in self.cal["calendars"]:
            row = box(6, horizontal=True)
            name = label(c["name"] + (" · default" if c["is_default"] else ""), "heading")
            name.add_css_class("pp-cal-dot-" + c["color"])
            name.set_hexpand(True)
            row.append(name)
            colors = Gtk.DropDown.new_from_strings(list(COLORS))
            colors.set_selected(COLORS.index(c["color"]))
            colors.update_property([Gtk.AccessibleProperty.LABEL], ["Color of " + c["name"]])
            colors.connect("notify::selected", lambda d, _p, cal=c: self.cal_write(
                lambda client: client.update_calendar(cal["id"], cal["revision"], color=COLORS[d.get_selected()]),
                lambda _: self.toast("Color saved")) if COLORS[d.get_selected()] != cal["color"] else None)
            row.append(colors)
            row.append(button("Rename", lambda cal=c: self.form_dialog("Rename calendar", "Name", lambda value, cal=cal: self.cal_write(
                lambda client: client.update_calendar(cal["id"], cal["revision"], name=value), lambda _: self.toast("Renamed")), cal["name"])))
            if not c["is_default"]:
                row.append(button("Make default", lambda cal=c: self.cal_write(
                    lambda client: client.update_calendar(cal["id"], cal["revision"], is_default=True), lambda _: self.toast("Default calendar changed"))))
            row.append(button("Feed…", lambda cal=c: self.cal_feed_dialog(cal)))
            row.append(button("Delete " + c["name"], lambda cal=c: self.cal_delete_calendar(cal, window), "user-trash-symbolic", "flat"))
            content.append(row)
        content.append(label("New calendar", "heading"))
        name = Gtk.Entry(placeholder_text="Name")
        content.append(name)
        color = Gtk.DropDown.new_from_strings(list(COLORS))
        content.append(self.cal_field("Color", color))
        error = label("", "error", wrap=True)
        content.append(error)

        def create():
            value = name.get_text().strip()
            if not value or len(value) > 80:
                error.set_text("Name the calendar (up to 80 characters).")
                return
            self.cal_create_calendar({"request_key": None, "name": value, "color": COLORS[color.get_selected()]}, window, error)
        content.append(button("Create calendar", create, style="suggested-action"))
        window.present()
        return window

    def cal_create_calendar(self, body, window, error):
        from .calendar_core import new_request_key
        try:
            lease = self.cal_write_lease()
        except Retired:
            return
        mailbox, attempts, client = lease.mailbox, Attempts(self.protected), self.cal_client(lease)
        body = dict(body, request_key=body.get("request_key") or new_request_key())

        def create():
            raw = attempts.save(lease, mailbox, "calendar", body)
            try:
                result = client.post_exact("calendar", raw)
            except APIError as problem:
                if not uncertain(problem):
                    attempts.remove(lease, mailbox, "calendar", body["request_key"])
                raise
            attempts.remove(lease, mailbox, "calendar", body["request_key"])
            return result

        def failed(problem):
            if mailbox != self.identity:
                return
            if isinstance(problem, APIError) and uncertain(problem):
                window.close()
                self.notice("The calendar was not confirmed", "The request is saved: retry it from the calendar page.")
                self.load_calendar()
            else:
                error.set_text(str(problem) if isinstance(problem, CalendarError) else self.cal_error_text(problem))
        self.bound_work(lease, create, lambda _: (window.close(), self.toast("Calendar created"), self.load_calendar())
                        if mailbox == self.identity else None, failed)

    def cal_delete_calendar(self, calendar, parent):
        self.confirm(f"Delete {calendar['name']}?",
                     "Events you organize in it are cancelled and their guests are told. Invitations in it move to your default calendar.",
                     lambda: self.cal_write(lambda c: c.delete_calendar(calendar["id"], calendar["revision"]),
                                            lambda _: (parent.close(), self.toast("Calendar deleted"))))

    def cal_feed_dialog(self, calendar):
        window, content = self.dialog("Calendar feed", 520)
        content.append(label(f"Subscribe to “{calendar['name']}” from another calendar app. Anyone with the link can read what it shows.", wrap=True))
        status = label("Checking…", "dim-label", wrap=True)
        content.append(status)
        projection = Gtk.DropDown.new_from_strings(["Busy times only", "My titles and locations (private events as Busy)"])
        content.append(self.cal_field("Shows", projection))
        link_box = box(6)
        content.append(link_box)
        client = self.cal_client()

        def show_status(info):
            when = in_zone(info["expires_at"], self.cal_zone()).strftime("%d %b %Y") if info["expires_at"] else "—"
            status.set_text(f"On · {info['projection'].replace('_', ' ')} · expires {when}" if info["enabled"] else "Off")
        self._work(lambda: client.feed(calendar["id"]), show_status, lambda e: status.set_text(self.cal_error_text(e)))

        def enable():
            chosen = ("busy", "personal_redacted")[projection.get_selected()]

            def shown(result):
                clear(link_box)
                link_box.append(label("Copy this link now: it is shown only once. Making a new one stops the old link.", "heading", wrap=True))
                text = label(result["url"], "pp-mono", wrap=True)
                text.set_selectable(True)
                link_box.append(text)
                link_box.append(button("Copy link", lambda: (self.get_clipboard().set(result["url"]), self.toast("Feed link copied"))))
                status.set_text("On")
            self.cal_write(lambda c: c.set_feed(calendar["id"], True, chosen), shown, reload=False)

        def revoke():
            self.cal_write(lambda c: c.set_feed(calendar["id"], False), lambda _: (clear(link_box), status.set_text("Off"),
                                                                                   self.toast("Feed link stopped")), reload=False)
        content.append(button("Make a new feed link", enable, style="suggested-action"))
        content.append(button("Stop the feed", revoke, style="destructive-action"))
        window.present()
        return window

    # ---- settings -------------------------------------------------------------------------------

    def cal_settings_dialog(self):
        if not self.calendar_on():
            return
        settings = self.cal.get("settings") or {"previews": False, "timezone": None, "week_start": 1}
        window, content = self.dialog("Calendar settings", 480)
        zones = ["Use this computer’s time zone"] + sorted(z for z in available_timezones() if "/" in z and not z.startswith(("Etc/", "posix/", "right/")))
        zone_pick = Gtk.DropDown.new_from_strings(zones)
        zone_pick.set_enable_search(True)
        zone_pick.set_expression(Gtk.PropertyExpression.new(Gtk.StringObject, None, "string"))
        zone_pick.set_selected(zones.index(settings["timezone"]) if settings.get("timezone") in zones else 0)
        content.append(self.cal_field("Time zone", zone_pick))
        week = Gtk.DropDown.new_from_strings(["Monday", "Saturday", "Sunday"])
        week.set_selected({1: 0, 6: 1, 7: 2}.get(settings.get("week_start"), 0))
        content.append(self.cal_field("Week starts on", week))
        previews = Gtk.Switch(active=bool(settings.get("previews")))
        content.append(self.cal_field("Show event titles in reminders", previews))
        content.append(label("Off: reminders only say an event is starting soon.", "dim-label", wrap=True))
        error = label("", "error", wrap=True)
        content.append(error)

        def save():
            chosen = {"timezone": None if zone_pick.get_selected() == 0 else zones[zone_pick.get_selected()],
                      "week_start": (1, 6, 7)[week.get_selected()], "previews": previews.get_active()}
            self.cal_write(lambda c: c.save_settings(chosen), lambda _: (window.close(), self.toast("Calendar settings saved")),
                           lambda e: error.set_text(str(e) if isinstance(e, CalendarError) else self.cal_error_text(e)))
        content.append(button("Save", save, style="suggested-action"))
        window.present()
        return window

    # ---- reminders ------------------------------------------------------------------------------

    def calendar_tick(self):
        """Every few seconds: keep the next day's occurrences and their reminders, and show the due
        ones as desktop notifications. Only the selected mailbox; only while Pigeonpost runs."""
        if not self.calendar_on():
            return
        now = time.time()
        if now - self.cal["upcoming_at"] >= UPCOMING_SECONDS and not self.cal.get("upcoming_loading"):
            self.cal["upcoming_at"] = now
            self.cal_load_upcoming()
        known = {k: v[1] for k, v in self.cal["reminders"].items()}
        # Only events whose own reminders were read: never a guessed default.
        upcoming = [o for o in self.cal["upcoming"] if o["event_id"] in known]
        due = due_reminders(upcoming, known, self.cal["fired"], now, horizon=0)
        previews = bool((self.cal.get("settings") or {}).get("previews"))
        for key, o, minutes, _ in due:
            self.cal["fired"].add(key)
            title, body = reminder_text(o, minutes, previews)
            notification = Gio.Notification.new(title)
            notification.set_body(body)
            notification.set_default_action_and_target("app.open-calendar", GLib.Variant("s", self.identity))
            app = self.get_application()
            if app:
                app.send_notification("calendar-" + key.replace(":", "-"), notification)

    def cal_load_upcoming(self):
        client, version = self.cal_client(), self.cal["version"]
        known = dict(self.cal["reminders"])
        self.cal["upcoming_loading"] = True

        def read():
            now = int(time.time())
            occurrences = client.occurrences(now - 600, now + 26 * 3600)
            settings = client.settings()
            reminders = {}
            wanted = [o for o in occurrences if not o["all_day"] and o["status"] == "confirmed"]
            for o in wanted[:20]:
                cached = known.get(o["event_id"])
                if cached and cached[0] == o["revision"]:
                    continue
                try:
                    detail = client.event(o["event_id"])
                    reminders[o["event_id"]] = (o["revision"], [r["minutes_before"] for r in detail["event"]["reminders"]])
                except APIError:
                    pass
            return occurrences, settings, reminders

        def loaded(result):
            self.cal["upcoming_loading"] = False
            if version != self.cal["version"]:
                return
            occurrences, settings, reminders = result
            self.cal["upcoming"] = occurrences
            if self.cal.get("settings") is None:
                self.cal["settings"] = settings
            else:
                self.cal["settings"]["previews"] = settings["previews"]
            self.cal["reminders"].update(reminders)

        def failed(_):
            self.cal["upcoming_loading"] = False
        self._work(read, loaded, failed)
