using NodaTime;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Pigeonpost.Core.Calendar;

// Server projections are data. Unknown/absent roles and optional fields grant no action.
public sealed record CalendarFolder(string Id, string? Name = null, string? Color = null, bool? IsDefault = null, long? Revision = null);
public sealed record CalendarPerson(string? Address = null, string? Handle = null, string? Name = null);
public sealed record CalendarReminder(int MinutesBefore, string? Channel = null);
public sealed record CalendarEventKey(string EventId, string? OccurrenceId = null);
public sealed record ScheduledMeeting(bool? Enabled = null, string? Code = null, string? Url = null,
    long? OpensAt = null, long? ClosesAt = null, string? Waiting = null, string? Admit = null,
    bool? AllowKnock = null, int? JoinBeforeMinutes = null, bool? GuestsSeeList = null,
    string? Transport = null, int? ParticipantLimit = null, IReadOnlyList<string>? Admitters = null);
public sealed record CalendarOccurrence(string OccurrenceId, string EventId, string? RecurrenceKey = null,
    string? CalendarId = null, string? Color = null, bool? AllDay = null, long? StartsAt = null, long? EndsAt = null,
    string? Tzid = null, string? StartDate = null, string? EndDate = null, string? Title = null, string? Location = null,
    string? Status = null, bool? Recurring = null, bool? Overridden = null, CalendarPerson? Organizer = null,
    string? Role = null, string? MyRsvp = null, bool? Invited = null, ScheduledMeeting? Meeting = null)
{
    public CalendarEventKey Key => new(EventId, OccurrenceId);
    public bool OccursOn(CalendarDay day, DateTimeZone zone)
    {
        try
        {
            if (AllDay == true) return StartDate is not null && EndDate is not null
                && CalendarDay.Parse(StartDate).Date <= day.Date && day.Date < CalendarDay.Parse(EndDate).Date;
            if (AllDay != false || StartsAt is not { } first || EndsAt is not { } last || last <= first) return false;
            var span = day.Interval(zone); return first < span.End && last > span.Start;
        }
        catch (ArgumentException) { return false; }
    }
}
public sealed record CalendarFoldersResponse(IReadOnlyList<CalendarFolder>? Calendars = null);
public sealed record CalendarOccurrencesResponse(IReadOnlyList<CalendarOccurrence>? Occurrences = null);

public sealed record CalendarAttendee(string AttendeeId, string? Kind = null, string? Address = null, string? Handle = null,
    string? Contact = null, string? Name = null, string? Role = null, bool? Optional = null, string? Rsvp = null,
    long? RsvpAt = null, string? Proof = null, string? Delivery = null, bool? Invited = null);
public sealed record CalendarMembership(string? Role = null, string? AttendeeId = null, string? Rsvp = null, string? CalendarId = null);
public sealed record ScheduledEvent(string Id, string? Uid = null, string? CalendarId = null, long? Revision = null,
    long? Sequence = null, string? Status = null, CalendarPerson? Organizer = null, string? Title = null,
    string? Description = null, string? Location = null, bool? AllDay = null, string? Start = null, string? End = null,
    string? Tzid = null, long? StartsAt = null, long? EndsAt = null, string? StartDate = null, string? EndDate = null,
    string? Rrule = null, IReadOnlyList<string>? Exdates = null, string? Transparency = null, string? Privacy = null,
    ScheduledMeeting? Meeting = null, IReadOnlyList<CalendarAttendee>? Attendees = null, IReadOnlyDictionary<string, int>? Counts = null,
    CalendarMembership? My = null, IReadOnlyList<CalendarReminder>? Reminders = null, string? LinkedGroup = null,
    bool? CanEdit = null, string? Locale = null)
{ public override string ToString() => "ScheduledEvent(redacted)"; }
public sealed record CalendarOccurrenceDetail(string? OccurrenceId = null, string? RecurrenceKey = null,
    long? StartsAt = null, long? EndsAt = null, bool? AllDay = null, string? Tzid = null,
    long? OpensAt = null, long? ClosesAt = null, string? Status = null, string? MyRsvp = null,
    string? Title = null, string? Description = null, string? Location = null, string? Start = null, string? End = null,
    string? StartDate = null, string? EndDate = null);
public sealed record CalendarSplit(string EventId, long? Revision = null);
public sealed record CalendarEventResponse(ScheduledEvent Event, CalendarOccurrenceDetail? Occurrence = null,
    string? DeliveryStatus = null, CalendarSplit? SplitFrom = null, IReadOnlyDictionary<string, int>? Carried = null,
    IReadOnlyDictionary<string, int>? Dropped = null)
{
    [JsonIgnore] public JsonElement Original { get; init; }
    public override string ToString() => "CalendarEventResponse(redacted)";
}
public sealed record CalendarInvitation(string EventId, string? OccurrenceId = null, string? Title = null,
    long? StartsAt = null, long? EndsAt = null, bool? AllDay = null, string? StartDate = null, string? EndDate = null,
    string? Tzid = null, CalendarPerson? Organizer = null, bool? Recurring = null, bool? Meeting = null, long? InvitedAt = null)
{ public CalendarEventKey Key => new(EventId, OccurrenceId); }
public sealed record CalendarInvitationsResponse(IReadOnlyList<CalendarInvitation>? Invitations = null);
public sealed record CalendarWorkingHours(string? Start = null, string? End = null, IReadOnlyList<int>? Days = null);
public sealed record CalendarSettings(bool? Previews = null, string? Timezone = null, int? WeekStart = null, CalendarWorkingHours? WorkingHours = null);
