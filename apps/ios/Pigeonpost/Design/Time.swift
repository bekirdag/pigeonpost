//  When a message arrived, said the way a messenger says it.
//
//  WhatsApp's rule, which is also the web app's: today shows a clock, this week a weekday, older a
//  date. Formatters are held rather than built per row — a thread list rebuilds these on every
//  keystroke of a search.

import Foundation

enum Time {
    private static let clock: DateFormatter = {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("jm")
        return formatter
    }()

    private static let weekdayShort: DateFormatter = {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("EEE")
        return formatter
    }()

    private static let weekdayLong: DateFormatter = {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("EEEE")
        return formatter
    }()

    private static let dayMonth: DateFormatter = {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("d MMM")
        return formatter
    }()

    private static let dayMonthLong: DateFormatter = {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("d MMMM")
        return formatter
    }()

    private static let dayMonthYear: DateFormatter = {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("d MMMM y")
        return formatter
    }()

    static func clockTime(_ unix: Int) -> String {
        clock.string(from: Date(timeIntervalSince1970: TimeInterval(unix)))
    }

    /// The stamp on a row in the conversation list.
    static func listTime(_ unix: Int, now: Date = Date()) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unix))
        let calendar = Calendar.current
        if calendar.isDateInToday(date) { return clockTime(unix) }
        if now.timeIntervalSince(date) < 6 * 86_400 { return weekdayShort.string(from: date) }
        return dayMonth.string(from: date)
    }

    /// The separator between days inside a thread.
    static func dayLabel(_ unix: Int, now: Date = Date()) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unix))
        let calendar = Calendar.current
        if calendar.isDateInToday(date) { return "Today" }
        if calendar.isDateInYesterday(date) { return "Yesterday" }
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date), to: calendar.startOfDay(for: now)).day ?? 0
        if days < 7 { return weekdayLong.string(from: date) }
        let sameYear = calendar.component(.year, from: date) == calendar.component(.year, from: now)
        return sameYear ? dayMonthLong.string(from: date) : dayMonthYear.string(from: date)
    }

    static func sameDay(_ a: Int, _ b: Int) -> Bool {
        Calendar.current.isDate(
            Date(timeIntervalSince1970: TimeInterval(a)),
            inSameDayAs: Date(timeIntervalSince1970: TimeInterval(b))
        )
    }
}
