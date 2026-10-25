import Foundation

struct CalendarMeetingStatus: Decodable {
    let state: String?
    let occurrences: [CalendarMeetingOccurrence]?

    var choices: [CalendarMeetingOccurrence] {
        guard state == "open", let occurrences, occurrences.count <= 50,
              Set(occurrences.map(\.occurrenceId)).count == occurrences.count,
              occurrences.allSatisfy(\.valid) else { return [] }
        return occurrences
    }
}

struct CalendarMeetingOccurrence: Decodable, Identifiable {
    let occurrenceId: String
    let startsAt: Int64?
    let endsAt: Int64?
    let opensAt: Int64?
    let closesAt: Int64?
    var id: String { occurrenceId }
    var valid: Bool {
        let bytes = occurrenceId.utf8
        guard occurrenceId.hasPrefix("occ_"), (5...80).contains(bytes.count),
              bytes.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 95 }),
              let startsAt, let endsAt, let opensAt, let closesAt,
              (0...253_402_300_799).contains(opensAt), closesAt <= 253_402_300_799,
              opensAt <= startsAt, startsAt < endsAt, endsAt <= closesAt else { return false }
        return true
    }
}
