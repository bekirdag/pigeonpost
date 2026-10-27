using System.Text.Json;
using Pigeonpost.Core.Calendar;

internal static class CalendarDraftTests
{
    private static readonly Guid Context = Guid.NewGuid();
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return Case("Calendar create drafts preserve literal wall time, request identity and personal reminders", () =>
        {
            var draft = New(); var fields = draft.CreatePayload("reviewed_key").Fields();
            Require(fields.GetProperty("request_key").GetString() == "reviewed_key" && fields.GetProperty("start").GetString() == "2026-10-27T09:00"
                && fields.GetProperty("end").GetString() == "2026-10-27T10:00" && fields.GetProperty("tzid").GetString() == "Europe/Istanbul"
                && fields.GetProperty("reminders")[0].GetProperty("minutes_before").GetInt32() == 10);
            Require(!draft.ToString().Contains("Fixture", StringComparison.Ordinal));
        });
        yield return Case("All-day drafts display an inclusive last day and send an exclusive end date", () =>
        {
            var fields = (New() with { AllDay = true, LastDay = "2026-10-28" }).Fields().Fields();
            Require(fields.GetProperty("start_date").GetString() == "2026-10-27" && fields.GetProperty("end_date").GetString() == "2026-10-29" && !fields.TryGetProperty("tzid", out _));
            var target = Target(Event() with { AllDay = true, StartDate = "2026-10-27", EndDate = "2026-10-29" });
            Require(CalendarEventDraft.From(target, "UTC").LastDay == "2026-10-28");
        });
        yield return Case("A new calendar draft refuses a daylight-saving gap and requires a repeated-time choice", () =>
        {
            var gap = New() with { StartDay = "2026-03-08", LastDay = "2026-03-08", StartTime = "02:30", EndTime = "03:30", Tzid = "America/New_York" };
            Require(Error(() => gap.Fields()) is CalendarTimeException { Code: "nonexistent" });
            var fold = gap with { StartDay = "2026-11-01", LastDay = "2026-11-01", StartTime = "01:30", EndTime = "02:30" };
            Require(Error(() => fold.Fields()) is CalendarTimeException { Code: "ambiguous" });
            Require((fold with { Fold = CalendarFold.Later }).Fields().Fields().GetProperty("fold").GetString() == "later");
        });
        yield return Case("Title-only calendar edits do not resend or shift untouched historical timing", () =>
        {
            var target = Target(Event() with { Start = "2026-03-08T02:30", End = "2026-03-08T03:30", Tzid = "America/New_York" });
            var draft = CalendarEventDraft.From(target, "UTC") with { Title = "Reviewed title only" };
            var fields = draft.Fields().Fields(); Require(fields.EnumerateObject().Count() == 1 && fields.GetProperty("title").GetString() == "Reviewed title only");
        });
        yield return Case("A moved occurrence editor uses its projection and retains its original opaque identity", () =>
        {
            var start = new CalendarWallTime("2026-10-30T11:00", "Europe/Istanbul").Resolve();
            var end = new CalendarWallTime("2026-10-30T12:00", "Europe/Istanbul").Resolve();
            var key = new CalendarEventKey("evt_fixture", "occ_original_20261022T090000");
            var response = new CalendarEventResponse(Event(), new(OccurrenceId: key.OccurrenceId, StartsAt: start, EndsAt: end, Title: "Moved fixture"));
            var target = new CalendarEditTarget(response, key, CalendarEventScope.This, Context);
            var draft = CalendarEventDraft.From(target, "UTC"); Require(draft.StartDay == "2026-10-30" && draft.StartTime == "11:00:00" && draft.Target!.Key == key);
            var fields = (draft with { StartTime = "11:15:00" }).Fields().Fields();
            Require(fields.GetProperty("start").GetString() == "2026-10-30T11:15:00" && fields.TryGetProperty("end", out _) && fields.TryGetProperty("all_day", out _)
                && !fields.TryGetProperty("rrule", out _) && !fields.TryGetProperty("meeting", out _) && !fields.TryGetProperty("calendar_id", out _));
        });
        yield return Case("Editing a meeting preserves unknown policy while excluding response-only codes and capability URLs", () =>
        {
            using var raw = JsonDocument.Parse("{\"meeting\":{\"code\":\"LOCATOR_ONLY\",\"url\":\"https://fixture.invalid/#private_capability\",\"opens_at\":1,\"closes_at\":2,\"participant_limit\":6,\"future_policy\":{\"keep\":true}}}");
            var e = Event() with { Meeting = new(Code: "LOCATOR_ONLY", Waiting: "guests", Admit: "hosts", Transport: "mesh") };
            var response = new CalendarEventResponse(e) { Original = raw.RootElement.Clone() };
            var draft = CalendarEventDraft.From(new(response, new(e.Id), CalendarEventScope.All, Context), "UTC");
            var changed = draft with { Meeting = draft.Meeting with { Waiting = "everyone_except_hosts" } };
            var meeting = changed.Fields().Fields().GetProperty("meeting");
            Require(meeting.GetProperty("future_policy").GetProperty("keep").GetBoolean() && meeting.GetProperty("waiting").GetString() == "everyone_except_hosts"
                && !meeting.TryGetProperty("code", out _) && !meeting.TryGetProperty("url", out _) && !meeting.TryGetProperty("opens_at", out _) && !meeting.TryGetProperty("participant_limit", out _));
        });
        yield return Case("Calendar edits require organizer access, a reviewed revision and the same editor inbox context", () =>
        {
            Require(Error(() => Target(Event() with { My = new(Role: "future_role") })) is ArgumentException);
            Require(Error(() => Target(Event() with { Revision = null })) is ArgumentException);
            Require(Error(() => Target(Event() with { CanEdit = false })) is ArgumentException);
            var draft = CalendarEventDraft.From(Target(Event()), "UTC"); Require(Error(() => (draft with { ContextId = Guid.NewGuid(), Title = "Other inbox" }).Fields()) is ArgumentException);
        });
        yield return Case("Calendar create drafts bound guest kinds and required meeting capacity without granting a contact host role", () =>
        {
            var draft = New() with { Invitees = [new("guest@fixture.invalid", "Guest", Cohost: true), new("/fixture/person", Cohost: true)], Meeting = new(Enabled: true) };
            var people = draft.Fields().Fields().GetProperty("attendees");
            Require(people[0].TryGetProperty("contact", out _) && !people[0].TryGetProperty("role", out _) && people[1].GetProperty("role").GetString() == "cohost");
            var tooMany = draft with { Invitees = Enumerable.Range(0, 6).Select(i => new CalendarInviteeDraft("/fixture/person" + i)).ToArray() };
            Require(Error(() => tooMany.Fields()) is ArgumentException);
            Require((tooMany with { Meeting = new(Enabled: true, Transport: "sfu") }).Fields().Fields().GetProperty("attendees").GetArrayLength() == 6);
        });
        yield return Case("Calendar drafts refuse malformed reminders, recurrence headers, empty titles and all-day calls", () =>
        {
            foreach (var text in new[] { "1,1", "-1", "1,", "1,2,3,4,5,6", "40321", "1.5" }) Require(Error(() => CalendarReminderInput.Parse(text)) is ArgumentException);
            Require(CalendarReminderInput.Parse("0, 10, 40320").SequenceEqual([0, 10, 40320]) && CalendarReminderInput.Parse("").Count == 0);
            Require(Error(() => (New() with { Rrule = "RRULE:FREQ=DAILY" }).Fields()) is ArgumentException);
            Require(Error(() => (New() with { Title = " " }).Fields()) is ArgumentException);
            Require(Error(() => (New() with { AllDay = true, Meeting = new(Enabled: true) }).Fields()) is ArgumentException);
        });
        yield return Case("Unchanged unknown calendar fields stay untouched rather than being replaced by editor defaults", () =>
        {
            var draft = CalendarEventDraft.From(Target(Event() with { Privacy = "future_privacy", Transparency = "future_transparency", Locale = "future_locale" }), "UTC") with { Title = "Title edit" };
            var fields = draft.Fields().Fields(); Require(fields.EnumerateObject().Count() == 1 && fields.TryGetProperty("title", out _));
        });
    }
    private static CalendarEventDraft New() => new(Context, "2026-10-27", "2026-10-27", "Europe/Istanbul") { Title = "Fixture event" };
    private static ScheduledEvent Event() => new("evt_fixture", Revision: 3, Title: "Original fixture", AllDay: false, Start: "2026-10-27T09:00", End: "2026-10-27T10:00", Tzid: "Europe/Istanbul", My: new(Role: "organizer"), CanEdit: true);
    private static CalendarEditTarget Target(ScheduledEvent e) => new(new(e), new(e.Id), CalendarEventScope.All, Context);
    private static (string, Func<Task>) Case(string name, Action run) => (name, () => { run(); return Task.CompletedTask; });
    private static Exception Error(Action work) { try { work(); } catch (ArgumentException error) { return error; } throw new Exception("Expected a draft validation error."); }
    private static void Require(bool value) { if (!value) throw new Exception("Calendar draft assertion failed."); }
}
