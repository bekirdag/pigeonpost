using System.Net;
using System.Text;
using System.Text.Json;
using NodaTime;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

// Audit P2-18 / P3-7 on Windows: the week/day time grid and now line, join-before up to 30 minutes, feeds, .ics
// import, the guest invitation link, and calendar toasts parsed from /v1/events.
internal static class CalendarWindowsGapTests
{
    private const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa";
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        var zone = CalendarTime.Zone("Europe/Istanbul");
        long At(string local) => new CalendarWallTime(local, "Europe/Istanbul").Resolve();
        CalendarOccurrence Row(string id, string start, string end, string? title = null) =>
            new("occ_" + id, "evt_" + id, AllDay: false, StartsAt: At(start), EndsAt: At(end), Title: title ?? id);

        yield return ("The week grid starts on the chosen weekday and the day view shows only the selected day", () =>
        {
            var wednesday = CalendarDay.Parse("2026-11-04");
            var monday = CalendarTimeGrid.Days(CalendarView.Week, wednesday, 1);
            Require(monday.Count == 7 && monday[0].Text == "2026-11-02" && monday[6].Text == "2026-11-08", "Monday week");
            Require(CalendarTimeGrid.Days(CalendarView.Week, wednesday, 0)[0].Text == "2026-11-01", "Sunday week");
            Require(CalendarTimeGrid.Days(CalendarView.Week, CalendarDay.Parse("2026-11-01"), 1)[0].Text == "2026-10-26", "a Sunday belongs to the week before");
            Require(CalendarTimeGrid.Days(CalendarView.Day, wednesday, 1).Single() == wednesday, "day view");
            return Task.CompletedTask;
        });
        yield return ("Overlapping events share their column side by side, and a multi-day event is split per day", () =>
        {
            var days = CalendarTimeGrid.Days(CalendarView.Week, CalendarDay.Parse("2026-11-04"), 1);
            var blocks = CalendarTimeGrid.Layout([
                Row("a", "2026-11-04T09:00", "2026-11-04T10:00"), Row("b", "2026-11-04T09:30", "2026-11-04T11:00"),
                Row("c", "2026-11-04T10:30", "2026-11-04T11:30"), Row("d", "2026-11-04T13:00", "2026-11-04T13:05"),
                Row("night", "2026-11-05T22:00", "2026-11-06T02:00"),
                new("occ_all", "evt_all", AllDay: true, StartDate: "2026-11-04", EndDate: "2026-11-05", Title: "Holiday")], days, zone);
            var a = blocks.Single(b => b.Row.EventId == "evt_a"); var bb = blocks.Single(b => b.Row.EventId == "evt_b");
            var c = blocks.Single(b => b.Row.EventId == "evt_c"); var d = blocks.Single(b => b.Row.EventId == "evt_d");
            Require(a.Day == 2 && a.StartMinute == 540 && a.EndMinute == 600 && a.Lane == 0 && a.Lanes == 2, "a");
            Require(bb.Lane == 1 && bb.Lanes == 2 && c.Lane == 0 && c.Lanes == 2, "b and c share the cluster");
            Require(d.Lanes == 1 && d.EndMinute - d.StartMinute == CalendarTimeGrid.MinimumMinutes, "short events stay tappable");
            var night = blocks.Where(b => b.Row.EventId == "evt_night").OrderBy(b => b.Day).ToArray();
            Require(night.Length == 2 && night[0] is { Day: 3, StartMinute: 1320, EndMinute: 1440, ContinuesAfter: true }
                && night[1] is { Day: 4, StartMinute: 0, EndMinute: 120, ContinuesBefore: true }, "split across midnight");
            Require(blocks.All(b => b.Row.AllDay != true) && CalendarTimeGrid.AllDay([new("occ_all", "evt_all", AllDay: true, StartDate: "2026-11-04", EndDate: "2026-11-05")], days[2], zone).Count == 1, "all-day events stay in the all-day row");
            return Task.CompletedTask;
        });
        yield return ("The now line appears only on today's column, at the local minute", () =>
        {
            var days = CalendarTimeGrid.Days(CalendarView.Week, CalendarDay.Parse("2026-11-04"), 1);
            var now = Instant.FromUnixTimeSeconds(At("2026-11-04T14:30"));
            Require(CalendarTimeGrid.Now(now, days, zone) == (2, 870.0), "today");
            Require(CalendarTimeGrid.Now(now, CalendarTimeGrid.Days(CalendarView.Day, CalendarDay.Parse("2026-11-05"), 1), zone) is null, "not shown on another day");
            Require(CalendarTimeGrid.FirstMinute([], null) == 480 && CalendarTimeGrid.FirstMinute([], (2, 870)) == 480, "empty days open at 08:00");
            Require(CalendarTimeGrid.FirstMinute(CalendarTimeGrid.Layout([Row("e", "2026-11-04T06:00", "2026-11-04T07:00")], days, zone), null) == 300, "an early event scrolls into view");
            return Task.CompletedTask;
        });
        yield return ("A meeting can open up to 30 minutes before it starts, as the postbox allows", () =>
        {
            CalendarEventDraft Draft(int minutes) => new(Guid.NewGuid(), "2026-11-04", "2026-11-04", "Europe/Istanbul")
                { Title = "Meeting", Meeting = new(Enabled: true, JoinBeforeMinutes: minutes) };
            Require(Draft(30).Fields().Fields().GetProperty("meeting").GetProperty("join_before_minutes").GetInt32() == 30, "30 accepted");
            Fails<ArgumentException>(() => Draft(31).Fields());
            return Task.CompletedTask;
        });
        yield return ("Feed links are person-requested, show only the chosen projection and are checked on the way back", async () =>
        {
            using var f = new Fixture();
            f.Handler.Reply = """{"enabled":true,"projection":"busy","url":"https://postbox.pigeonpost.dev/v1/calendar/feed/sec_1.ics","expires_at":99}""";
            var feed = await f.Api.SetCalendarFeedAsync(Own, "cal_work", true, "busy", default);
            Require(feed.Url!.EndsWith(".ics") && f.Handler.Last!.Path == "/v1/calendar/feeds/cal_work" && f.Handler.Last.Body!.Contains("\"projection\":\"busy\""), "turn on");
            Require(!feed.ToString().Contains("sec_1"), "feed secrets are not printed");
            f.Handler.Reply = """{"enabled":true,"projection":"busy","url":"https://evil.example/steal","expires_at":99}""";
            Require((await Error<PostboxException>(() => f.Api.SetCalendarFeedAsync(Own, "cal_work", true, "busy", default))).Code == "bad_response", "foreign link");
            f.Handler.Reply = """{"enabled":false}""";
            Require(!(await f.Api.SetCalendarFeedAsync(Own, "cal_work", false, null, default)).Enabled && f.Handler.Last!.Body == "{\"enabled\":false}", "turn off");
            f.Handler.Reply = """{"enabled":true,"projection":"personal_redacted","created_at":1,"expires_at":2}""";
            Require((await f.Api.GetCalendarFeedAsync(Own, "cal_work", default)).Projection == "personal_redacted" && f.Handler.Last!.Method == "GET", "read");
            await Error<ArgumentException>(() => f.Api.SetCalendarFeedAsync(Own, "cal_work", true, "everything", default));
        });
        yield return ("An .ics import is previewed as raw text/calendar and only the ticked entries are confirmed", async () =>
        {
            using var f = new Fixture();
            f.Handler.Reply = """{"draft_id":"imp_1","calendar_id":"cal_work","events":[{"index":0,"title":"Standup","all_day":false,"start":"2026-11-04T09:00","tzid":"Europe/Istanbul","rrule":"FREQ=DAILY","warnings":[],"importable":true},{"index":1,"title":"Trip","all_day":true,"start_date":"2026-11-10","end_date":"2026-11-13","warnings":["Attachments are skipped."],"importable":true}],"warnings":[]}""";
            var preview = await f.Api.PreviewCalendarImportAsync(Own, "cal_work", Encoding.UTF8.GetBytes("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"), default);
            Require(f.Handler.Last!.ContentType == "text/calendar" && f.Handler.Last.Path == "/v1/calendar/import/preview" && f.Handler.Last.Query.Contains("calendar_id=cal_work"), "raw body");
            Require(preview.Events[0].Label == "Standup — 2026-11-04 09:00 Europe/Istanbul · repeats" && preview.Events[1].Label == "Trip — 2026-11-10 – 2026-11-12", "labels");
            f.Handler.Reply = """{"created":[{"event_id":"evt_1"}]}""";
            Require(await f.Api.ConfirmCalendarImportAsync(Own, "imp_1", [1], "cal_work", default) == 1 && f.Handler.Last!.Path == "/v1/calendar/import/imp_1/confirm"
                && f.Handler.Last.Body == "{\"select\":[1],\"calendar_id\":\"cal_work\"}", "confirm");
            await Error<ArgumentException>(() => f.Api.PreviewCalendarImportAsync(Own, null, new byte[CalendarSharing.MaxImportBytes + 1], default));
            await Error<ArgumentException>(() => f.Api.ConfirmCalendarImportAsync(Own, "imp_1", [], null, default));
        });
        yield return ("A guest's own invitation link comes only from the meeting page origin", async () =>
        {
            using var f = new Fixture();
            f.Handler.Reply = """{"url":"https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=tok","expires_at":5}""";
            Require((await f.Api.CalendarGuestLinkAsync(Own, "evt_1", "att_2", default)).Url.Contains("/m/ABCDEFGHJK") && f.Handler.Last!.Path == "/v1/calendar/events/evt_1/attendees/att_2/link", "link");
            f.Handler.Reply = """{"url":"javascript:alert(1)"}""";
            Require((await Error<PostboxException>(() => f.Api.CalendarGuestLinkAsync(Own, "evt_1", "att_2", default))).Code == "bad_response", "unsafe link");
            await Error<ArgumentException>(() => f.Api.CalendarGuestLinkAsync(Own, "evt_1", "../x", default));
        });
        yield return ("Calendar account events become invitation and reminder alerts with locators only", () =>
        {
            var invite = CalendarAlerts.Parse("event.invite", $$$"""{"event_id":"c1","type":"event.invite","mailbox":"{{{Own}}}","data":{"event_id":"evt_1","revision":2,"filtered":false,"occurrence_id":null}}""");
            Require(invite is { Kind: CalendarAlertKind.Invite, EventId: "evt_1", OccurrenceId: null, Filtered: false } && invite.Mailbox == Own, "invite");
            var reminder = CalendarAlerts.Parse("event.reminder", $$$"""{"mailbox":"{{{Own}}}","data":{"event_id":"evt_1","occurrence_id":"occ_9","reminder_id":"rem_1","starts_at":1000}}""");
            Require(reminder is { Kind: CalendarAlertKind.Reminder, OccurrenceId: "occ_9", StartsAt: 1000 }, "reminder");
            Require(CalendarAlerts.Parse("event.updated", "{}") is null && CalendarAlerts.Parse("event.invite", "{\"mailbox\":\"/bekir\",\"data\":{\"event_id\":\"evt_1\"}}") is null
                && CalendarAlerts.Parse("event.invite", "{not json") is null && CalendarAlerts.Parse("event.invite", $$$"""{"mailbox":"{{{Own}}}","data":{"event_id":"../x"}}""") is null, "refusals");
            var now = DateTimeOffset.FromUnixTimeSeconds(1000 - 600);
            Require(CalendarAlerts.ReminderText("Design review", true, 1000, now) == "Design review: Starting in 10 minutes", "preview on");
            Require(CalendarAlerts.ReminderText("Design review", false, 1000, now) == "Event: Starting in 10 minutes", "preview off");
            Require(CalendarAlerts.ReminderText(null, true, 990, DateTimeOffset.FromUnixTimeSeconds(1000)) == "Event: Starting now", "now");
            ScheduledEvent Ev(string status, string role, string rsvp) => new("evt_1", Status: status, My: new(Role: role, AttendeeId: "att_1", Rsvp: rsvp));
            Require(CalendarAlerts.Answerable(new(Ev("confirmed", "attendee", "pending"))) && !CalendarAlerts.Answerable(new(Ev("confirmed", "attendee", "accepted")))
                && !CalendarAlerts.Answerable(new(Ev("cancelled", "attendee", "pending"))) && !CalendarAlerts.Answerable(new(Ev("confirmed", "organizer", "pending"))), "answerable");
            return Task.CompletedTask;
        });
    }

    private static void Require(bool condition, string why) { if (!condition) throw new Exception("Calendar Windows gaps: " + why); }
    private static void Fails<T>(Action action) where T : Exception { try { action(); } catch (T) { return; } throw new Exception("Expected refusal."); }
    private static async Task<T> Error<T>(Func<Task> action) where T : Exception { try { await action(); } catch (T e) { return e; } throw new Exception("Expected " + typeof(T).Name); }

    private sealed record Seen(string Method, string Path, string Query, string? Body, string? ContentType);
    private sealed class Handler : HttpMessageHandler
    {
        internal string Reply = "{}";
        internal Seen? Last;
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            var uri = request.RequestUri!;
            if (uri.AbsolutePath == "/v1/capabilities")
                return new(HttpStatusCode.OK) { Content = new StringContent("{\"features\":[\"calendar\"]}", Encoding.UTF8, "application/json") };
            Last = new(request.Method.Method, uri.AbsolutePath, uri.Query, request.Content is null ? null : await request.Content.ReadAsStringAsync(ct),
                request.Content?.Headers.ContentType?.MediaType);
            return new(HttpStatusCode.OK) { Content = new StringContent(Reply, Encoding.UTF8, "application/json") };
        }
    }
    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; } = new();
        public Task<string> GetTokenAsync(CancellationToken ct) => Task.FromResult("calendar-gap-test");
        public Task<string> RefreshTokenAsync(CancellationToken ct) => GetTokenAsync(ct);
    }
    private sealed class Fixture : IDisposable
    {
        internal readonly Handler Handler = new();
        internal readonly PostboxClient Api;
        internal Fixture() { var http = new HttpClient(Handler); Api = new PostboxClient(http, new Tokens(), new Uri("http://127.0.0.1:9"), http); }
        public void Dispose() => Api.Dispose();
    }
}
