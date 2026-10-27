using System.Globalization;
using NodaTime;

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
    public static DateTimeZone Zone(string id)
    {
        if (id.Length is < 1 or > 128
            || id.Any(c => !char.IsAsciiLetterOrDigit(c) && c is not '/' and not '_' and not '-' and not '+'))
            throw new CalendarTimeException("zone_unknown");
        return DateTimeZoneProviders.Tzdb.GetZoneOrNull(id) ?? throw new CalendarTimeException("zone_unknown");
    }
    internal static long Resolve(DateTime local, DateTimeZone zone, CalendarFold? fold)
    {
        var mapping = zone.MapLocal(LocalDateTime.FromDateTime(local));
        if (mapping.Count == 0) throw new CalendarTimeException("nonexistent");
        var earlier = mapping.First().ToInstant().ToUnixTimeSeconds();
        if (mapping.Count > 1)
        {
            var later = mapping.Last().ToInstant().ToUnixTimeSeconds();
            return fold switch { CalendarFold.Earlier => earlier, CalendarFold.Later => later,
                _ => throw new CalendarTimeException("ambiguous", earlier, later) };
        }
        return earlier;
    }
    internal static long DayBoundary(DateOnly day, DateTimeZone zone, bool rejectSkipped)
    {
        var date = new LocalDate(day.Year, day.Month, day.Day);
        for (var skipped = 0; skipped <= 2; skipped++)
        {
            try { return zone.AtStartOfDay(date).ToInstant().ToUnixTimeSeconds(); }
            catch (SkippedTimeException) { if (rejectSkipped) throw new CalendarTimeException("nonexistent"); date = date.PlusDays(1); }
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
    public long Start(DateTimeZone zone) => CalendarTime.DayBoundary(Date, zone, rejectSkipped: true);
    public CalendarInterval Interval(DateTimeZone zone) => new(Start(zone), CalendarTime.DayBoundary(Date.AddDays(1), zone, rejectSkipped: false));
    public override string ToString() => Text;
}
public sealed class CalendarWallTime
{
    public string Text { get; }
    public string Tzid { get; }
    public DateTime Local { get; }
    public DateTimeZone Zone { get; }
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
    public CalendarInterval Interval(DateTimeZone zone) => new(Days[0].Start(zone), Days[^1].PlusDays(1).Start(zone));
}
