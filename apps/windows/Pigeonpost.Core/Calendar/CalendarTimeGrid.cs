using NodaTime;

namespace Pigeonpost.Core.Calendar;

public enum CalendarView { Month, Week, Day }

// One timed occurrence placed on one day column of the week/day time grid (audit P2-18). A multi-day event gets one
// block per day it touches. Minutes are wall-clock minutes from that day's local midnight (0–1440); overlapping
// blocks share the column side by side (Lane of Lanes).
public sealed record CalendarGridBlock(CalendarOccurrence Row, int Day, double StartMinute, double EndMinute, int Lane, int Lanes, bool ContinuesBefore, bool ContinuesAfter);

public static class CalendarTimeGrid
{
    public const double MinimumMinutes = 20;

    public static IReadOnlyList<CalendarDay> Days(CalendarView view, CalendarDay selected, int weekStart)
    {
        if (view == CalendarView.Day) return [selected];
        var offset = ((int)selected.Date.DayOfWeek - weekStart + 7) % 7;
        var first = selected.PlusDays(-offset);
        return Enumerable.Range(0, 7).Select(first.PlusDays).ToArray();
    }

    // Minutes since local midnight on `day`, in `zone`, of an instant inside that day; clamped to 0–1440.
    private static double Minute(long at, CalendarDay day, DateTimeZone zone)
    {
        var span = day.Interval(zone);
        if (at <= span.Start) return 0;
        if (at >= span.End) return 1440;
        var local = Instant.FromUnixTimeSeconds(at).InZone(zone).LocalDateTime;
        var date = DateOnly.FromDateTime(local.ToDateTimeUnspecified());
        if (date != day.Date) return date < day.Date ? 0 : 1440;
        return local.Hour * 60 + local.Minute + local.Second / 60.0;
    }

    public static IReadOnlyList<CalendarOccurrence> AllDay(IEnumerable<CalendarOccurrence> rows, CalendarDay day, DateTimeZone zone) =>
        rows.Where(r => r.AllDay == true && r.OccursOn(day, zone)).OrderBy(r => r.Title, StringComparer.CurrentCulture).ToArray();

    public static IReadOnlyList<CalendarGridBlock> Layout(IEnumerable<CalendarOccurrence> rows, IReadOnlyList<CalendarDay> days, DateTimeZone zone)
    {
        var placed = new List<CalendarGridBlock>();
        var timed = rows.Where(r => r.AllDay == false && r.StartsAt is { } s && r.EndsAt is { } e && e > s).ToArray();
        for (var index = 0; index < days.Count; index++)
        {
            var day = days[index];
            CalendarInterval span;
            try { span = day.Interval(zone); }
            catch (ArgumentException) { continue; }
            var today = timed.Where(r => r.StartsAt < span.End && r.EndsAt > span.Start)
                .Select(r =>
                {
                    var start = Minute(r.StartsAt!.Value, day, zone);
                    var end = Math.Max(Minute(r.EndsAt!.Value, day, zone), Math.Min(1440, start + MinimumMinutes));
                    return (Row: r, Start: start, End: end, Before: r.StartsAt < span.Start, After: r.EndsAt > span.End);
                })
                .OrderBy(b => b.Start).ThenByDescending(b => b.End).ThenBy(b => b.Row.Title, StringComparer.Ordinal).ToList();
            // Overlap clusters: each block takes the first free lane; every block in a cluster shares its lane count.
            var cluster = new List<(CalendarOccurrence Row, double Start, double End, bool Before, bool After, int Lane)>();
            double clusterEnd = -1;
            void Flush()
            {
                var lanes = cluster.Count == 0 ? 0 : cluster.Max(c => c.Lane) + 1;
                placed.AddRange(cluster.Select(c => new CalendarGridBlock(c.Row, index, c.Start, c.End, c.Lane, lanes, c.Before, c.After)));
                cluster.Clear(); clusterEnd = -1;
            }
            foreach (var block in today)
            {
                if (cluster.Count > 0 && block.Start >= clusterEnd) Flush();
                var lane = 0;
                while (cluster.Any(c => c.Lane == lane && c.End > block.Start)) lane++;
                cluster.Add((block.Row, block.Start, block.End, block.Before, block.After, lane));
                clusterEnd = Math.Max(clusterEnd, block.End);
            }
            Flush();
        }
        return placed;
    }

    // The red "now" line: the column index and minute when now falls on one of the shown days, else null.
    public static (int Day, double Minute)? Now(Instant now, IReadOnlyList<CalendarDay> days, DateTimeZone zone)
    {
        var local = now.InZone(zone).LocalDateTime;
        var date = DateOnly.FromDateTime(local.ToDateTimeUnspecified());
        for (var i = 0; i < days.Count; i++)
            if (days[i].Date == date) return (i, local.Hour * 60 + local.Minute + local.Second / 60.0);
        return null;
    }

    // Where a week/day view scrolls first: an hour before the earliest timed block, or 08:00 when the days are empty.
    public static double FirstMinute(IReadOnlyList<CalendarGridBlock> blocks, (int Day, double Minute)? now) =>
        Math.Max(0, Math.Min(blocks.Count == 0 ? 8 * 60 : blocks.Min(b => b.StartMinute) - 60, now is { } n ? n.Minute - 60 : double.MaxValue));
}
