using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

internal static class CalendarApiTests
{
    private const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa";
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Calendar reads use the original canonical mailbox in the header and query without creating a default", async () =>
        {
            using var f = new Fixture(); var folders = await f.Api.GetCalendarsAsync(Own);
            Require(folders.Count == 0 && f.Handler.Calls.All(c => c.Method == "GET") && f.Handler.Routes.Single().Identity == Own
                && Uri.UnescapeDataString(f.Handler.Routes.Single().Query).Contains("identity=" + Own, StringComparison.Ordinal));
        });
        yield return ("Calendar occurrence windows preserve selected filters and include pending invitations", async () =>
        {
            using var f = new Fixture(); f.Handler.Occurrences = "{\"occurrences\":[{\"occurrence_id\":\"occ_original_key\",\"event_id\":\"evt_fixture\",\"title\":\"Moved\",\"role\":\"future_role\"}]}";
            var rows = await f.Api.GetCalendarOccurrencesAsync(Own, 100, 200, ["cal_a", "cal_b"]);
            var request = f.Handler.Routes.Single(); var query = Uri.UnescapeDataString(request.Query);
            Require(rows.Single().Key.OccurrenceId == "occ_original_key" && rows.Single().Role == "future_role"
                && query.Contains("include=invitations", StringComparison.Ordinal) && query.Contains("calendars=cal_a,cal_b", StringComparison.Ordinal));
        });
        yield return ("Calendar details reject a substituted event or missing/substituted selected occurrence", async () =>
        {
            using var f = new Fixture();
            foreach (var body in new[] { "{\"event\":{\"id\":\"evt_other\"}}", "{\"event\":{\"id\":\"evt_fixture\"}}",
                "{\"event\":{\"id\":\"evt_fixture\"},\"occurrence\":{\"occurrence_id\":\"occ_other\"}}" })
            {
                f.Handler.Detail = body;
                Require((await Error<PostboxException>(() => f.Api.GetCalendarEventAsync(Own, new("evt_fixture", "occ_original_key")))).Code == "bad_response");
            }
            f.Handler.Detail = "{\"event\":{\"id\":\"evt_fixture\",\"can_edit\":false},\"occurrence\":{\"occurrence_id\":\"occ_original_key\"}}";
            var result = await f.Api.GetCalendarEventAsync(Own, new("evt_fixture", "occ_original_key"));
            Require(result.Event.CanEdit == false && result.Occurrence?.OccurrenceId == "occ_original_key");
        });
        yield return ("Disabled and older calendar servers never receive a calendar route request", async () =>
        {
            foreach (var older in new[] { false, true })
            {
                using var f = new Fixture(); f.Handler.Enabled = false; f.Handler.Older = older;
                Require((await Error<PostboxException>(() => f.Api.GetCalendarsAsync(Own))).Code == "feature_disabled" && f.Handler.Routes.Count == 0);
            }
        });
        yield return ("Calendar API rejects unsafe identifiers, aliases and overflowing windows before any request", async () =>
        {
            using var f = new Fixture();
            await Error<ArgumentException>(() => f.Api.GetCalendarsAsync("/fixture/alias"));
            await Error<ArgumentException>(() => f.Api.GetCalendarEventAsync(Own, new("../secrets")));
            await Error<ArgumentException>(() => f.Api.GetCalendarEventAsync(Own, new("evt_fixture", "occ_a&identity=other")));
            foreach (var range in new[] { (0L, 0L), (100L, 99L), (0L, 62L * 86400 + 1), (long.MinValue, long.MaxValue), (long.MaxValue, long.MinValue) })
                await Error<ArgumentException>(() => f.Api.GetCalendarOccurrencesAsync(Own, range.Item1, range.Item2));
            Require(f.Handler.Calls.Count == 0);
        });
        yield return ("Calendar invitation reads keep pending and filtered lists distinct and use no contact mutation", async () =>
        {
            using var f = new Fixture(); await f.Api.GetCalendarInvitationsAsync(Own); await f.Api.GetCalendarInvitationsAsync(Own, filtered: true);
            Require(f.Handler.Routes.Count == 2 && f.Handler.Routes[0].Query.Contains("state=pending", StringComparison.Ordinal)
                && f.Handler.Routes[1].Query.Contains("state=filtered", StringComparison.Ordinal) && f.Handler.Calls.All(c => c.Method == "GET"));
        });
        yield return ("A calendar read retains every original caller through delayed token acquisition", async () =>
        {
            using var f = new Fixture(); var caller = f.Tokens.Root.Child(); f.Tokens.BlockAt = 2; Task<IReadOnlyList<CalendarFolder>> read;
            using (new AuthorizationContext(caller)) read = f.Api.GetCalendarsAsync(Own);
            await f.Tokens.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); f.Tokens.Release.TrySetResult();
            await Error<AuthorizationRetiredException>(async () => await read); Require(f.Tokens.Root.IsActive && f.Handler.Routes.Count == 0);
        });
        yield return ("Calendar token acquisition cannot adopt an unretired provider replacement", async () =>
        {
            using var f = new Fixture(); var original = f.Tokens.Root; f.Tokens.BlockAt = 2; var read = f.Api.GetCalendarsAsync(Own);
            await f.Tokens.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); f.Tokens.Root = new(); f.Tokens.Release.TrySetResult();
            await Error<AuthorizationRetiredException>(async () => await read); Require(original.IsActive && f.Handler.Routes.Count == 0); original.Retire();
        });
        yield return ("A delayed calendar response cannot escape into an unretired replacement account", async () =>
        {
            using var f = new Fixture(); f.Handler.BlockRoute = true; var original = f.Tokens.Root; var read = f.Api.GetCalendarsAsync(Own);
            await f.Handler.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); f.Tokens.Root = new(); f.Handler.Release.TrySetResult();
            await Error<AuthorizationRetiredException>(async () => await read); Require(original.IsActive && f.Handler.Routes.Count == 1); original.Retire();
        });
        yield return ("Calendar reads survive ordinary same-owner token renewal with the original mailbox", async () =>
        {
            using var f = new Fixture(); f.Handler.UnauthorizedOnce = true; await f.Api.GetCalendarSettingsAsync(Own);
            Require(f.Tokens.Refreshes == 1 && f.Handler.Routes.Count == 2 && f.Handler.Routes.All(c => c.Identity == Own && c.Method == "GET"));
        });
        yield return ("Calendar collections refuse malformed or duplicate identities rather than substituting details", async () =>
        {
            using var f = new Fixture(); f.Handler.Folders = "{\"calendars\":[{\"id\":\"cal_same\"},{\"id\":\"cal_same\"}]}";
            Require((await Error<PostboxException>(() => f.Api.GetCalendarsAsync(Own))).Code == "bad_response");
            f.Handler.Occurrences = "{\"occurrences\":[{\"occurrence_id\":\"occ_same\",\"event_id\":\"evt_a\"},{\"occurrence_id\":\"occ_same\",\"event_id\":\"evt_b\"}]}";
            await Error<PostboxException>(() => f.Api.GetCalendarOccurrencesAsync(Own, 0, 10));
            f.Handler.Detail = "{}"; await Error<PostboxException>(() => f.Api.GetCalendarEventAsync(Own, new("evt_fixture")));
        });
        yield return ("Calendar payloads freeze sorted nested fields and retain their exact bytes after restoration", () =>
        {
            var fields = new Dictionary<string, object> { ["title"] = "Fixture title ğ 😀", ["request_key"] = "key_1", ["meeting"] = new Dictionary<string, object> { ["waiting"] = "guests", ["future_policy"] = true } };
            var body = CalendarWritePayload.From(fields); var encoded = JsonSerializer.Serialize(body.Fields()); fields["title"] = "Changed later";
            var restored = CalendarWritePayload.Restore(encoded);
            Require(body.RequestKey() == "key_1" && restored.Fields().GetProperty("title").GetString() == "Fixture title ğ 😀"
                && restored.Fields().GetProperty("meeting").GetProperty("future_policy").GetBoolean() && restored.ByteCount == body.ByteCount
                && !body.ToString().Contains("Fixture title", StringComparison.Ordinal)); return Task.CompletedTask;
        });
        yield return ("Calendar payloads refuse ambiguous fields, changed saved bytes and invalid request keys", async () =>
        {
            using var duplicate = JsonDocument.Parse("{\"request_key\":\"k\",\"meeting\":{\"waiting\":\"guests\",\"waiting\":\"none\"}}");
            await Error<ArgumentException>(() => { _ = new CalendarWritePayload(duplicate.RootElement); return Task.CompletedTask; });
            await Error<ArgumentException>(() => { _ = CalendarWritePayload.Restore("{ \"request_key\":\"k\" }"); return Task.CompletedTask; });
            foreach (var key in new[] { "", "a b", new string('a', 65), "ğ" })
                await Error<ArgumentException>(() => { _ = CalendarWritePayload.From(new { request_key = key }).RequestKey(); return Task.CompletedTask; });
        });
        yield return ("Calendar edits carry the reviewed revision and opaque occurrence without changing their route", async () =>
        {
            using var f = new Fixture(); var key = new CalendarEventKey("evt_fixture", "occ_original_key");
            var response = await f.Api.UpdateCalendarEventAsync(Own, key, 3, CalendarEventScope.This, CalendarWritePayload.From(new { title = "Reviewed edit ğ" }));
            var call = f.Handler.Routes.Single(); using var body = JsonDocument.Parse(call.Body);
            Require(response.Event.Id == key.EventId && call.Method == "PATCH" && call.Identity == Own && call.Path == "/v1/calendar/events/evt_fixture"
                && body.RootElement.GetProperty("expected_revision").GetInt64() == 3 && body.RootElement.GetProperty("scope").GetString() == "this"
                && body.RootElement.GetProperty("occurrence_id").GetString() == key.OccurrenceId && body.RootElement.GetProperty("title").GetString() == "Reviewed edit ğ");
        });
        yield return ("A following-series edit accepts only a split explicitly linked to the reviewed original event", async () =>
        {
            using var f = new Fixture(); f.Handler.Detail = "{\"event\":{\"id\":\"evt_new\"},\"split_from\":{\"event_id\":\"evt_other\"}}";
            var key = new CalendarEventKey("evt_fixture", "occ_original_key"); var changes = CalendarWritePayload.From(new { title = "Split fixture" });
            await Error<PostboxException>(() => f.Api.UpdateCalendarEventAsync(Own, key, 3, CalendarEventScope.Following, changes));
            f.Handler.Detail = "{\"event\":{\"id\":\"evt_new\"},\"split_from\":{\"event_id\":\"evt_fixture\"}}";
            Require((await f.Api.UpdateCalendarEventAsync(Own, key, 3, CalendarEventScope.Following, changes)).Event.Id == "evt_new");
        });
        yield return ("Calendar mutation inputs cannot override their reviewed scope or send invalid responses", async () =>
        {
            using var f = new Fixture(); var key = new CalendarEventKey("evt_fixture"); var empty = CalendarWritePayload.From(new { });
            await Error<ArgumentException>(() => f.Api.UpdateCalendarEventAsync(Own, key, -1, CalendarEventScope.All, empty));
            await Error<ArgumentException>(() => f.Api.UpdateCalendarEventAsync(Own, key, 1, CalendarEventScope.This, empty));
            await Error<ArgumentException>(() => f.Api.UpdateCalendarEventAsync(Own, key, 1, CalendarEventScope.All, CalendarWritePayload.From(new { expected_revision = 100 })));
            await Error<ArgumentException>(() => f.Api.AnswerCalendarInvitationAsync(Own, key, (CalendarAnswer)999, CalendarAnswerScope.All));
            await Error<ArgumentException>(() => f.Api.AnswerCalendarInvitationAsync(Own, key, CalendarAnswer.Accepted, CalendarAnswerScope.All, new string('a', 301)));
            await Error<ArgumentException>(() => f.Api.SaveCalendarRemindersAsync(Own, key.EventId, [new(-1)]));
            Require(f.Handler.Calls.Count == 0);
        });
        yield return ("Invitation replies and cancellation use distinct scoped endpoints and never create a contact", async () =>
        {
            using var f = new Fixture(); var key = new CalendarEventKey("evt_fixture", "occ_original_key");
            await f.Api.AnswerCalendarInvitationAsync(Own, key, CalendarAnswer.Tentative, CalendarAnswerScope.This, "Reviewed response");
            await f.Api.CancelCalendarEventAsync(Own, key, 4, CalendarEventScope.Following, "Reviewed cancellation");
            Require(f.Handler.Routes.Count == 2 && f.Handler.Routes.All(c => c.Method == "POST" && c.Identity == Own)
                && f.Handler.Routes[0].Path.EndsWith("/rsvp", StringComparison.Ordinal) && f.Handler.Routes[1].Path.EndsWith("/cancel", StringComparison.Ordinal));
            using var reply = JsonDocument.Parse(f.Handler.Routes[0].Body); using var cancel = JsonDocument.Parse(f.Handler.Routes[1].Body);
            Require(reply.RootElement.GetProperty("status").GetString() == "tentative" && reply.RootElement.GetProperty("scope").GetString() == "this"
                && cancel.RootElement.GetProperty("expected_revision").GetInt64() == 4 && cancel.RootElement.GetProperty("scope").GetString() == "following");
        });
        yield return ("A calendar mutation retains every original caller before its delayed POST", async () =>
        {
            using var f = new Fixture(); var caller = f.Tokens.Root.Child(); f.Tokens.BlockAt = 2; Task answer;
            using (new AuthorizationContext(caller)) answer = f.Api.AnswerCalendarInvitationAsync(Own, new("evt_fixture"), CalendarAnswer.Accepted, CalendarAnswerScope.All);
            await f.Tokens.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); f.Tokens.Release.TrySetResult();
            await Error<AuthorizationRetiredException>(async () => await answer); Require(f.Handler.Routes.Count == 0 && f.Tokens.Root.IsActive);
        });
        yield return ("Ordinary renewal repeats only the exact reviewed calendar mutation body", async () =>
        {
            using var f = new Fixture(); f.Handler.UnauthorizedOnce = true;
            await f.Api.UpdateCalendarEventAsync(Own, new("evt_fixture"), 4, CalendarEventScope.All, CalendarWritePayload.From(new { title = "Same reviewed update" }));
            Require(f.Handler.Routes.Count == 2 && f.Handler.Routes.All(c => c.Method == "PATCH" && c.Identity == Own)
                && f.Handler.Routes[0].Body == f.Handler.Routes[1].Body && f.Tokens.Refreshes == 1);
        });
        yield return ("Calendar settings reject unknown zones and malformed values before making a request", async () =>
        {
            using var f = new Fixture();
            foreach (var fields in new[] { CalendarWritePayload.From(new { timezone = "Turkey Standard Time" }), CalendarWritePayload.From(new { week_start = "Monday" }),
                CalendarWritePayload.From(new { previews = "on" }), CalendarWritePayload.From(new { unknown_setting = true }) })
                await Error<ArgumentException>(() => f.Api.SaveCalendarSettingsAsync(Own, fields));
            Require(f.Handler.Calls.Count == 0);
        });
        yield return ("Calendar private reminders and export keep the acting inbox and safe fixed event path", async () =>
        {
            using var f = new Fixture(); await f.Api.SaveCalendarRemindersAsync(Own, "evt_fixture", [new(10)]);
            var bytes = await f.Api.ExportCalendarEventAsync(Own, "evt_fixture");
            Require(f.Handler.Routes.Count == 2 && f.Handler.Routes.All(c => c.Identity == Own)
                && f.Handler.Routes[0].Method == "PUT" && f.Handler.Routes[1].Method == "GET" && Encoding.UTF8.GetString(bytes).Contains("BEGIN:VCALENDAR", StringComparison.Ordinal));
        });
    }
    private static async Task<T> Error<T>(Func<Task> work) where T : Exception
    { try { await work(); } catch (T error) { return error; } throw new Exception("Expected " + typeof(T).Name); }
    private static void Require(bool value) { if (!value) throw new Exception("Calendar API assertion failed."); }
    private sealed class Tokens : IAccessTokenProvider
    {
        internal AuthorizationLease Root = new(); public AuthorizationLease? AcceptedAuthorization => Root;
        internal int BlockAt, Calls, Refreshes;
        internal readonly TaskCompletionSource Waiting = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public async Task<string> GetTokenAsync(CancellationToken ct) { if (++Calls == BlockAt) { Waiting.TrySetResult(); await Release.Task.WaitAsync(ct); } return "synthetic-calendar-fixture"; }
        public Task<string> RefreshTokenAsync(CancellationToken ct) { ct.ThrowIfCancellationRequested(); Refreshes++; return Task.FromResult("synthetic-calendar-renewal"); }
    }
    private sealed record Call(string Method, string Path, string Query, string? Identity, string Body);
    private sealed class Handler : HttpMessageHandler
    {
        internal bool Enabled = true, Older, UnauthorizedOnce, BlockRoute;
        internal string Folders = "{\"calendars\":[]}", Occurrences = "{\"occurrences\":[]}", Detail = "{\"event\":{\"id\":\"evt_fixture\"}}";
        internal readonly List<Call> Calls = [];
        internal List<Call> Routes => Calls.Where(c => c.Path != "/v1/capabilities").ToList();
        internal readonly TaskCompletionSource Waiting = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            var uri = request.RequestUri!; Calls.Add(new(request.Method.Method, uri.AbsolutePath, uri.Query,
                request.Headers.TryGetValues("x-pigeonpost-identity", out var identity) ? identity.Single() : null,
                request.Content is null ? "" : await request.Content.ReadAsStringAsync(ct)));
            var status = HttpStatusCode.OK; string body;
            if (uri.AbsolutePath == "/v1/capabilities")
            { status = Older ? HttpStatusCode.NotFound : HttpStatusCode.OK; body = Enabled ? "{\"features\":[\"calendar\"]}" : "{\"features\":[]}"; }
            else
            {
                if (BlockRoute) { BlockRoute = false; Waiting.TrySetResult(); await Release.Task.WaitAsync(ct); }
                if (UnauthorizedOnce) { UnauthorizedOnce = false; status = HttpStatusCode.Unauthorized; }
                body = uri.AbsolutePath switch { "/v1/calendars" => Folders, "/v1/calendar/events" => Occurrences,
                    "/v1/calendar/events/evt_fixture" => Detail, "/v1/calendar/invitations" => "{\"invitations\":[]}",
                    "/v1/calendar/settings" => "{\"timezone\":\"Europe/Istanbul\"}", "/v1/calendar/events/evt_fixture/rsvp" or "/v1/calendar/events/evt_fixture/cancel" or "/v1/calendar/events/evt_fixture/reminders" => "{}",
                    "/v1/calendar/events/evt_fixture.ics" => "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", _ => throw new Exception("Unexpected fixture path.") };
            }
            return new(status) { Content = new StringContent(body, Encoding.UTF8, "application/json") };
        }
    }
    private sealed class Fixture : IDisposable
    {
        internal readonly Tokens Tokens = new(); internal readonly Handler Handler = new(); private readonly HttpClient http;
        internal readonly PostboxClient Api;
        internal Fixture() { http = new(Handler); Api = new(http, Tokens, new Uri("https://calendar-fixture.invalid")); }
        public void Dispose() { Tokens.Root.Retire(); Api.Dispose(); http.Dispose(); }
    }
}
