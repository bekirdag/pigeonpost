#if UI_TESTS
using System.Security.Cryptography;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

// Synthetic native UI data only. This service performs no HTTP or public authentication.
internal sealed class NativeCalendarFixture : ICalendarActionsService, IProtectedCalendarService
{
    private readonly PreviewInboxService inbox;
    private readonly string statePath;
    private readonly Dictionary<string, CalendarEventResponse> events = [];
    private readonly List<CalendarFolder> calendars = [new("cal_work", "Work", "blue")];
    private readonly Dictionary<string, (string Hash, CalendarCreateReceipt Receipt)> requests = [];
    private CalendarSettings settings = new(Previews: true, Timezone: "Europe/Istanbul", WeekStart: 1);
    private int posts, edits, answers, cancels, reminderSaves, settingsSaves, serial, losses;
    internal AuthorizationLease Root { get; } = new();
    public AuthorizationLease? AcceptedAuthorization => Root;
    public string? RecoveryOwnerKey => new string('a', 64);
    public string? RecoveryOrigin => "https://calendar-ui-fixture.invalid";
    private string own = "";
    private readonly string today;
    internal NativeCalendarFixture(PreviewInboxService inbox, string statePath)
    {
        this.inbox = inbox; this.statePath = statePath;
        losses = int.TryParse(Environment.GetEnvironmentVariable("PIGEONPOST_UI_CALENDAR_LOSSES"), out var count) ? count : 0;
        var day = today = NodaTime.SystemClock.Instance.GetCurrentInstant().InZone(CalendarTime.Zone("Europe/Istanbul")).Date.ToString("yyyy-MM-dd", null);
        CalendarEventResponse Seed(string id, string title, string role) => new(new(id, CalendarId: "cal_work", Revision: 1, Status: "confirmed", Title: title,
            AllDay: false, Start: day + "T09:00", End: day + "T10:00", Tzid: "Europe/Istanbul",
            StartsAt: new CalendarWallTime(day + "T09:00", "Europe/Istanbul").Resolve(), EndsAt: new CalendarWallTime(day + "T10:00", "Europe/Istanbul").Resolve(),
            Organizer: new(Handle: "/fixture/design", Name: "Design"), My: new(Role: role, AttendeeId: role == "attendee" ? "att_" + id : null, Rsvp: role == "attendee" ? "pending" : "accepted"),
            CanEdit: role == "organizer", Reminders: [new(10, "push")]));
        events.Add("evt_design", Seed("evt_design", "Design review", "organizer"));
        events.Add("evt_invitation", Seed("evt_invitation", "Team invitation", "attendee"));
        events.Add("evt_filtered", Seed("evt_filtered", "Filtered invitation", "attendee"));
        Save();
    }
    private void Check(CancellationToken ct) { ct.ThrowIfCancellationRequested(); Root.RequireActive(); }
    private void Save()
    {
        var state = new { today, posts, edits, answers, cancels, reminderSaves, settingsSaves, requests = requests.Select(p => new { key = p.Key, sha = p.Value.Hash }),
            events = events.Values.Select(v => new { id = v.Event.Id, title = v.Event.Title, revision = v.Event.Revision, status = v.Event.Status, rsvp = v.Event.My?.Rsvp }),
            settings = new { timezone = settings.Timezone, previews = settings.Previews, week_start = settings.WeekStart } };
        var temporary = statePath + ".new"; File.WriteAllText(temporary, JsonSerializer.Serialize(state)); FixtureState.Replace(temporary, statePath);
    }
    public async Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct)
    { Check(ct); var mailboxes = await inbox.GetMailboxesAsync(ct); own = mailboxes[0].Address; return mailboxes; }
    public Task<ServerFeatures> GetCapabilitiesAsync(CancellationToken ct)
    { Check(ct); return Task.FromResult(new ServerFeatures(new HashSet<string> { ServerFeatures.Calendar })); }
    public Task<IReadOnlyList<CalendarFolder>> GetCalendarsAsync(string identity, CancellationToken ct)
    { Check(ct); return Task.FromResult<IReadOnlyList<CalendarFolder>>(identity == own ? calendars.ToArray() : []); }
    public Task<CalendarSettings> GetCalendarSettingsAsync(string identity, CancellationToken ct) { Check(ct); return Task.FromResult(settings); }
    public Task<IReadOnlyList<CalendarOccurrence>> GetCalendarOccurrencesAsync(string identity, long from, long to, IReadOnlyList<string>? selected, CancellationToken ct)
    {
        Check(ct);
        var rows = events.Values.Select(v => v.Event).Where(e => e.My?.Role == "organizer" && e.StartsAt < to && e.EndsAt > from && (selected is null || selected.Contains(e.CalendarId!)))
            .Select(e => new CalendarOccurrence("occ_" + e.Id, e.Id, CalendarId: e.CalendarId, AllDay: e.AllDay, StartsAt: e.StartsAt, EndsAt: e.EndsAt, Tzid: e.Tzid,
                StartDate: e.StartDate, EndDate: e.EndDate, Title: e.Title, Status: e.Status, Meeting: e.Meeting)).ToArray();
        return Task.FromResult<IReadOnlyList<CalendarOccurrence>>(identity == own ? rows : []);
    }
    public Task<IReadOnlyList<CalendarInvitation>> GetCalendarInvitationsAsync(string identity, bool filtered, CancellationToken ct)
    {
        Check(ct); var id = filtered ? "evt_filtered" : "evt_invitation"; var e = events[id].Event;
        return Task.FromResult<IReadOnlyList<CalendarInvitation>>(identity == own && e.My?.Rsvp == "pending" ? [new(e.Id, Title: e.Title)] : []);
    }
    public Task<CalendarEventResponse> GetCalendarEventAsync(string identity, CalendarEventKey key, CancellationToken ct)
    {
        Check(ct); if (identity != own || !events.TryGetValue(key.EventId, out var detail)) throw new PostboxException(404, "not_found");
        return Task.FromResult(detail with { Occurrence = key.OccurrenceId is null ? null : new(OccurrenceId: key.OccurrenceId) });
    }
    public Task<CalendarCreateReceipt> SendPreparedCalendarAsync(MessageRecoveryScope scope, PreparedCalendarAttempt attempt, CancellationToken ct)
    {
        Check(ct); scope.RequireCurrent(ct); posts++;
        var fields = attempt.Payload.Fields(); var hash = Convert.ToHexString(SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(fields.GetRawText())));
        if (requests.TryGetValue(attempt.Id, out var previous))
        { if (previous.Hash != hash) throw new InvalidOperationException("Fixture retry changed bytes."); Save(); return Task.FromResult(previous.Receipt); }
        CalendarCreateReceipt receipt;
        if (attempt.Kind == CalendarAttemptKind.Calendar)
        {
            var calendar = new CalendarFolder("cal_created_" + ++serial, fields.GetProperty("name").GetString(), "blue"); calendars.Add(calendar); receipt = new(attempt.Kind, Calendar: calendar);
        }
        else
        {
            string? Value(string key) => fields.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
            var allDay = fields.TryGetProperty("all_day", out var all) && all.GetBoolean(); var id = "evt_created_" + ++serial; var zone = Value("tzid") ?? settings.Timezone!;
            var e = new ScheduledEvent(id, CalendarId: Value("calendar_id") ?? "cal_work", Revision: 1, Status: "confirmed", Title: Value("title"), AllDay: allDay,
                Start: Value("start"), End: Value("end"), Tzid: zone, StartDate: Value("start_date"), EndDate: Value("end_date"),
                StartsAt: allDay ? CalendarDay.Parse(Value("start_date")!).Start(CalendarTime.Zone(zone)) : new CalendarWallTime(Value("start")!, zone).Resolve(),
                EndsAt: allDay ? CalendarDay.Parse(Value("end_date")!).Start(CalendarTime.Zone(zone)) : new CalendarWallTime(Value("end")!, zone).Resolve(),
                My: new(Role: "organizer"), CanEdit: true);
            events.Add(id, new(e)); receipt = new(attempt.Kind, Event: new(e));
        }
        requests.Add(attempt.Id, (hash, receipt)); Save();
        if (attempt.Kind == CalendarAttemptKind.Event && losses > 0) { losses--; throw new HttpRequestException("Synthetic committed calendar response loss."); }
        return Task.FromResult(receipt);
    }
    public Task<CalendarEventResponse> UpdateCalendarEventAsync(string identity, CalendarEventKey key, long expectedRevision, CalendarEventScope scope, CalendarWritePayload changes, CancellationToken ct)
    {
        Check(ct); var old = events[key.EventId]; if (old.Event.Revision != expectedRevision) throw new PostboxException(409, "revision_conflict");
        if (old.Event.My?.Role != "organizer") throw new PostboxException(403, "forbidden");
        var fields = changes.Fields(); var updated = old with { Event = old.Event with { Title = fields.TryGetProperty("title", out var title) ? title.GetString() : old.Event.Title, Revision = expectedRevision + 1 } };
        events[key.EventId] = updated; edits++; Save(); return Task.FromResult(updated with { Occurrence = key.OccurrenceId is null ? null : new(OccurrenceId: key.OccurrenceId) });
    }
    public Task CancelCalendarEventAsync(string identity, CalendarEventKey key, long expectedRevision, CalendarEventScope scope, string? message, CancellationToken ct)
    {
        Check(ct); var old = events[key.EventId]; if (old.Event.Revision != expectedRevision) throw new PostboxException(409, "revision_conflict");
        events[key.EventId] = old with { Event = old.Event with { Status = "cancelled", Revision = expectedRevision + 1 } }; cancels++; Save(); return Task.CompletedTask;
    }
    public Task AnswerCalendarInvitationAsync(string identity, CalendarEventKey key, CalendarAnswer answer, CalendarAnswerScope scope, string? note, CancellationToken ct)
    { Check(ct); var old = events[key.EventId]; events[key.EventId] = old with { Event = old.Event with { My = old.Event.My! with { Rsvp = answer.ToString().ToLowerInvariant() } } }; answers++; Save(); return Task.CompletedTask; }
    public Task SaveCalendarRemindersAsync(string identity, string eventId, IReadOnlyList<CalendarReminder> reminders, CancellationToken ct)
    { Check(ct); var old = events[eventId]; events[eventId] = old with { Event = old.Event with { Reminders = reminders.ToArray() } }; reminderSaves++; Save(); return Task.CompletedTask; }
    public Task<CalendarSettings> SaveCalendarSettingsAsync(string identity, CalendarWritePayload payload, CancellationToken ct)
    { Check(ct); var fields = payload.Fields(); settings = new(fields.GetProperty("previews").GetBoolean(), fields.GetProperty("timezone").GetString(), fields.GetProperty("week_start").GetInt32()); settingsSaves++; Save(); return Task.FromResult(settings); }
    public Task<byte[]> ExportCalendarEventAsync(string identity, string eventId, CancellationToken ct)
    { Check(ct); return Task.FromResult(System.Text.Encoding.UTF8.GetBytes("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")); }
    public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct) => throw new NotSupportedException();
    public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? thread, CancellationToken ct) => throw new NotSupportedException();
    public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => throw new NotSupportedException();
    public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => throw new NotSupportedException();
    public Task AcknowledgeAsync(string identity, string message, CancellationToken ct) => throw new NotSupportedException();
    public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => throw new NotSupportedException();
    public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => throw new NotSupportedException();
    public Task DeleteThreadAsync(string identity, string thread, CancellationToken ct) => throw new NotSupportedException();
}

// The UI scripts poll these state files while the app rewrites them. On Windows replacing a file
// that a reader has open fails with a sharing violation, which used to surface as the fixture's
// action failing after its in-memory change (the page said "Cancelled", the file never did).
internal static class FixtureState
{
    internal static void Replace(string temporary, string path)
    {
        for (var attempt = 0; ; attempt++)
        {
            try { File.Move(temporary, path, overwrite: true); return; }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException && attempt < 100) { Thread.Sleep(20); }
        }
    }
}
#endif
