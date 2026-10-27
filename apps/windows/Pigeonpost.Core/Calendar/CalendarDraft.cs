using System.Globalization;
using System.Text;
using System.Text.Json;
using NodaTime;

namespace Pigeonpost.Core.Calendar;

// This is a reviewed data snapshot. The store still rechecks the original actor and server access.
public sealed class CalendarEditTarget
{
    public CalendarEventResponse Response { get; }
    public CalendarEventKey Key { get; }
    public CalendarEventScope Scope { get; }
    public Guid ContextId { get; }
    public long Revision => Response.Event.Revision!.Value;
    public CalendarEditTarget(CalendarEventResponse response, CalendarEventKey key, CalendarEventScope scope, Guid contextId)
    {
        if (response.Event.Id != key.EventId || response.Event.CanEdit != true || response.Event.My?.Role != "organizer"
            || response.Event.Revision is not >= 0 || !Enum.IsDefined(scope)
            || scope != CalendarEventScope.All && (key.OccurrenceId is null || response.Occurrence?.OccurrenceId != key.OccurrenceId))
            throw new ArgumentException("Reload this event and review its editing access.");
        Response = response; Key = key; Scope = scope; ContextId = contextId;
    }
    public override string ToString() => "CalendarEditTarget(redacted)";
}
public static class CalendarReminderInput
{
    public static IReadOnlyList<int> Parse(string text)
    {
        if (string.IsNullOrWhiteSpace(text)) return [];
        var parts = text.Split(',').Select(s => s.Trim()).ToArray();
        const string error = "Enter up to five distinct whole minutes between 0 and 40320, separated by commas.";
        if (parts.Length > 5) throw new ArgumentException(error);
        var values = parts.Select(s => s.Length > 0 && s.All(char.IsAsciiDigit) && int.TryParse(s, NumberStyles.None, CultureInfo.InvariantCulture, out var value)
            && value is >= 0 and <= 40320 ? value : throw new ArgumentException(error)).ToArray();
        if (values.Distinct().Count() != values.Length) throw new ArgumentException(error); return values;
    }
}
public sealed record CalendarInviteeDraft(string Destination = "", string Name = "", bool Cohost = false, bool Optional = false)
{
    internal Dictionary<string, object?>? Fields()
    {
        var value = Destination.Trim(); if (value.Length == 0) return null;
        var handle = value.StartsWith('/');
        if (Encoding.UTF8.GetByteCount(value) > 512 || value.Any(c => char.IsWhiteSpace(c) || char.IsControl(c))
            || handle && !PostAddress.IsValid(value) || !handle && (!value.Contains('@') || value.StartsWith('@') || value.EndsWith('@')))
            throw new ArgumentException("Enter a Pigeonpost handle or a guest's contact address.");
        var result = new Dictionary<string, object?> { [handle ? "handle" : "contact"] = value };
        if (!string.IsNullOrWhiteSpace(Name)) result["name"] = Name;
        if (handle && Cohost) result["role"] = "cohost"; if (Optional) result["optional"] = true; return result;
    }
    public override string ToString() => "CalendarInviteeDraft(redacted)";
}
public sealed record CalendarMeetingDraft(bool Enabled = false, string Waiting = "guests", string Admit = "hosts", bool AllowKnock = false,
    int JoinBeforeMinutes = 10, bool GuestsSeeList = false, string Transport = "mesh", IReadOnlyList<string>? Admitters = null)
{
    private JsonElement original;
    internal Dictionary<string, object?> Fields(bool validate = true)
    {
        if (!Enabled) return new() { ["enabled"] = false };
        if (validate && (Waiting is not ("guests" or "everyone_except_hosts" or "none") || Admit is not ("organizer" or "hosts" or "delegated")
            || Transport is not ("mesh" or "sfu") || JoinBeforeMinutes is < 0 or > 10)) throw new ArgumentException("Review the meeting's lobby and admission settings.");
        var result = new Dictionary<string, object?>();
        if (original.ValueKind == JsonValueKind.Object)
            foreach (var field in original.EnumerateObject())
                if (field.Name is not ("code" or "url" or "opens_at" or "closes_at" or "participant_limit")) result[field.Name] = field.Value.Clone();
        result["enabled"] = true; result["waiting"] = Waiting; result["admit"] = Admit; result["allow_knock"] = AllowKnock;
        result["join_before_minutes"] = JoinBeforeMinutes; result["guests_see_list"] = GuestsSeeList; result["transport"] = Transport; result["admitters"] = Admitters?.ToArray() ?? [];
        return result;
    }
    internal static CalendarMeetingDraft From(ScheduledMeeting? value, JsonElement original) => new(value?.Enabled ?? !string.IsNullOrEmpty(value?.Code), value?.Waiting ?? "guests",
        value?.Admit ?? "hosts", value?.AllowKnock ?? false, value?.JoinBeforeMinutes ?? 10, value?.GuestsSeeList ?? false, value?.Transport ?? "mesh", value?.Admitters?.ToArray()) { original = original.ValueKind == JsonValueKind.Object ? original.Clone() : default };
    public override string ToString() => "CalendarMeetingDraft(redacted)";
}

