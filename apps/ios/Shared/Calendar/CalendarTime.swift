import Foundation

enum CalendarTimeError: Error, Equatable {
    case invalidFormat
    case unknownZone
    case nonexistent
    case ambiguous(earlier: Int, later: Int)
    case order
}

enum CalendarFold: String, CaseIterable { case earlier, later }

/// A calendar day has no timezone/UTC offset. Encoding it as a UTC noon is only a bridge to
/// DatePicker and date arithmetic, never a conversion of an authored event time.
struct CalendarDay: Equatable, Hashable, Comparable, Identifiable {
    let year: Int
    let month: Int
    let day: Int
    var id: String { String(format: "%04d-%02d-%02d", year, month, day) }

    static var utc: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        calendar.locale = Locale(identifier: "en_US_POSIX")
        return calendar
    }

    init(_ text: String) throws {
        let bytes = Array(text.utf8)
        guard bytes.count == 10, bytes[4] == 45, bytes[7] == 45,
              bytes.enumerated().allSatisfy({ [4, 7].contains($0.offset) || (48...57).contains($0.element) }),
              let year = Int(text.prefix(4)), let month = Int(text.dropFirst(5).prefix(2)), let day = Int(text.suffix(2)),
              (1...9999).contains(year), (1...12).contains(month), (1...31).contains(day),
              let date = Self.utc.date(from: DateComponents(year: year, month: month, day: day, hour: 12))
        else { throw CalendarTimeError.invalidFormat }
        let checked = Self.utc.dateComponents([.year, .month, .day], from: date)
        guard checked.year == year, checked.month == month, checked.day == day else { throw CalendarTimeError.invalidFormat }
        self.year = year; self.month = month; self.day = day
    }

    init(date: Date, zone: TimeZone) {
        var calendar = Self.utc
        calendar.timeZone = zone
        let parts = calendar.dateComponents([.year, .month, .day], from: date)
        year = parts.year!; month = parts.month!; day = parts.day!
    }

    var pickerDate: Date { Self.utc.date(from: DateComponents(year: year, month: month, day: day, hour: 12))! }

    func adding(days: Int) -> CalendarDay {
        CalendarDay(date: Self.utc.date(byAdding: .day, value: days, to: pickerDate)!, zone: Self.utc.timeZone)
    }

    func start(in zone: TimeZone) throws -> Date {
        var calendar = Self.utc
        calendar.timeZone = zone
        let noon = try CalendarWallTime(id + "T12:00", tzid: zone.identifier).resolve(fold: .earlier)
        guard let interval = calendar.dateInterval(of: .day, for: noon) else { throw CalendarTimeError.nonexistent }
        return interval.start
    }

    static func < (left: CalendarDay, right: CalendarDay) -> Bool { left.id < right.id }
}

/// Preserve the typed wall time. Foundation's lenient date construction is used only in UTC
/// to check calendar validity; strict matching in the chosen zone detects gaps and folds.
struct CalendarWallTime: Equatable {
    let text: String
    let tzid: String
    let day: CalendarDay
    let hour: Int
    let minute: Int
    let second: Int

    init(_ text: String, tzid: String) throws {
        let bytes = Array(text.utf8)
        guard bytes.count == 16 || bytes.count == 19, bytes[10] == 84, bytes[13] == 58,
              bytes.count != 19 || bytes[16] == 58,
              bytes.enumerated().allSatisfy({ [4, 7, 10, 13, 16].contains($0.offset) || (48...57).contains($0.element) }),
              let hour = Int(text.dropFirst(11).prefix(2)), let minute = Int(text.dropFirst(14).prefix(2)),
              let second = bytes.count == 19 ? Int(text.suffix(2)) : 0,
              (0...23).contains(hour), (0...59).contains(minute), (0...59).contains(second)
        else { throw CalendarTimeError.invalidFormat }
        day = try CalendarDay(String(text.prefix(10)))
        guard TimeZone(identifier: tzid) != nil else { throw CalendarTimeError.unknownZone }
        self.text = text; self.tzid = tzid; self.hour = hour; self.minute = minute; self.second = second
    }

    func resolve(fold: CalendarFold? = nil) throws -> Date {
        guard let zone = TimeZone(identifier: tzid) else { throw CalendarTimeError.unknownZone }
        var calendar = CalendarDay.utc
        calendar.timeZone = zone
        let parts = DateComponents(year: day.year, month: day.month, day: day.day, hour: hour, minute: minute, second: second)
        let anchor = day.pickerDate.addingTimeInterval(-48 * 3600)
        func match(_ policy: Calendar.RepeatedTimePolicy) -> Date? {
            guard let date = calendar.nextDate(after: anchor, matching: parts, matchingPolicy: .strict, repeatedTimePolicy: policy) else { return nil }
            let actual = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
            return actual == parts ? date : nil
        }
        guard let earlier = match(.first) else { throw CalendarTimeError.nonexistent }
        let later = match(.last) ?? earlier
        guard earlier != later else { return earlier }
        switch fold {
        case .earlier: return earlier
        case .later: return later
        case nil: throw CalendarTimeError.ambiguous(earlier: Int(earlier.timeIntervalSince1970), later: Int(later.timeIntervalSince1970))
        }
    }

    static func range(start: String, end: String, tzid: String, fold: CalendarFold? = nil) throws -> Range<Date> {
        let first = try CalendarWallTime(start, tzid: tzid).resolve(fold: fold)
        let last = try CalendarWallTime(end, tzid: tzid).resolve(fold: fold)
        guard last > first else { throw CalendarTimeError.order }
        return first..<last
    }
}

struct CalendarMonthWindow {
    let days: [CalendarDay]

    init(containing day: CalendarDay, weekStart: Int) throws {
        guard (0...6).contains(weekStart) else { throw CalendarTimeError.invalidFormat }
        let first = try CalendarDay(String(format: "%04d-%02d-01", day.year, day.month))
        let weekday = CalendarDay.utc.component(.weekday, from: first.pickerDate) - 1
        let offset = (weekday - weekStart + 7) % 7
        days = (0..<42).map { first.adding(days: $0 - offset) }
    }

    func bounds(in zone: TimeZone) throws -> Range<Date> {
        let first = try days[0].start(in: zone)
        let last = try days[41].adding(days: 1).start(in: zone)
        return first..<last
    }
}
