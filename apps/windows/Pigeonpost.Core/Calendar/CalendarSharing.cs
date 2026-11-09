using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace Pigeonpost.Core.Calendar;

// Audit P2-18 / P3-7 on Windows: subscription links (feeds), .ics import and a guest's own invitation link.
// Same routes and rules as site-inbox/calendar.js: feeds and links are person-only; an import is previewed
// first and only the entries the person ticks are created; nothing in the file is fetched and no one is invited.
public sealed record CalendarFeed(bool Enabled, string? Projection = null, long? ExpiresAt = null, string? Url = null)
{
    public override string ToString() => "CalendarFeed(redacted)";
}
public sealed record CalendarImportEntry(int Index, string? Title, bool AllDay, string? Start, string? Tzid, string? StartDate, string? EndDate,
    string? Rrule, IReadOnlyList<string> Warnings, bool Importable)
{
    public string Label
    {
        get
        {
            string when;
            if (AllDay)
            {
                var last = EndDate;
                try { if (EndDate is not null) last = CalendarDay.Parse(EndDate).PlusDays(-1).Text; } catch (ArgumentException) { }
                when = StartDate + (last is not null && last != StartDate ? " – " + last : "");
            }
            else when = (Start ?? "").Replace('T', ' ') + (Tzid is { Length: > 0 } ? " " + Tzid : "");
            return (string.IsNullOrWhiteSpace(Title) ? "(No title)" : Title) + " — " + when + (Rrule is { Length: > 0 } ? " · repeats" : "");
        }
    }
}
public sealed record CalendarImportPreview(string DraftId, string CalendarId, IReadOnlyList<CalendarImportEntry> Events, IReadOnlyList<string> Warnings);
public sealed record CalendarGuestLink(string Url, long? ExpiresAt);

public interface ICalendarSharingService
{
    Task<CalendarFeed> GetCalendarFeedAsync(string identity, string calendarId, CancellationToken ct);
    Task<CalendarFeed> SetCalendarFeedAsync(string identity, string calendarId, bool enabled, string? projection, CancellationToken ct);
    Task<CalendarImportPreview> PreviewCalendarImportAsync(string identity, string? calendarId, byte[] ics, CancellationToken ct);
    Task<int> ConfirmCalendarImportAsync(string identity, string draftId, IReadOnlyList<int> select, string? calendarId, CancellationToken ct);
    Task<CalendarGuestLink> CalendarGuestLinkAsync(string identity, string eventId, string attendeeId, CancellationToken ct);
}

public static class CalendarSharing
{
    public const int MaxImportBytes = 512 * 1024;
    public static bool Projection(string? value) => value is "busy" or "personal_redacted";
    internal static bool Id(string? value) => value is { Length: >= 1 and <= 128 } && value.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or '-');
    // Feed and guest links are served by the postbox or inbox origins only (https, no credentials in the authority).
    internal static bool Link(string? value, string pathPrefix) => Uri.TryCreate(value, UriKind.Absolute, out var uri)
        && uri.Scheme == Uri.UriSchemeHttps && uri.UserInfo.Length == 0 && uri.AbsolutePath.StartsWith(pathPrefix, StringComparison.Ordinal) && value!.Length <= 2048;
}

public sealed partial class CalendarStore
{
    // Sharing calls never retire the calendar view on a refusal (a feed, an import or a link failing is not a reason to
    // sign the calendar out); the error lands in State.Error for the dialog to show.
    private async Task<T?> Share<T>(CalendarActionTarget target, Func<Operation, ICalendarSharingService, Task<T>> work, CancellationToken ct) where T : class
    {
        using var operation = Begin(target);
        if (operation is null || api is not ICalendarSharingService sharing || state.Busy) return null;
        try
        {
            operation.Local(() => state = state with { Error = null }); Publish();
            await VerifyInbox(operation, ct);
            var result = await work(operation, sharing); operation.Check(); return result;
        }
        catch (OperationCanceledException) { return null; }
        catch (Exception error)
        {
            if (operation.IsCurrent)
            { operation.Local(() => state = state with { Error = SharingError(error) }); Publish(); }
            return null;
        }
    }

    private static string SharingError(Exception error) => error switch
    {
        ArgumentException => error.Message,
        PostboxException { Code: "import_invalid" } => "This file could not be read as a calendar (.ics) file.",
        PostboxException { Code: "too_large" } => "That file is larger than 512 KB.",
        PostboxException { Code: "calendar_unavailable" } => "That calendar is no longer available. Refresh and try again.",
        PostboxException { Code: "event_cancelled" } => "This event was cancelled.",
        PostboxException { Code: "not_found" } => "This import expired or the event changed. Start again.",
        _ => Describe(error),
    };

    public Task<CalendarFeed?> FeedAsync(CalendarActionTarget target, string calendarId, CancellationToken ct = default) =>
        Share(target, (operation, sharing) => sharing.GetCalendarFeedAsync(operation.Owner.Mailbox, calendarId, ct), ct);
    public Task<CalendarFeed?> SetFeedAsync(CalendarActionTarget target, string calendarId, bool enabled, string? projection, CancellationToken ct = default) =>
        Share(target, (operation, sharing) => sharing.SetCalendarFeedAsync(operation.Owner.Mailbox, calendarId, enabled, projection, ct), ct);
    public Task<CalendarImportPreview?> PreviewImportAsync(CalendarActionTarget target, string? calendarId, byte[] ics, CancellationToken ct = default) =>
        Share(target, (operation, sharing) => sharing.PreviewCalendarImportAsync(operation.Owner.Mailbox, calendarId, ics, ct), ct);
    public async Task<int?> ConfirmImportAsync(CalendarActionTarget target, string draftId, IReadOnlyList<int> select, string? calendarId, CancellationToken ct = default)
    {
        var created = await Share(target, async (operation, sharing) => (object)await sharing.ConfirmCalendarImportAsync(operation.Owner.Mailbox, draftId, select, calendarId, ct), ct) as int?;
        if (created is not null && target.IsCurrent) await ReloadAsync(target, ct);
        return created;
    }
    public Task<CalendarGuestLink?> GuestLinkAsync(CalendarActionTarget target, CalendarEventKey key, string attendeeId, CancellationToken ct = default) =>
        Share(target, async (operation, sharing) =>
        {
            var fresh = await FreshDetail(operation, key, ct);
            if (fresh.Event.My?.Role != "organizer" || fresh.Event.Status != "confirmed"
                || fresh.Event.Attendees?.Any(a => a.AttendeeId == attendeeId && a.Kind == "external") != true) throw new PostboxException(403, "forbidden");
            return await sharing.CalendarGuestLinkAsync(operation.Owner.Mailbox, key.EventId, attendeeId, ct);
        }, ct);
}
