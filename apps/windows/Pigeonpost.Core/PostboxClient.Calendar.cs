using Pigeonpost.Core.Calendar;
using System.Text.Json;

namespace Pigeonpost.Core;

public interface ICalendarService
{
    AuthorizationLease? AcceptedAuthorization { get; }
    Task<IReadOnlyList<CalendarFolder>> GetCalendarsAsync(string identity, CancellationToken cancellationToken);
    Task<IReadOnlyList<CalendarOccurrence>> GetCalendarOccurrencesAsync(string identity, long from, long to, IReadOnlyList<string>? calendars, CancellationToken cancellationToken);
    Task<CalendarEventResponse> GetCalendarEventAsync(string identity, CalendarEventKey key, CancellationToken cancellationToken);
    Task<IReadOnlyList<CalendarInvitation>> GetCalendarInvitationsAsync(string identity, bool filtered, CancellationToken cancellationToken);
    Task<CalendarSettings> GetCalendarSettingsAsync(string identity, CancellationToken cancellationToken);
}

public sealed partial class PostboxClient : ICalendarService
{
    AuthorizationLease? ICalendarService.AcceptedAuthorization => AcceptedAuthorization;
    private AuthorizationContext BindCalendar(string identity, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (!MessageRecoveryScope.CanonicalMailbox(identity)) throw new ArgumentException("Choose a canonical calendar inbox.");
        return new AuthorizationContext(AcceptedAuthorization ?? throw new AuthorizationRetiredException());
    }
    private async Task RequireCalendarAsync(CancellationToken cancellationToken)
    {
        var features = await GetCapabilitiesAsync(cancellationToken).ConfigureAwait(false);
        if (!features.Has(ServerFeatures.Calendar)) throw new PostboxException(403, "feature_disabled");
    }
    private static bool CalendarSegmentValid(string? id) => id is { Length: >= 1 and <= 128 }
        && id.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or '-');
    private static string CalendarSegment(string id) => CalendarSegmentValid(id) ? id : throw new ArgumentException("This calendar item is invalid.");
    private static void CalendarWindow(long from, long to)
    {
        if (from < -62135596800L || to > 253402300799L || to <= from || to - from > 62L * 86400)
            throw new ArgumentException("Choose a calendar window of at most 62 days.");
    }
    private static string CalendarEventPath(CalendarEventKey key)
    {
        var path = "/v1/calendar/events/" + CalendarSegment(key.EventId);
        return key.OccurrenceId is { } occurrence ? path + "?occurrence_id=" + CalendarSegment(occurrence) : path;
    }
    public async Task<IReadOnlyList<CalendarFolder>> GetCalendarsAsync(string identity, CancellationToken cancellationToken = default)
    {
        using var binding = BindCalendar(identity, cancellationToken);
        await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        var response = await GetJsonAsync<CalendarFoldersResponse>("/v1/calendars", identity, cancellationToken).ConfigureAwait(false);
        var folders = response.Calendars ?? [];
        if (folders.Count > 20 || folders.Any(f => f is null || !CalendarSegmentValid(f.Id))
            || folders.Select(f => f.Id).Distinct(StringComparer.Ordinal).Count() != folders.Count) throw new PostboxException(502, "bad_response");
        return folders.ToArray();
    }
    public async Task<IReadOnlyList<CalendarOccurrence>> GetCalendarOccurrencesAsync(string identity, long from, long to,
        IReadOnlyList<string>? calendars = null, CancellationToken cancellationToken = default)
    {
        CalendarWindow(from, to);
        var chosen = calendars?.ToArray() ?? [];
        if (chosen.Length > 20) throw new ArgumentException("Choose at most 20 calendars.");
        foreach (var id in chosen) CalendarSegment(id);
        using var binding = BindCalendar(identity, cancellationToken);
        await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        var path = "/v1/calendar/events?from=" + from.ToString(System.Globalization.CultureInfo.InvariantCulture)
            + "&to=" + to.ToString(System.Globalization.CultureInfo.InvariantCulture) + "&include=invitations";
        if (chosen.Length > 0) path += "&calendars=" + Uri.EscapeDataString(string.Join(',', chosen.Distinct(StringComparer.Ordinal)));
        var response = await GetJsonAsync<CalendarOccurrencesResponse>(path, identity, cancellationToken).ConfigureAwait(false);
        var occurrences = response.Occurrences ?? [];
        if (occurrences.Count > 1000 || occurrences.Any(o => o is null || !CalendarSegmentValid(o.EventId) || !CalendarSegmentValid(o.OccurrenceId))
            || occurrences.Select(o => o.OccurrenceId).Distinct(StringComparer.Ordinal).Count() != occurrences.Count) throw new PostboxException(502, "bad_response");
        return occurrences.ToArray();
    }
    public async Task<CalendarEventResponse> GetCalendarEventAsync(string identity, CalendarEventKey key, CancellationToken cancellationToken = default)
    {
        var path = CalendarEventPath(key);
        using var binding = BindCalendar(identity, cancellationToken);
        var owner = new RequestAuthorization(tokens);
        await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        var raw = await GetJsonAsync<JsonElement>(path, identity, cancellationToken).ConfigureAwait(false);
        var response = ParseCalendarEvent(raw); owner.Check(cancellationToken);
        if (response.Event?.Id != key.EventId || key.OccurrenceId is { } selected && response.Occurrence?.OccurrenceId != selected)
            throw new PostboxException(502, "bad_response");
        return response;
    }
    private static CalendarEventResponse ParseCalendarEvent(JsonElement raw)
    {
        try
        {
            if (raw.ValueKind != JsonValueKind.Object || !raw.TryGetProperty("event", out var original) || original.ValueKind != JsonValueKind.Object)
                throw new PostboxException(502, "bad_response");
            var response = raw.Deserialize<CalendarEventResponse>(PostboxJson.Options) ?? throw new PostboxException(502, "bad_response");
            return response with { Original = original.Clone() };
        }
        catch (JsonException) { throw new PostboxException(502, "bad_response"); }
    }
    public async Task<IReadOnlyList<CalendarInvitation>> GetCalendarInvitationsAsync(string identity, bool filtered = false, CancellationToken cancellationToken = default)
    {
        using var binding = BindCalendar(identity, cancellationToken);
        await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        var response = await GetJsonAsync<CalendarInvitationsResponse>("/v1/calendar/invitations?state=" + (filtered ? "filtered" : "pending"), identity, cancellationToken).ConfigureAwait(false);
        var invitations = response.Invitations ?? [];
        if (invitations.Count > 1000 || invitations.Any(i => i is null || !CalendarSegmentValid(i.EventId)
            || i.OccurrenceId is not null && !CalendarSegmentValid(i.OccurrenceId))) throw new PostboxException(502, "bad_response");
        return invitations.ToArray();
    }
    public async Task<CalendarSettings> GetCalendarSettingsAsync(string identity, CancellationToken cancellationToken = default)
    {
        using var binding = BindCalendar(identity, cancellationToken);
        await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        return await GetJsonAsync<CalendarSettings>("/v1/calendar/settings", identity, cancellationToken).ConfigureAwait(false);
    }
}
