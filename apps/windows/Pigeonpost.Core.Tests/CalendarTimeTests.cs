using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

internal static class CalendarTimeTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return Case("Calendar wall-time authoring retains its literal IANA time and seconds", () =>
        {
            var time = new CalendarWallTime("2026-10-27T09:03:07", "Europe/Istanbul");
            Require(time.Text == "2026-10-27T09:03:07" && time.Tzid == "Europe/Istanbul"
                && time.Resolve() == DateTimeOffset.Parse("2026-10-27T06:03:07Z").ToUnixTimeSeconds());
        });
        yield return Case("Calendar authoring refuses a spring-forward gap without shifting its time", () =>
        { Require(Error(() => new CalendarWallTime("2026-03-08T02:30", "America/New_York").Resolve()).Code == "nonexistent"); });
        yield return Case("Calendar repeated times require an explicit earlier or later occurrence", () =>
        {
            var time = new CalendarWallTime("2026-11-01T01:30", "America/New_York");
            var error = Error(() => time.Resolve()); Require(error.Code == "ambiguous" && error.Later - error.Earlier == 3600);
            Require(time.Resolve(CalendarFold.Earlier) == error.Earlier && time.Resolve(CalendarFold.Later) == error.Later);
        });
        yield return Case("Local calendar days span 23 or 25 hours across daylight-saving changes", () =>
        {
            var zone = CalendarTime.Zone("America/New_York");
            var spring = CalendarDay.Parse("2026-03-08").Interval(zone); var fall = CalendarDay.Parse("2026-11-01").Interval(zone);
            Require(spring.End - spring.Start == 23 * 3600 && fall.End - fall.Start == 25 * 3600);
        });
        yield return Case("All-day occurrence ends remain exclusive dates across display zones", () =>
        {
            var occurrence = new CalendarOccurrence("occ_moved", "evt_fixture", AllDay: true, StartDate: "2026-10-27", EndDate: "2026-10-29");
            foreach (var id in new[] { "America/Los_Angeles", "Asia/Tokyo" })
            {
                var zone = CalendarTime.Zone(id);
                Require(occurrence.OccursOn(CalendarDay.Parse("2026-10-27"), zone) && occurrence.OccursOn(CalendarDay.Parse("2026-10-28"), zone)
                    && !occurrence.OccursOn(CalendarDay.Parse("2026-10-29"), zone));
            }
        });
        yield return Case("Timed occurrences use interval overlap and do not spill onto an exclusive midnight end", () =>
        {
            var range = CalendarWallTime.Range("2026-10-27T23:00", "2026-10-28T00:00", "Europe/Istanbul");
            var occurrence = new CalendarOccurrence("occ_opaque_original_key", "evt_fixture", AllDay: false, StartsAt: range.Start, EndsAt: range.End);
            Require(occurrence.OccursOn(CalendarDay.Parse("2026-10-27"), CalendarTime.Zone("Europe/Istanbul"))
                && !occurrence.OccursOn(CalendarDay.Parse("2026-10-28"), CalendarTime.Zone("Europe/Istanbul"))
                && occurrence.Key.OccurrenceId == "occ_opaque_original_key");
        });
        yield return Case("Calendar month grids follow the chosen weekday with a bounded local-time window", () =>
        {
            var month = new CalendarMonthWindow(CalendarDay.Parse("2026-11-18"), 1);
            Require(month.Days.Count == 42 && month.Days[0].Text == "2026-10-26" && month.Days[^1].Text == "2026-12-06");
            var span = month.Interval(CalendarTime.Zone("America/New_York")); Require(span.End - span.Start == 42 * 86400 + 3600);
        });
        yield return Case("Calendar authoring refuses invalid literals, unknown zones and inverted ranges", () =>
        {
            foreach (var text in new[] { "2026-2-01T09:00", "2026-02-30T09:00", "2026-10-27T09:00Z", "2026-10-27T24:00", "2026-10-27 09:00" })
                Require(Error(() => new CalendarWallTime(text, "UTC")).Code == "invalid_format");
            foreach (var zone in new[] { "", "Turkey Standard Time", "Not/AZone", "../UTC" }) Require(Error(() => CalendarTime.Zone(zone)).Code == "zone_unknown");
            Require(Error(() => CalendarWallTime.Range("2026-10-27T10:00", "2026-10-27T09:00", "UTC")).Code == "order");
            Require(Error(() => CalendarDay.Parse("2026-2-01")).Code == "invalid_format");
        });
        yield return Case("A whole skipped calendar date is refused rather than becoming another day", () =>
        {
            var zone = CalendarTime.Zone("Pacific/Apia");
            Require(Error(() => CalendarDay.Parse("2011-12-30").Start(zone)).Code == "nonexistent");
            var prior = CalendarDay.Parse("2011-12-29").Interval(zone); Require(prior.End - prior.Start == 86400);
        });
        yield return Case("Calendar projections tolerate unknown optional data without inventing authority or changing occurrence identity", () =>
        {
            const string text = "{\"event\":{\"id\":\"evt_fixture\",\"my\":{\"role\":\"future_role\"},\"new_field\":true},\"occurrence\":{\"occurrence_id\":\"occ_original_key\",\"title\":\"Moved\"}}";
            var response = JsonSerializer.Deserialize<CalendarEventResponse>(text, PostboxJson.Options)!;
            Require(response.Event.CanEdit is null && response.Event.My?.Role == "future_role" && response.Occurrence?.OccurrenceId == "occ_original_key");
            var folders = JsonSerializer.Deserialize<CalendarFoldersResponse>("{\"calendars\":[{\"id\":\"cal_fixture\",\"is_default\":true}]}", PostboxJson.Options)!;
            Require(folders.Calendars?.Single().IsDefault == true && response.ToString() == "CalendarEventResponse(redacted)");
        });
    }
    private static (string, Func<Task>) Case(string name, Action run) => (name, () => { run(); return Task.CompletedTask; });
    private static CalendarTimeException Error(Action work)
    { try { work(); } catch (CalendarTimeException error) { return error; } throw new Exception("Expected a calendar time error."); }
    private static void Require(bool value) { if (!value) throw new Exception("Calendar time assertion failed."); }
}
