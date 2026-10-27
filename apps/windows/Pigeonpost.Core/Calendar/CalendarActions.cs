namespace Pigeonpost.Core.Calendar;

public enum CalendarEventScope { All, This, Following }
public enum CalendarAnswerScope { All, This }
public enum CalendarAnswer { Accepted, Tentative, Declined }

public interface ICalendarActionsService : ICalendarService
{
    Task<CalendarEventResponse> UpdateCalendarEventAsync(string identity, CalendarEventKey key, long expectedRevision,
        CalendarEventScope scope, CalendarWritePayload changes, CancellationToken cancellationToken);
    Task CancelCalendarEventAsync(string identity, CalendarEventKey key, long expectedRevision, CalendarEventScope scope,
        string? message, CancellationToken cancellationToken);
    Task AnswerCalendarInvitationAsync(string identity, CalendarEventKey key, CalendarAnswer answer, CalendarAnswerScope scope,
        string? note, CancellationToken cancellationToken);
    Task SaveCalendarRemindersAsync(string identity, string eventId, IReadOnlyList<CalendarReminder> reminders, CancellationToken cancellationToken);
    Task<CalendarSettings> SaveCalendarSettingsAsync(string identity, CalendarWritePayload fields, CancellationToken cancellationToken);
    Task<byte[]> ExportCalendarEventAsync(string identity, string eventId, CancellationToken cancellationToken);
}
