namespace Pigeonpost.Core.Calendar;

public sealed record CalendarState(CalendarDay SelectedDay)
{
    public Guid ContextId { get; init; }
    public string? Mailbox { get; init; }
    public bool Enabled { get; init; }
    public bool EmailEnabled { get; init; }
    public IReadOnlyList<CalendarFolder> Folders { get; init; } = [];
    public IReadOnlyList<CalendarOccurrence> Occurrences { get; init; } = [];
    public IReadOnlyList<CalendarInvitation> Invitations { get; init; } = [];
    public IReadOnlyList<CalendarInvitation> FilteredInvitations { get; init; } = [];
    public CalendarSettings Settings { get; init; } = new();
    public string DisplayZone { get; init; } = "UTC";
    // null = all, empty = none. Never encode an explicit empty filter as the wire default.
    public IReadOnlySet<string>? SelectedCalendars { get; init; }
    public CalendarEventKey? OpenEvent { get; init; }
    public CalendarEventResponse? Detail { get; init; }
    public bool Loading { get; init; }
    public bool Busy { get; init; }
    public string? Error { get; init; }
    public IReadOnlyList<PreparedCalendarAttempt> Pending { get; init; } = [];
    public bool AttemptsLoaded { get; init; }
    public bool AttemptsUnreadable { get; init; }
    public bool DurableWrites { get; init; }
    public bool CanCreate => Enabled && !Busy && AttemptsLoaded && !AttemptsUnreadable && DurableWrites;
    public int WeekStart => Settings.WeekStart is >= 0 and <= 6 ? Settings.WeekStart.Value : 1;
    public IReadOnlyList<CalendarOccurrence> Agenda => Occurrences.Where(o => o.OccursOn(SelectedDay, CalendarTime.Zone(DisplayZone)))
        .OrderByDescending(o => o.AllDay == true).ThenBy(o => o.StartsAt ?? long.MinValue).ToArray();
    public override string ToString() => "CalendarState(redacted)";
}
