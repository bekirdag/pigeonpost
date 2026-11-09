using System.Text.Json;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Core;

public sealed partial class PostboxClient : ICalendarSharingService
{
    private static PostboxException CalendarBad() => new(502, "bad_response");
    private static string? CalendarText(JsonElement value, string name) => value.TryGetProperty(name, out var field)
        ? field.ValueKind switch { JsonValueKind.String => field.GetString(), JsonValueKind.Null => null, _ => throw CalendarBad() } : null;
    private static long? CalendarNumber(JsonElement value, string name) => value.TryGetProperty(name, out var field)
        ? field.ValueKind switch { JsonValueKind.Number when field.TryGetInt64(out var n) => n, JsonValueKind.Null => null, _ => throw CalendarBad() } : null;
    private static bool CalendarFlag(JsonElement value, string name) => value.TryGetProperty(name, out var field)
        ? field.ValueKind switch { JsonValueKind.True => true, JsonValueKind.False => false, _ => throw CalendarBad() } : false;

    private static CalendarFeed ParseFeed(JsonElement body, bool expectUrl)
    {
        if (body.ValueKind != JsonValueKind.Object) throw CalendarBad();
        var enabled = CalendarFlag(body, "enabled");
        var projection = CalendarText(body, "projection");
        var url = CalendarText(body, "url");
        if (enabled && !CalendarSharing.Projection(projection) || !enabled && url is not null
            || url is not null && !CalendarSharing.Link(url, "/v1/calendar/feed/") || expectUrl && enabled && url is null) throw CalendarBad();
        return new(enabled, enabled ? projection : null, CalendarNumber(body, "expires_at"), url);
    }

    public async Task<CalendarFeed> GetCalendarFeedAsync(string identity, string calendarId, CancellationToken ct)
    {
        CalendarSegment(calendarId);
        using var binding = BindCalendar(identity, ct); await RequireCalendarAsync(ct).ConfigureAwait(false);
        return ParseFeed(await GetJsonAsync<JsonElement>("/v1/calendar/feeds/" + calendarId, identity, ct).ConfigureAwait(false), expectUrl: false);
    }

    public async Task<CalendarFeed> SetCalendarFeedAsync(string identity, string calendarId, bool enabled, string? projection, CancellationToken ct)
    {
        CalendarSegment(calendarId);
        if (enabled && !CalendarSharing.Projection(projection)) throw new ArgumentException("Choose what the link shows.");
        object body = enabled ? new { enabled = true, projection } : new { enabled = false };
        using var binding = BindCalendar(identity, ct); await RequireCalendarAsync(ct).ConfigureAwait(false);
        var feed = ParseFeed(await SendJsonAsync<JsonElement>(HttpMethod.Post, "/v1/calendar/feeds/" + calendarId, body, identity, ct).ConfigureAwait(false), expectUrl: true);
        if (feed.Enabled != enabled || enabled && feed.Projection != projection) throw CalendarBad();
        return feed;
    }

    public async Task<CalendarImportPreview> PreviewCalendarImportAsync(string identity, string? calendarId, byte[] ics, CancellationToken ct)
    {
        if (calendarId is not null) CalendarSegment(calendarId);
        if (ics.Length is 0 or > CalendarSharing.MaxImportBytes) throw new ArgumentException("Choose an .ics file up to 512 KB.");
        using var binding = BindCalendar(identity, ct); await RequireCalendarAsync(ct).ConfigureAwait(false);
        var body = await SendBytesAsync<JsonElement>(HttpMethod.Post, "/v1/calendar/import/preview" + (calendarId is null ? "" : "?calendar_id=" + calendarId),
            ics, "text/calendar", identity, ct).ConfigureAwait(false);
        if (body.ValueKind != JsonValueKind.Object || !body.TryGetProperty("events", out var events) || events.ValueKind != JsonValueKind.Array || events.GetArrayLength() > 1000)
            throw CalendarBad();
        IReadOnlyList<string> Strings(JsonElement value, string name) => !value.TryGetProperty(name, out var list) ? []
            : list.ValueKind == JsonValueKind.Array && list.GetArrayLength() <= 50
                ? list.EnumerateArray().Select(w => w.ValueKind == JsonValueKind.String ? w.GetString()! : throw CalendarBad()).ToArray() : throw CalendarBad();
        var entries = events.EnumerateArray().Select(e =>
        {
            if (e.ValueKind != JsonValueKind.Object || CalendarNumber(e, "index") is not { } index || index is < 0 or > 10_000) throw CalendarBad();
            return new CalendarImportEntry((int)index, CalendarText(e, "title"), CalendarFlag(e, "all_day"), CalendarText(e, "start"), CalendarText(e, "tzid"),
                CalendarText(e, "start_date"), CalendarText(e, "end_date"), CalendarText(e, "rrule"), Strings(e, "warnings"),
                !e.TryGetProperty("importable", out _) || CalendarFlag(e, "importable"));
        }).ToArray();
        var draft = CalendarText(body, "draft_id"); var calendar = CalendarText(body, "calendar_id");
        if (!CalendarSegmentValid(draft) || !CalendarSegmentValid(calendar) || calendarId is not null && calendar != calendarId
            || entries.Select(e => e.Index).Distinct().Count() != entries.Length) throw CalendarBad();
        return new(draft!, calendar!, entries, Strings(body, "warnings"));
    }

    public async Task<int> ConfirmCalendarImportAsync(string identity, string draftId, IReadOnlyList<int> select, string? calendarId, CancellationToken ct)
    {
        CalendarSegment(draftId); if (calendarId is not null) CalendarSegment(calendarId);
        if (select.Count is < 1 or > 200 || select.Any(i => i < 0) || select.Distinct().Count() != select.Count) throw new ArgumentException("Choose up to 200 events to import.");
        object body = calendarId is null ? new { select } : new { select, calendar_id = calendarId };
        using var binding = BindCalendar(identity, ct); await RequireCalendarAsync(ct).ConfigureAwait(false);
        var result = await SendJsonAsync<JsonElement>(HttpMethod.Post, "/v1/calendar/import/" + draftId + "/confirm", body, identity, ct).ConfigureAwait(false);
        if (result.ValueKind != JsonValueKind.Object || !result.TryGetProperty("created", out var created) || created.ValueKind != JsonValueKind.Array
            || created.GetArrayLength() > select.Count) throw CalendarBad();
        return created.GetArrayLength();
    }

    public async Task<CalendarGuestLink> CalendarGuestLinkAsync(string identity, string eventId, string attendeeId, CancellationToken ct)
    {
        CalendarSegment(eventId); CalendarSegment(attendeeId);
        using var binding = BindCalendar(identity, ct); await RequireCalendarAsync(ct).ConfigureAwait(false);
        var body = await SendJsonAsync<JsonElement>(HttpMethod.Post, "/v1/calendar/events/" + eventId + "/attendees/" + attendeeId + "/link", new { }, identity, ct).ConfigureAwait(false);
        if (body.ValueKind != JsonValueKind.Object || CalendarText(body, "url") is not { } url || !CalendarSharing.Link(url, "/m/")) throw CalendarBad();
        return new(url, CalendarNumber(body, "expires_at"));
    }
}
