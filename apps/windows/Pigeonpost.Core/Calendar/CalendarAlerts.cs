using System.Text.Json;

namespace Pigeonpost.Core.Calendar;

public enum CalendarAlertKind { Invite, Reminder }

// A calendar account event from /v1/events (`event.invite`, `event.reminder`; see postbox calendar/events.rs and
// worker.rs). Locators only: the app re-reads the event under the mailbox's own authorization before showing
// anything, and every toast action is re-checked against the signed-in account.
public sealed record CalendarAlert(CalendarAlertKind Kind, string Mailbox, string EventId, string? OccurrenceId, bool Filtered, long? StartsAt)
{
    public CalendarEventKey Key => new(EventId, OccurrenceId);
    public override string ToString() => "CalendarAlert(redacted)";
}

public static class CalendarAlerts
{
    public static CalendarAlert? Parse(string? eventName, string? payload)
    {
        var kind = eventName switch { "event.invite" => CalendarAlertKind.Invite, "event.reminder" => CalendarAlertKind.Reminder, _ => (CalendarAlertKind?)null };
        if (kind is null || payload is null || payload.Length > 16384) return null;
        try
        {
            using var document = JsonDocument.Parse(payload);
            var root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object || !root.TryGetProperty("data", out var data) || data.ValueKind != JsonValueKind.Object) return null;
            string? Text(JsonElement value, string name) => value.TryGetProperty(name, out var field) && field.ValueKind == JsonValueKind.String ? field.GetString() : null;
            var mailbox = Text(root, "mailbox"); var eventId = Text(data, "event_id"); var occurrence = Text(data, "occurrence_id");
            if (mailbox is null || !MessageRecoveryScope.CanonicalMailbox(mailbox) || !CalendarSharing.Id(eventId) || occurrence is not null && !CalendarSharing.Id(occurrence)) return null;
            var filtered = data.TryGetProperty("filtered", out var f) && f.ValueKind == JsonValueKind.True;
            long? starts = data.TryGetProperty("starts_at", out var s) && s.ValueKind == JsonValueKind.Number && s.TryGetInt64(out var at) ? at : null;
            return new(kind.Value, mailbox, eventId!, occurrence, filtered, starts);
        }
        catch (JsonException) { return null; }
    }

    // The reminder's words, as the postbox's own push uses them (worker.rs plan_push_reminder): the title only when the
    // person's "Show event details in reminder notifications" setting is on.
    public static string ReminderText(string? title, bool previews, long? startsAt, DateTimeOffset now)
    {
        var minutes = startsAt is { } at ? (int)Math.Round((at - now.ToUnixTimeSeconds()) / 60.0) : 0;
        var lead = minutes <= 0 ? "Starting now" : minutes == 1 ? "Starting in 1 minute" : $"Starting in {minutes} minutes";
        return previews && !string.IsNullOrWhiteSpace(title) ? $"{title}: {lead}" : $"Event: {lead}";
    }

    // Which actions an invitation toast offers: only a pending, confirmed invitation the mailbox attends.
    public static bool Answerable(CalendarEventResponse detail) => detail.Event is { Status: "confirmed", My: { Role: "attendee" or "cohost", AttendeeId.Length: > 0 } my }
        && (detail.Occurrence?.MyRsvp ?? my.Rsvp) is null or "pending" or "needs_action";

    public static string Group(string mailbox, string eventId) => "calendar-" + MessageAlerts.Key(mailbox + "\n" + eventId);
}
