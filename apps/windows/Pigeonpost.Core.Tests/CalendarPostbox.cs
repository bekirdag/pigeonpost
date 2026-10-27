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
            Console.WriteLine("PASS Windows calendar postbox interoperability: empty reads, protected folder/event creation, exact committed-loss replay, one occurrence, owned details, invitation separation and settings.");
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
