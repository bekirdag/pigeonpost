using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Core;

public sealed partial class PostboxClient : IProtectedCalendarService
{
    async Task<CalendarCreateReceipt> IProtectedCalendarService.SendPreparedCalendarAsync(MessageRecoveryScope scope,
        PreparedCalendarAttempt attempt, CancellationToken cancellationToken)
    {
        attempt.RequireScope(scope);
        using var binding = scope.BindForService(this);
        scope.RequireCurrent(cancellationToken);
        await RequireCalendarAsync(cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        var path = attempt.Kind switch { CalendarAttemptKind.Calendar => "/v1/calendars", CalendarAttemptKind.Event => "/v1/calendar/events",
            _ => throw new ArgumentException("Choose a valid calendar change.") };
        using var response = await RequestAsync(HttpMethod.Post, Scoped(path, scope.Mailbox), attempt.Payload.WireJson,
            cancellationToken, identity: scope.Mailbox).ConfigureAwait(false);
        if (attempt.Kind == CalendarAttemptKind.Calendar)
        {
            var result = await DecodeAsync<CalendarFolderCreated>(response, cancellationToken).ConfigureAwait(false);
            scope.RequireCurrent(cancellationToken);
            if (result.Calendar is null || !CalendarSegmentValid(result.Calendar.Id)) throw new PostboxException(502, "bad_response");
            return new(attempt.Kind, Calendar: result.Calendar);
        }
        var created = await DecodeAsync<CalendarEventResponse>(response, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        if (created.Event is null || !CalendarSegmentValid(created.Event.Id)) throw new PostboxException(502, "bad_response");
        return new(attempt.Kind, Event: created);
    }
    private sealed record CalendarFolderCreated(CalendarFolder? Calendar);
}
