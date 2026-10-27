using System.Text;
using System.Text.Json;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Core;

public sealed partial class PostboxClient : ICalendarActionsService
{
    private static string CalendarScope(CalendarEventScope scope) => scope switch
    { CalendarEventScope.All => "all", CalendarEventScope.This => "this", CalendarEventScope.Following => "following", _ => throw new ArgumentException("Choose a valid occurrence scope.") };
    private static string CalendarAnswerScopeName(CalendarAnswerScope scope) => scope switch
    { CalendarAnswerScope.All => "all", CalendarAnswerScope.This => "this", _ => throw new ArgumentException("Choose a valid answer scope.") };
    private static Dictionary<string, JsonElement> CalendarActionFields(CalendarEventKey key, string scope, CalendarWritePayload? changes = null)
    {
        CalendarSegment(key.EventId);
        if (scope != "all" && key.OccurrenceId is null) throw new ArgumentException("Choose an occurrence for this change.");
        if (key.OccurrenceId is { } occurrence) CalendarSegment(occurrence);
        var fields = changes?.Fields().EnumerateObject().ToDictionary(p => p.Name, p => p.Value.Clone(), StringComparer.Ordinal) ?? [];
        if (new[] { "scope", "occurrence_id", "expected_revision", "request_key", "identity" }.Any(fields.ContainsKey))
            throw new ArgumentException("Calendar changes cannot replace their reviewed scope or revision.");
        fields["scope"] = JsonSerializer.SerializeToElement(scope);
        if (key.OccurrenceId is { } selected) fields["occurrence_id"] = JsonSerializer.SerializeToElement(selected);
        return fields;
    }
    private static void CalendarRevision(Dictionary<string, JsonElement> fields, long expectedRevision)
    { if (expectedRevision < 0) throw new ArgumentException("This calendar revision is invalid."); fields["expected_revision"] = JsonSerializer.SerializeToElement(expectedRevision); }
    private static string CalendarItemPath(CalendarEventKey key) => "/v1/calendar/events/" + CalendarSegment(key.EventId);

    public async Task<CalendarEventResponse> UpdateCalendarEventAsync(string identity, CalendarEventKey key, long expectedRevision,
        CalendarEventScope scope, CalendarWritePayload changes, CancellationToken cancellationToken = default)
    {
        var fields = CalendarActionFields(key, CalendarScope(scope), changes); CalendarRevision(fields, expectedRevision);
        var body = CalendarWritePayload.From(fields);
        using var binding = BindCalendar(identity, cancellationToken); await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        var owner = new RequestAuthorization(tokens);
        var raw = await SendJsonAsync<JsonElement>(HttpMethod.Patch, CalendarItemPath(key), body.Fields(), identity, cancellationToken).ConfigureAwait(false);
        var response = ParseCalendarEvent(raw); owner.Check(cancellationToken);
        if (response.Event is null || !CalendarSegmentValid(response.Event.Id)
            || scope == CalendarEventScope.Following && response.SplitFrom?.EventId != key.EventId
            || scope != CalendarEventScope.Following && response.Event.Id != key.EventId) throw new PostboxException(502, "bad_response");
        return response;
    }
    public async Task CancelCalendarEventAsync(string identity, CalendarEventKey key, long expectedRevision, CalendarEventScope scope,
        string? message = null, CancellationToken cancellationToken = default)
    {
        var fields = CalendarActionFields(key, CalendarScope(scope)); CalendarRevision(fields, expectedRevision);
        if (message is not null) fields["message"] = JsonSerializer.SerializeToElement(message);
        var body = CalendarWritePayload.From(fields);
        using var binding = BindCalendar(identity, cancellationToken); await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        await SendAsync(HttpMethod.Post, CalendarItemPath(key) + "/cancel", body.Fields(), identity, cancellationToken).ConfigureAwait(false);
    }
    public async Task AnswerCalendarInvitationAsync(string identity, CalendarEventKey key, CalendarAnswer answer, CalendarAnswerScope scope,
        string? note = null, CancellationToken cancellationToken = default)
    {
        var status = answer switch { CalendarAnswer.Accepted => "accepted", CalendarAnswer.Tentative => "tentative", CalendarAnswer.Declined => "declined", _ => throw new ArgumentException("Choose a valid invitation response.") };
        if (note is not null && note.EnumerateRunes().Take(301).Count() > 300) throw new ArgumentException("Keep your response note within 300 characters.");
        var fields = CalendarActionFields(key, CalendarAnswerScopeName(scope)); fields["status"] = JsonSerializer.SerializeToElement(status);
        if (note is not null) fields["note"] = JsonSerializer.SerializeToElement(note);
        var body = CalendarWritePayload.From(fields);
        using var binding = BindCalendar(identity, cancellationToken); await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        await SendAsync(HttpMethod.Post, CalendarItemPath(key) + "/rsvp", body.Fields(), identity, cancellationToken).ConfigureAwait(false);
    }
    public async Task SaveCalendarRemindersAsync(string identity, string eventId, IReadOnlyList<CalendarReminder> reminders, CancellationToken cancellationToken = default)
    {
        CalendarSegment(eventId); var frozen = reminders.ToArray();
        if (frozen.Length > 5 || frozen.Any(r => r is null || r.MinutesBefore is < 0 or > 40320 || r.Channel is not (null or "push")))
            throw new ArgumentException("Choose up to five push reminders within four weeks.");
        var body = CalendarWritePayload.From(new { reminders = frozen.Select(r => new { minutes_before = r.MinutesBefore, channel = "push" }).ToArray() });
        using var binding = BindCalendar(identity, cancellationToken); await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        await SendAsync(HttpMethod.Put, "/v1/calendar/events/" + eventId + "/reminders", body.Fields(), identity, cancellationToken).ConfigureAwait(false);
    }
    public async Task<CalendarSettings> SaveCalendarSettingsAsync(string identity, CalendarWritePayload fields, CancellationToken cancellationToken = default)
    {
        var body = fields.Fields();
        if (body.EnumerateObject().Any(p => p.Name is not ("previews" or "timezone" or "week_start" or "working_hours"))) throw new ArgumentException("Choose valid calendar settings.");
        if (body.TryGetProperty("previews", out var previews) && previews.ValueKind is not (JsonValueKind.True or JsonValueKind.False)) throw new ArgumentException("Choose a valid preview setting.");
        if (body.TryGetProperty("timezone", out var timezone))
        { if (timezone.ValueKind != JsonValueKind.String) throw new ArgumentException("Choose a valid time zone."); _ = CalendarTime.Zone(timezone.GetString()!); }
        if (body.TryGetProperty("week_start", out var week) && (week.ValueKind != JsonValueKind.Number || !week.TryGetInt32(out var start) || start is < 0 or > 6)) throw new ArgumentException("Choose a valid first weekday.");
        using var binding = BindCalendar(identity, cancellationToken); await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        return await SendJsonAsync<CalendarSettings>(HttpMethod.Put, "/v1/calendar/settings", body, identity, cancellationToken).ConfigureAwait(false);
    }
    public async Task<byte[]> ExportCalendarEventAsync(string identity, string eventId, CancellationToken cancellationToken = default)
    {
        CalendarSegment(eventId);
        using var binding = BindCalendar(identity, cancellationToken); await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        return await GetBytesAsync("/v1/calendar/events/" + eventId + ".ics", identity, cancellationToken, maxBytes: 512 * 1024).ConfigureAwait(false);
    }
}