public sealed record CalendarEventDraft(Guid ContextId, string StartDay, string LastDay, string Tzid)
{
    public string Title { get; init; } = "";
    public string Description { get; init; } = "";
    public string Location { get; init; } = "";
    public string? CalendarId { get; init; }
    public bool AllDay { get; init; }
    public string StartTime { get; init; } = "09:00";
    public string EndTime { get; init; } = "10:00";
    public CalendarFold? Fold { get; init; }
    public string Rrule { get; init; } = "";
    public string Transparency { get; init; } = "opaque";
    public string Privacy { get; init; } = "default";
    public string Locale { get; init; } = "en";
    public CalendarMeetingDraft Meeting { get; init; } = new();
    public IReadOnlyList<CalendarInviteeDraft> Invitees { get; init; } = [];
    public string ReminderText { get; init; } = "10";
    public CalendarEditTarget? Target { get; init; }
    private JsonElement original;
    public bool IsOccurrenceEdit => Target?.Scope == CalendarEventScope.This;
    private JsonElement Authored()
    {
        var fields = new Dictionary<string, object?> { ["title"] = Title.Trim(), ["description"] = Description, ["location"] = Location, ["all_day"] = AllDay };
        if (AllDay) { fields["start_date"] = CalendarDay.Parse(StartDay).Text; fields["end_date"] = CalendarDay.Parse(LastDay).PlusDays(1).Text; }
        else { fields["start"] = StartDay + "T" + StartTime; fields["end"] = LastDay + "T" + EndTime; fields["tzid"] = Tzid; if (Fold is { } fold) fields["fold"] = fold == CalendarFold.Earlier ? "earlier" : "later"; }
        if (!IsOccurrenceEdit)
        {
            if (CalendarId is not null) fields["calendar_id"] = CalendarId;
            fields["rrule"] = string.IsNullOrWhiteSpace(Rrule) ? null : Rrule.Trim(); fields["transparency"] = Transparency; fields["privacy"] = Privacy;
            fields["locale"] = Locale; fields["meeting"] = Meeting.Fields(validate: false);
        }
        return CalendarWritePayload.From(fields).Fields();
    }
    public JsonElement ChangedFields()
    {
        var authored = Authored(); if (Target is null) return authored;
        if (Target.ContextId != ContextId) throw new ArgumentException("Reopen the editor in the current inbox.");
        var changed = authored.EnumerateObject().Where(p => !original.TryGetProperty(p.Name, out var before) || !JsonElement.DeepEquals(before, p.Value))
            .ToDictionary(p => p.Name, p => (object?)p.Value.Clone(), StringComparer.Ordinal);
        var timing = new[] { "all_day", "start", "end", "start_date", "end_date", "tzid", "fold" };
        if (timing.Any(changed.ContainsKey)) foreach (var field in authored.EnumerateObject().Where(p => timing.Contains(p.Name))) changed[field.Name] = field.Value.Clone();
        return CalendarWritePayload.From(changed).Fields();
    }
    public CalendarWritePayload Fields()
    {
        var changed = ChangedFields();
        var fields = changed.EnumerateObject().ToDictionary(p => p.Name, p => (object?)p.Value.Clone(), StringComparer.Ordinal);
        if ((Target is null || fields.ContainsKey("title")) && (string.IsNullOrWhiteSpace(Title) || Title.Length > 300)) throw new ArgumentException("Give the event a title of at most 300 characters.");
        if (Target is null || fields.ContainsKey("all_day"))
        {
            if (AllDay) { if (CalendarDay.Parse(LastDay).Date < CalendarDay.Parse(StartDay).Date) throw new CalendarTimeException("order"); }
            else _ = CalendarWallTime.Range(StartDay + "T" + StartTime, LastDay + "T" + EndTime, Tzid, Fold);
            if (AllDay && Meeting.Enabled) throw new ArgumentException("Choose a start and end time for a call.");
        }
        if (fields.ContainsKey("rrule") && (Rrule.Trim().StartsWith("RRULE:", StringComparison.OrdinalIgnoreCase) || Rrule.Contains('\n') || Rrule.Contains('\r') || Rrule.Length > 1000)) throw new ArgumentException("Enter a recurrence rule without the RRULE: prefix.");
        if (fields.ContainsKey("meeting")) fields["meeting"] = Meeting.Fields();
        if (fields.ContainsKey("privacy") && Privacy is not ("default" or "private")) throw new ArgumentException("Choose a valid event privacy setting.");
        if (fields.ContainsKey("transparency") && Transparency is not ("opaque" or "transparent")) throw new ArgumentException("Choose whether the event blocks your time.");
        if (fields.ContainsKey("locale") && Locale is not ("en" or "tr")) throw new ArgumentException("Choose English or Turkish for guest invitations.");
        if (Target is null)
        {
            var people = Invitees.Select(p => p.Fields()).Where(p => p is not null).Cast<Dictionary<string, object?>>().ToArray();
            if (people.Count(p => p.ContainsKey("handle")) > 500 || people.Count(p => p.ContainsKey("contact")) > 50) throw new ArgumentException("Choose at most 500 Pigeonpost invitees and 50 contact guests.");
            if (Meeting.Enabled && 1 + people.Count(p => !p.ContainsKey("optional")) > (Meeting.Transport == "sfu" ? 30 : 6)) throw new ArgumentException("The required invitees exceed this meeting's participant limit.");
            fields["attendees"] = people; fields["reminders"] = CalendarReminderInput.Parse(ReminderText).Select(n => new { minutes_before = n, channel = "push" }).ToArray();
        }
        return CalendarWritePayload.From(fields);
    }
    public CalendarWritePayload CreatePayload(string requestKey)
    {
        if (Target is not null) throw new ArgumentException("This draft is an edit.");
        var fields = Fields().Fields().EnumerateObject().ToDictionary(p => p.Name, p => (object?)p.Value.Clone(), StringComparer.Ordinal); fields["request_key"] = requestKey;
        var payload = CalendarWritePayload.From(fields); _ = payload.RequestKey(); return payload;
    }
    public static CalendarEventDraft From(CalendarEditTarget target, string fallbackZone)
    {
        var e = target.Response.Event; var o = target.Scope == CalendarEventScope.All ? null : target.Response.Occurrence;
        var allDay = o?.AllDay ?? e.AllDay ?? false; var zone = o?.Tzid ?? e.Tzid ?? fallbackZone;
        string? Wall(string? literal, long? at) => literal ?? (at is { } seconds ? Instant.FromUnixTimeSeconds(seconds).InZone(CalendarTime.Zone(zone)).LocalDateTime
            .ToDateTimeUnspecified().ToString("yyyy-MM-dd'T'HH:mm:ss", CultureInfo.InvariantCulture) : null);
        var first = allDay ? null : new CalendarWallTime(Wall(o?.Start, o?.StartsAt) ?? e.Start ?? "", zone);
        var last = allDay ? null : new CalendarWallTime(Wall(o?.End, o?.EndsAt) ?? e.End ?? "", zone);
        var choices = new List<CalendarFold>();
        void Infer(CalendarWallTime? wall, long? at)
        {
            if (wall is null) return;
            try { wall.Resolve(); }
            catch (CalendarTimeException error) when (error.Code == "ambiguous") { if (at == error.Earlier) choices.Add(CalendarFold.Earlier); else if (at == error.Later) choices.Add(CalendarFold.Later); }
            catch (CalendarTimeException error) when (error.Code == "nonexistent") { }
        }
        Infer(first, o?.StartsAt ?? e.StartsAt); Infer(last, o?.EndsAt ?? e.EndsAt);
        JsonElement meeting = default;
        if (target.Response.Original.ValueKind == JsonValueKind.Object) target.Response.Original.TryGetProperty("meeting", out meeting);
        var result = new CalendarEventDraft(target.ContextId, allDay ? CalendarDay.Parse(o?.StartDate ?? e.StartDate ?? "").Text : first!.Text[..10],
            allDay ? CalendarDay.Parse(o?.EndDate ?? e.EndDate ?? "").PlusDays(-1).Text : last!.Text[..10], zone)
        {
            Target = target, Title = o?.Title ?? e.Title ?? "", Description = o?.Description ?? e.Description ?? "", Location = o?.Location ?? e.Location ?? "", CalendarId = e.CalendarId,
            AllDay = allDay, StartTime = first?.Text[11..] ?? "09:00", EndTime = last?.Text[11..] ?? "10:00", Fold = choices.Distinct().Count() == 1 ? choices[0] : null,
            Rrule = e.Rrule ?? "", Transparency = e.Transparency ?? "opaque", Privacy = e.Privacy ?? "default", Locale = e.Locale ?? "en", Meeting = CalendarMeetingDraft.From(e.Meeting, meeting),
            ReminderText = string.Join(", ", e.Reminders?.Select(r => r.MinutesBefore) ?? [])
        };
        return result with { original = result.Authored() };
    }
    public override string ToString() => "CalendarEventDraft(redacted)";
}
