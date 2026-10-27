using System.Net.Http.Json;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

internal static class CalendarPostbox
{
    internal static async Task<int> RunAsync()
    {
        var stage = "setup";
        var folder = Path.Combine(Path.GetTempPath(), "pp-windows-calendar-postbox-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(folder);
        try
        {
            var file = Environment.GetEnvironmentVariable("PIGEONPOST_TEST_FIXTURE") ?? throw new InvalidOperationException();
            using var setup = JsonDocument.Parse(await File.ReadAllTextAsync(file)); var root = setup.RootElement;
            var endpoint = Local(root.GetProperty("postbox").GetString()!); var proxy = Local(root.GetProperty("lossProxy").GetString()!);
            var alice = root.GetProperty("people").GetProperty("alice"); var bob = root.GetProperty("people").GetProperty("bob");
            var own = alice.GetProperty("address").GetString()!; var peer = bob.GetProperty("address").GetString()!;
            using var http = new HttpClient(new HttpClientHandler { AllowAutoRedirect = false }) { Timeout = TimeSpan.FromSeconds(15) };
            using var api = new PostboxClient(http, new Tokens(alice.GetProperty("token").GetString()!, 'a'), endpoint);
            using var other = new PostboxClient(http, new Tokens(bob.GetProperty("token").GetString()!, 'b'), endpoint);
            using var loss = new PostboxClient(http, new Tokens(alice.GetProperty("token").GetString()!, 'a'), proxy);
            using var scope = await MessageRecoveryScope.AuthorizeAsync(loss, own) ?? throw new InvalidOperationException();
            using var protector = new MessageAttemptStoreTests.Protector();
            var coordinator = new CalendarDeliveryCoordinator(loss, new FileCalendarAttemptStore(folder, protector));
            stage = "read without creating a default";
            Check((await api.GetCalendarsAsync(own)).Count == 0 && (await api.GetCalendarsAsync(own)).Count == 0);
            stage = "protected calendar committed loss";
            var create = await coordinator.PrepareAsync(scope, CalendarAttemptKind.Calendar,
                CalendarWritePayload.From(new { name = "Windows fixture calendar", color = "blue", request_key = "folder_" + Guid.NewGuid().ToString("N") }));
            await Control(http, proxy, "after"); await Lost(() => coordinator.SendAsync(scope, create));
            var calendars = await api.GetCalendarsAsync(own); Check(calendars.Count == 1 && (await coordinator.GetPendingAsync(scope)).Single().Id == create.Id);
            await Control(http, proxy, "off"); var confirmed = await coordinator.SendAsync(scope, create);
            Check(!confirmed.PendingCleanup && confirmed.Receipt.Calendar?.Id == calendars.Single().Id && (await api.GetCalendarsAsync(own)).Count == 1);
            await Attempts(http, proxy, create.Id);
            stage = "protected scheduled event committed loss";
            var day = DateTime.UtcNow.Date.AddDays(1).ToString("yyyy-MM-dd", System.Globalization.CultureInfo.InvariantCulture);
            var payload = CalendarWritePayload.From(new { title = "Windows protected calendar fixture ğ 😀", calendar_id = calendars.Single().Id,
                all_day = false, start = day + "T09:00", end = day + "T10:00", tzid = "Europe/Istanbul",
                request_key = "event_" + Guid.NewGuid().ToString("N"), attendees = new[] { new { handle = peer } } });
            var prepared = await coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, payload);
            await Control(http, proxy, "after"); await Lost(() => coordinator.SendAsync(scope, prepared));
            var range = CalendarWallTime.Range(day + "T00:00", day + "T23:59", "Europe/Istanbul");
            var rows = await api.GetCalendarOccurrencesAsync(own, range.Start, range.End);
            Check(rows.Count == 1 && rows.Single().Title == "Windows protected calendar fixture ğ 😀");
            stage = "restarted event recovery";
            var restarted = new CalendarDeliveryCoordinator(loss, new FileCalendarAttemptStore(folder, protector));
            var restored = (await restarted.GetPendingAsync(scope)).Single(); Check(restored.Id == prepared.Id && restored.CreatedAt == prepared.CreatedAt);
            await Control(http, proxy, "off"); var eventResult = await restarted.SendAsync(scope, restored);
            Check(!eventResult.PendingCleanup && eventResult.Receipt.Event?.Event.Id == rows.Single().EventId);
            await Attempts(http, proxy, prepared.Id);
            Check((await api.GetCalendarOccurrencesAsync(own, range.Start, range.End)).Count == 1 && (await restarted.GetPendingAsync(scope)).Count == 0);
            stage = "owned selected detail and invitation projections";
            var detail = await api.GetCalendarEventAsync(own, rows.Single().Key);
            Check(detail.Event.Id == rows.Single().EventId && detail.Occurrence?.OccurrenceId == rows.Single().OccurrenceId && detail.Event.My?.Role == "organizer");
            var invitations = await other.GetCalendarInvitationsAsync(peer);
            Check(invitations.Count == 1 && invitations.Single().EventId == detail.Event.Id && (await other.GetCalendarInvitationsAsync(peer, true)).Count == 0);
            _ = await api.GetCalendarSettingsAsync(own);
            stage = "revisioned edit and stale edit refusal";
            var updated = await api.UpdateCalendarEventAsync(own, rows.Single().Key, detail.Event.Revision!.Value, CalendarEventScope.All,
                CalendarWritePayload.From(new { title = "Windows revised calendar fixture" }));
            Check(updated.Event.Title == "Windows revised calendar fixture" && updated.Event.Revision > detail.Event.Revision);
            try
            {
                await api.UpdateCalendarEventAsync(own, rows.Single().Key, detail.Event.Revision.Value, CalendarEventScope.All, CalendarWritePayload.From(new { title = "Must not replace latest" }));
                throw new InvalidOperationException();
            }
            catch (PostboxException error) when (error.Code == "revision_conflict") { }
            stage = "native invitation RSVP and personal reminders";
            await other.AnswerCalendarInvitationAsync(peer, new(detail.Event.Id), CalendarAnswer.Accepted, CalendarAnswerScope.All);
            var accepted = await other.GetCalendarEventAsync(peer, rows.Single().Key);
            Check(accepted.Event.My?.Rsvp == "accepted" && (await other.GetCalendarInvitationsAsync(peer)).Count == 0);
            await other.SaveCalendarRemindersAsync(peer, detail.Event.Id, [new(10, "push"), new(30, "push")]);
            Check((await other.GetCalendarEventAsync(peer, rows.Single().Key)).Event.Reminders?.Select(r => r.MinutesBefore).Order().SequenceEqual([10, 30]) == true);
            stage = "calendar settings and read-only ICS export";
            var savedSettings = await api.SaveCalendarSettingsAsync(own, CalendarWritePayload.From(new { previews = false, timezone = "Europe/Istanbul", week_start = 1 }));
            Check(savedSettings.Previews == false && savedSettings.Timezone == "Europe/Istanbul" && (await api.GetCalendarSettingsAsync(own)).WeekStart == 1);
            var ics = System.Text.Encoding.UTF8.GetString(await api.ExportCalendarEventAsync(own, detail.Event.Id));
            Check(ics.Contains("BEGIN:VCALENDAR", StringComparison.Ordinal) && ics.Contains("Windows revised calendar fixture", StringComparison.Ordinal));
            stage = "calendar controller committed-loss recovery";
            using var model = new CalendarStore(loss, loss, restarted);
            await model.ActivateAsync(own, await loss.GetCapabilitiesAsync(default));
            Check(model.State.Enabled && model.State.CanCreate && model.State.Folders.Count == 1);
            using var action = model.CaptureView()?.BeginAction() ?? throw new InvalidOperationException();
            var controllerBody = CalendarWritePayload.From(new { request_key = "controller_" + Guid.NewGuid().ToString("N"), title = "Windows controller real postbox fixture",
                all_day = false, start = day + "T11:00", end = day + "T12:00", tzid = "Europe/Istanbul" });
            await Control(http, proxy, "after");
            Check(await model.CreateAsync(action, CalendarAttemptKind.Event, controllerBody) is null && !model.State.Busy && model.State.Pending.Count == 1);
            var reviewed = model.State.Pending.Single(); await Control(http, proxy, "off");
            Check(await model.RetryAsync(action, reviewed) is not null && model.State.Pending.Count == 0 && !model.State.Busy);
            await Attempts(http, proxy, reviewed.Id);
            Check((await api.GetCalendarOccurrencesAsync(own, range.Start, range.End)).Count(o => o.Title == "Windows controller real postbox fixture") == 1);
            stage = "controller revisioned edit and cancellation";
            var controllerRow = (await api.GetCalendarOccurrencesAsync(own, range.Start, range.End)).Single(o => o.Title == "Windows controller real postbox fixture");
            var edit = await model.ReviewEditAsync(action, controllerRow.Key, CalendarEventScope.All) ?? throw new InvalidOperationException();
            var changed = await model.UpdateEventAsync(action, CalendarEventDraft.From(edit, "UTC") with { Title = "Windows controller revised fixture" });
            Check(changed?.Event.Title == "Windows controller revised fixture" && !model.State.Busy);
            var cancellation = await model.ReviewEditAsync(action, controllerRow.Key, CalendarEventScope.All) ?? throw new InvalidOperationException();
            Check(await model.CancelEventAsync(action, cancellation) && (await api.GetCalendarEventAsync(own, controllerRow.Key)).Event.Status == "cancelled");
            stage = "recurring occurrence edit and following-series split";
            var recurring = await restarted.PrepareAsync(scope, CalendarAttemptKind.Event, CalendarWritePayload.From(new {
                request_key = "recurring_" + Guid.NewGuid().ToString("N"), title = "Windows recurring fixture", all_day = false,
                start = day + "T14:00", end = day + "T15:00", tzid = "Europe/Istanbul", rrule = "FREQ=DAILY;COUNT=3" }));
            var recurringReceipt = await restarted.SendAsync(scope, recurring); var recurringId = recurringReceipt.Receipt.Event!.Event.Id;
            var seriesRange = CalendarWallTime.Range(day + "T00:00", CalendarDay.Parse(day).PlusDays(4).Text + "T00:00", "Europe/Istanbul");
            var series = (await api.GetCalendarOccurrencesAsync(own, seriesRange.Start, seriesRange.End)).Where(o => o.EventId == recurringId).OrderBy(o => o.StartsAt).ToArray();
            Check(series.Length == 3); var seriesDetail = await api.GetCalendarEventAsync(own, series[0].Key);
            var occurrence = await api.UpdateCalendarEventAsync(own, series[0].Key, seriesDetail.Event.Revision!.Value, CalendarEventScope.This, CalendarWritePayload.From(new { title = "One occurrence override" }));
            Check(occurrence.Event.Id == recurringId && (await api.GetCalendarOccurrencesAsync(own, seriesRange.Start, seriesRange.End)).Single(o => o.OccurrenceId == series[0].OccurrenceId).Title == "One occurrence override");
            var splitDetail = await api.GetCalendarEventAsync(own, series[1].Key);
            var split = await api.UpdateCalendarEventAsync(own, series[1].Key, splitDetail.Event.Revision!.Value, CalendarEventScope.Following, CalendarWritePayload.From(new { title = "Following occurrences fixture" }));
            Check(split.Event.Id != recurringId && split.SplitFrom?.EventId == recurringId && split.Event.Title == "Following occurrences fixture");
            await api.CancelCalendarEventAsync(own, new(split.Event.Id), split.Event.Revision!.Value, CalendarEventScope.All);
            Check((await api.GetCalendarEventAsync(own, new(split.Event.Id))).Event.Status == "cancelled");
            Console.WriteLine("PASS Windows calendar postbox interoperability: protected create/controller recovery, exact committed-loss replay, revisioned edit/conflict, RSVP, reminders, settings, ICS, cancellation, occurrence override and following-series split.");
            return 0;
        }
        catch (Exception error)
        { Console.Error.WriteLine($"FAIL Windows calendar interoperability at {stage} ({error.GetType().Name})."); return 1; }
        finally { Directory.Delete(folder, recursive: true); }
    }
    private static Uri Local(string value)
    {
        var uri = new Uri(value);
        if (!uri.IsLoopback || uri.Scheme != "http" || uri.AbsolutePath != "/" || uri.UserInfo.Length > 0 || uri.Query.Length > 0 || uri.Fragment.Length > 0) throw new InvalidOperationException();
        return uri;
    }
    private static void Check(bool value) { if (!value) throw new InvalidOperationException(); }
    private static async Task Control(HttpClient http, Uri proxy, string mode)
    { using var response = await http.PostAsJsonAsync(new Uri(proxy, "/__test/control"), new { mode }); response.EnsureSuccessStatusCode(); }
    private static async Task Lost(Func<Task> send)
    { try { await send(); } catch (HttpRequestException) { return; } throw new InvalidOperationException(); }
    private static async Task Attempts(HttpClient http, Uri proxy, string key)
    {
        using var response = await http.GetAsync(new Uri(proxy, "/__test/attempts")); response.EnsureSuccessStatusCode();
        using var document = JsonDocument.Parse(await response.Content.ReadAsStringAsync()); var rows = document.RootElement.GetProperty("attempts").EnumerateArray().ToArray();
        Check(rows.Length == 2 && rows.All(row => row.GetProperty("requestKey").GetString() == key && row.GetProperty("status").GetInt32() is >= 200 and < 300)
            && rows.Select(row => row.GetProperty("sha").GetString()).Distinct().Count() == 1);
    }
    private sealed class Tokens(string token, char owner) : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; } = new();
        public string? RecoveryOwnerKey => new string(owner, 64);
        public Task<string> GetTokenAsync(CancellationToken ct) { ct.ThrowIfCancellationRequested(); return Task.FromResult(token); }
        public Task<string> RefreshTokenAsync(CancellationToken ct) => throw new InvalidOperationException("Synthetic fixture does not renew.");
    }
}
