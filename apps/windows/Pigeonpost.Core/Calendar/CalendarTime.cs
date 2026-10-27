using System.Globalization;
using System.Runtime.CompilerServices;

namespace Pigeonpost.Core.Calendar;

public enum CalendarFold { Earlier, Later }
public sealed class CalendarTimeException(string code, long? earlier = null, long? later = null) : ArgumentException(code switch
{
    "zone_unknown" => "Choose a valid IANA time zone.",
    "nonexistent" => "That local time does not exist because the clocks change. Choose another time.",
    "ambiguous" => "That time happens twice. Choose the earlier or later offset.",
    "order" => "The end must be after the start.",
    _ => "Enter a valid date and time."
})
{
    public string Code { get; } = code;
    public long? Earlier { get; } = earlier;
    public long? Later { get; } = later;
}
public sealed record CalendarInterval
{
    public long Start { get; }
    public long End { get; }
    public CalendarInterval(long start, long end)
    { if (end <= start) throw new CalendarTimeException("order"); Start = start; End = end; }
}

public static class CalendarTime
{
    private static readonly ConditionalWeakTable<TimeZoneInfo, TimeSpan[]> offsets = new();
    public static TimeZoneInfo Zone(string id)
    {
        if (id.Length is < 1 or > 128 || id != "UTC" && !id.Contains('/')
            || id.Any(c => !char.IsAsciiLetterOrDigit(c) && c is not '/' and not '_' and not '-' and not '+'))
            throw new CalendarTimeException("zone_unknown");
        try { return TimeZoneInfo.FindSystemTimeZoneById(id); }
        catch (Exception ex) when (ex is TimeZoneNotFoundException or InvalidTimeZoneException) { throw new CalendarTimeException("zone_unknown"); }
    }
    internal static long Resolve(DateTime local, TimeZoneInfo zone, CalendarFold? fold)
    {
        // Round-trip all distinct rule offsets. IsInvalidTime alone can miss an entire
        // skipped date caused by a base-offset change (for example Apia, 2011-12-30).
        var possible = offsets.GetValue(zone, value => value.GetAdjustmentRules()
            .SelectMany(rule => new[] { value.BaseUtcOffset + rule.BaseUtcOffsetDelta,
                value.BaseUtcOffset + rule.BaseUtcOffsetDelta + rule.DaylightDelta })
            .Append(value.BaseUtcOffset).Distinct().ToArray());
        var valid = new HashSet<long>();
        foreach (var offset in possible)
        {
            try
            {
                var candidate = new DateTimeOffset(local, offset);
                if (TimeZoneInfo.ConvertTime(candidate, zone).DateTime == local) valid.Add(candidate.ToUnixTimeSeconds());
            }
            catch (ArgumentException) { } // A rule offset may fall outside DateTimeOffset's UTC range.
        }
        var candidates = valid.Order().ToArray();
        if (candidates.Length == 0) throw new CalendarTimeException("nonexistent");
        if (candidates.Length > 1)
        {
            return fold switch { CalendarFold.Earlier => candidates[0], CalendarFold.Later => candidates[^1],
                _ => throw new CalendarTimeException("ambiguous", candidates[0], candidates[^1]) };
        }
        return candidates[0];
    }
    internal static long DayBoundary(DateOnly day, TimeZoneInfo zone, bool rejectSkipped)
    {
        var local = day.ToDateTime(TimeOnly.MinValue, DateTimeKind.Unspecified);
        for (var minutes = 0; minutes <= 2880; minutes++)
        {
            if (rejectSkipped && DateOnly.FromDateTime(local) != day) throw new CalendarTimeException("nonexistent");
            try { return Resolve(local, zone, CalendarFold.Earlier); }
            catch (CalendarTimeException ex) when (ex.Code == "nonexistent") { }
            local = local.AddMinutes(1);
        }
        throw new CalendarTimeException("nonexistent");
    }
}

public sealed record CalendarDay(DateOnly Date)
{
    public string Text => Date.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture);
    public static CalendarDay Parse(string text) => text.Length == 10
        && DateOnly.TryParseExact(text, "yyyy-MM-dd", CultureInfo.InvariantCulture, DateTimeStyles.None, out var date)
        ? new(date) : throw new CalendarTimeException("invalid_format");
    public CalendarDay PlusDays(int days) => new(Date.AddDays(days));
    public long Start(TimeZoneInfo zone) => CalendarTime.DayBoundary(Date, zone, rejectSkipped: true);
    public CalendarInterval Interval(TimeZoneInfo zone) => new(Start(zone), CalendarTime.DayBoundary(Date.AddDays(1), zone, rejectSkipped: false));
    public override string ToString() => Text;
}
public sealed class CalendarWallTime
{
    public string Text { get; }
    public string Tzid { get; }
    public DateTime Local { get; }
    public TimeZoneInfo Zone { get; }
    public CalendarWallTime(string text, string tzid)
    {
        if (text.Length is not (16 or 19) || !DateTime.TryParseExact(text, ["yyyy-MM-dd'T'HH:mm", "yyyy-MM-dd'T'HH:mm:ss"],
            CultureInfo.InvariantCulture, DateTimeStyles.None, out var local)) throw new CalendarTimeException("invalid_format");
        Text = text; Tzid = tzid; Local = DateTime.SpecifyKind(local, DateTimeKind.Unspecified); Zone = CalendarTime.Zone(tzid);
    }
    public long Resolve(CalendarFold? fold = null) => CalendarTime.Resolve(Local, Zone, fold);
    public static CalendarInterval Range(string start, string end, string tzid, CalendarFold? fold = null) =>
        new(new CalendarWallTime(start, tzid).Resolve(fold), new CalendarWallTime(end, tzid).Resolve(fold));
    public override string ToString() => "CalendarWallTime(redacted)";
}
public sealed class CalendarMonthWindow
{
    public IReadOnlyList<CalendarDay> Days { get; }
    public CalendarMonthWindow(CalendarDay containing, int weekStart)
    {
        if (weekStart is < 0 or > 6) throw new CalendarTimeException("invalid_format");
        var first = new DateOnly(containing.Date.Year, containing.Date.Month, 1);
        var offset = ((int)first.DayOfWeek - weekStart + 7) % 7;
        Days = Enumerable.Range(0, 42).Select(i => new CalendarDay(first.AddDays(i - offset))).ToArray();
    }
    public CalendarInterval Interval(TimeZoneInfo zone) => new(Days[0].Start(zone), Days[^1].PlusDays(1).Start(zone));
}
