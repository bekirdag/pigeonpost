#if UI_TESTS
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    // Exercise bundled IANA data in the actual Windows binary as well as the Core fixtures.
    private static void RunCalendarRuntimeChecks(Action<bool, string> check)
    {
        var istanbul = new CalendarWallTime("2026-10-27T09:00", "Europe/Istanbul");
        check(istanbul.Resolve() == DateTimeOffset.Parse("2026-10-27T06:00:00Z").ToUnixTimeSeconds(), "Windows resolves literal IANA wall time without changing it");
        var gapRefused = false;
        try { new CalendarWallTime("2026-03-08T02:30", "America/New_York").Resolve(); }
        catch (CalendarTimeException ex) when (ex.Code == "nonexistent") { gapRefused = true; }
        check(gapRefused, "Windows refuses a daylight-saving gap rather than shifting its time");
        var repeated = new CalendarWallTime("2026-11-01T01:30", "America/New_York");
        var foldRequired = false;
        try { repeated.Resolve(); }
        catch (CalendarTimeException ex) when (ex.Code == "ambiguous") { foldRequired = true; }
        check(foldRequired && repeated.Resolve(CalendarFold.Later) - repeated.Resolve(CalendarFold.Earlier) == 3600,
            "Windows requires an explicit earlier/later choice for a repeated time");
        var day = CalendarDay.Parse("2026-03-08").Interval(CalendarTime.Zone("America/New_York"));
        check(day.End - day.Start == 23 * 3600, "Windows calendar day boundaries retain the actual daylight-saving interval");
        var skippedRefused = false;
        try { CalendarDay.Parse("2011-12-30").Start(CalendarTime.Zone("Pacific/Apia")); }
        catch (CalendarTimeException ex) when (ex.Code == "nonexistent") { skippedRefused = true; }
        check(skippedRefused, "Windows refuses a whole skipped date from the bundled IANA database");
    }
}
#endif
