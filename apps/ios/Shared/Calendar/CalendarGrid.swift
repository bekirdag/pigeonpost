import Foundation

/// One timed occurrence placed on a day column of the week/day time grid (SDS §14B.13).
struct CalendarGridBlock: Equatable, Identifiable {
    let occurrenceId: String
    let eventId: String
    /// Minutes after the day's start, clipped to the day; an event from yesterday starts at 0.
    let startMinute: Int
    let endMinute: Int
    /// Side-by-side placement among the events it overlaps.
    let column: Int
    let columns: Int
    var id: String { occurrenceId }
}

/// Pure layout for the time grid: which days a week shows, where each event sits, and where "now" is.
enum CalendarGrid {
    static let dayMinutes = 24 * 60
    /// A short event still gets a block a finger can hit.
    static let minimumMinutes = 20

    /// The seven days of the week containing `day`, starting on `weekStart` (0 = Sunday).
    static func week(containing day: CalendarDay, weekStart: Int) -> [CalendarDay] {
        let weekday = CalendarDay.utc.component(.weekday, from: day.pickerDate) - 1
        let start = (0...6).contains(weekStart) ? weekStart : 1
        let first = day.adding(days: -((weekday - start + 7) % 7))
        return (0..<7).map { first.adding(days: $0) }
    }

    static func allDay(_ items: [EventOccurrence], day: CalendarDay, zone: TimeZone) -> [EventOccurrence] {
        CalendarPresentationCore.events(items, day: day, zone: zone).filter { $0.allDay == true }
            .sorted { ($0.title ?? "", $0.occurrenceId) < ($1.title ?? "", $1.occurrenceId) }
    }

    /// Timed occurrences touching `day`, clipped to it, with overlapping ones given columns.
    static func blocks(_ items: [EventOccurrence], day: CalendarDay, zone: TimeZone) -> [CalendarGridBlock] {
        guard let dayStart = try? day.start(in: zone).timeIntervalSince1970 else { return [] }
        struct Span { let item: EventOccurrence; let start: Int; let end: Int }
        let spans: [Span] = CalendarPresentationCore.events(items, day: day, zone: zone).compactMap { item in
            guard item.allDay != true, let first = item.startsAt, let last = item.endsAt, last > first else { return nil }
            let start = max(0, min(dayMinutes, Int((Double(first) - dayStart) / 60)))
            var end = max(0, min(dayMinutes, Int(((Double(last) - dayStart) / 60).rounded(.up))))
            end = min(dayMinutes, max(end, start + minimumMinutes))
            let clippedStart = min(start, dayMinutes - minimumMinutes)
            return Span(item: item, start: clippedStart, end: end)
        }.sorted { ($0.start, -$0.end, $0.item.occurrenceId) < ($1.start, -$1.end, $1.item.occurrenceId) }

        var out: [CalendarGridBlock] = []
        var cluster: [(Span, Int)] = []
        var columnEnds: [Int] = []
        func flush() {
            let width = max(1, columnEnds.count)
            out += cluster.map { span, column in
                CalendarGridBlock(occurrenceId: span.item.occurrenceId, eventId: span.item.eventId,
                                  startMinute: span.start, endMinute: span.end, column: column, columns: width)
            }
            cluster = []; columnEnds = []
        }
        for span in spans {
            // A cluster ends when every column is free: later events can use the full width again.
            if !columnEnds.isEmpty, columnEnds.allSatisfy({ $0 <= span.start }) { flush() }
            if let free = columnEnds.firstIndex(where: { $0 <= span.start }) {
                columnEnds[free] = span.end; cluster.append((span, free))
            } else {
                columnEnds.append(span.end); cluster.append((span, columnEnds.count - 1))
            }
        }
        flush()
        return out
    }

    /// Minutes into `day` that `now` is, or nil when now is not on that day.
    static func nowMinute(_ now: Date, day: CalendarDay, zone: TimeZone) -> Int? {
        guard let start = try? day.start(in: zone), let end = try? day.adding(days: 1).start(in: zone),
              now >= start, now < end else { return nil }
        return min(dayMinutes - 1, Int(now.timeIntervalSince(start) / 60))
    }

    /// Where to scroll a grid when it opens: an hour before now on today, otherwise 08:00, or the
    /// first event if it starts earlier.
    static func initialHour(now: Date, days: [CalendarDay], blocks: [CalendarGridBlock], zone: TimeZone) -> Int {
        if let minute = days.lazy.compactMap({ nowMinute(now, day: $0, zone: zone) }).first { return max(0, minute / 60 - 1) }
        let earliest = blocks.map(\.startMinute).min().map { $0 / 60 } ?? 8
        return min(8, earliest)
    }
}

/// The day filter shared by the agenda and the grid, Foundation-only so the model tests can run it.
enum CalendarPresentationCore {
    static func events(_ items: [EventOccurrence], day: CalendarDay, zone: TimeZone) -> [EventOccurrence] {
        // Resolve real day boundaries once per day, including 23/25-hour days, not once per row.
        let start = try? day.start(in: zone).timeIntervalSince1970
        let end = try? day.adding(days: 1).start(in: zone).timeIntervalSince1970
        return items.filter { item in
            if item.allDay == true {
                guard let first = item.startDate, let last = item.endDate else { return false }
                return first <= day.id && day.id < last
            }
            guard let first = item.startsAt, let last = item.endsAt, let start, let end else { return false }
            return Double(first) < end && Double(last) > start
        }
    }
}
