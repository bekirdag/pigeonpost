import Foundation

enum CalendarEventScope: String, CaseIterable { case all, this, following }
enum CalendarAnswerScope: String, CaseIterable { case all, this }
enum CalendarAnswer: String, CaseIterable { case accepted, tentative, declined }

struct CalendarEventKey: Hashable, Identifiable {
    let eventId: String
    var occurrenceId: String?
    var id: String { eventId + ":" + (occurrenceId ?? "series") }
}

// Calendar details are sealed at rest by the service, not end-to-end encrypted. These DTOs
// follow docs/calendar.md and use the shared decoder's convertFromSnakeCase strategy.
struct CalendarFolder: Codable, Equatable, Identifiable {
    let id: String
    var name: String?
    var color: String?
    var isDefault: Bool?
    var revision: Int?
}

struct CalendarPerson: Codable, Equatable {
    var address: String?
    var handle: String?
    var name: String?
}

struct CalendarReminder: Codable, Equatable {
    let minutesBefore: Int
    var channel: String?
}

struct CalendarFoldersResponse: Decodable { var calendars: [CalendarFolder]? }
struct CalendarFolderResponse: Decodable { let calendar: CalendarFolder }

struct ScheduledMeeting: Codable, Equatable {
    var enabled: Bool?
    var code: String?
    var url: String?
    var opensAt: Int?
    var closesAt: Int?
    var waiting: String?
    var admit: String?
    var allowKnock: Bool?
    var joinBeforeMinutes: Int?
    var guestsSeeList: Bool?
    var transport: String?
    var participantLimit: Int?
    var admitters: [String]?
}

struct EventOccurrence: Decodable, Equatable, Identifiable {
    let occurrenceId: String
    let eventId: String
    var recurrenceKey: String?
    var calendarId: String?
    var color: String?
    var allDay: Bool?
    var startsAt: Int?
    var endsAt: Int?
    var tzid: String?
    var startDate: String?
    var endDate: String?
    var title: String?
    var location: String?
    var status: String?
    var recurring: Bool?
    var overridden: Bool?
    var organizer: CalendarPerson?
    var role: String?
    var myRsvp: String?
    var invited: Bool?
    var meeting: ScheduledMeeting?
    var id: String { occurrenceId }
}

struct EventOccurrencesResponse: Decodable { var occurrences: [EventOccurrence]? }

struct CalendarAttendee: Decodable, Equatable, Identifiable {
    let attendeeId: String
    var kind: String?
    var address: String?
    var handle: String?
    var contact: String?
    var name: String?
    var role: String?
    var optional: Bool?
    var rsvp: String?
    var rsvpAt: Int?
    var proof: String?
    var delivery: String?
    var invited: Bool?
    var id: String { attendeeId }
}

struct CalendarMembership: Decodable, Equatable {
    var role: String?
    var attendeeId: String?
    var rsvp: String?
    var calendarId: String?
}

struct ScheduledEvent: Decodable, Equatable, Identifiable {
    let id: String
    var uid: String?
    var calendarId: String?
    var revision: Int?
    var sequence: Int?
    var status: String?
    var organizer: CalendarPerson?
    var title: String?
    var description: String?
    var location: String?
    var allDay: Bool?
    var start: String?
    var end: String?
    var tzid: String?
    var startsAt: Int?
    var endsAt: Int?
    var startDate: String?
    var endDate: String?
    var rrule: String?
    var exdates: [String]?
    var transparency: String?
    var privacy: String?
    var meeting: ScheduledMeeting?
    var attendees: [CalendarAttendee]?
    var counts: [String: Int]?
    var my: CalendarMembership?
    var reminders: [CalendarReminder]?
    var linkedGroup: String?
    var canEdit: Bool?
    var locale: String?
}

/// A detail projection is intentionally partial; unlike a listing item it need not repeat
/// event_id. Keep its opaque occurrence id/key when a series instance moves.
struct EventOccurrenceDetail: Decodable, Equatable {
    var occurrenceId: String?
    var recurrenceKey: String?
    var startsAt: Int?
    var endsAt: Int?
    var allDay: Bool?
    var tzid: String?
    var opensAt: Int?
    var closesAt: Int?
    var status: String?
    var myRsvp: String?
    var title: String?
    var description: String?
    var location: String?
    var start: String?
    var end: String?
    var startDate: String?
    var endDate: String?
}

struct CalendarEventResponse: Decodable {
    let event: ScheduledEvent
    var occurrence: EventOccurrenceDetail?
    var deliveryStatus: String?
    var splitFrom: CalendarSplit?
    var carried: [String: Int]?
    var dropped: [String: Int]?
}

struct CalendarSplit: Decodable { let eventId: String; var revision: Int? }

struct CalendarInvitation: Decodable, Equatable, Identifiable {
    let eventId: String
    var occurrenceId: String?
    var title: String?
    var startsAt: Int?
    var endsAt: Int?
    var allDay: Bool?
    var startDate: String?
    var endDate: String?
    var tzid: String?
    var organizer: CalendarPerson?
    var recurring: Bool?
    var meeting: Bool?
    var invitedAt: Int?
    var id: String { eventId + ":" + (occurrenceId ?? "series") }
}

struct CalendarInvitationsResponse: Decodable { var invitations: [CalendarInvitation]? }

struct CalendarWorkingHours: Codable, Equatable {
    var start: String?
    var end: String?
    var days: [Int]?
}

struct CalendarSettings: Codable, Equatable {
    var previews: Bool?
    var timezone: String?
    var weekStart: Int?
    var workingHours: CalendarWorkingHours?
}

struct CalendarGuestLink: Decodable { let url: String; var expiresAt: Int? }

struct CalendarFeed: Decodable {
    var url: String?
    var enabled: Bool?
    var projection: String?
    var createdAt: Int?
    var expiresAt: Int?
}

struct CalendarImportPreview: Decodable {
    let draftId: String
    var calendarId: String?
    var events: [CalendarImportEvent]?
    var warnings: [String]?
    var expiresAt: Int?
}

struct CalendarImportEvent: Decodable, Identifiable {
    let index: Int
    var importable: Bool?
    var title: String?
    var allDay: Bool?
    var start: String?
    var end: String?
    var startDate: String?
    var endDate: String?
    var tzid: String?
    var rrule: String?
    var warnings: [String]?
    var id: Int { index }
}

struct CalendarImportResult: Decodable { var created: [ScheduledEvent]? }

struct CalendarBusyInterval: Decodable, Equatable { let start: Int; let end: Int }

enum CalendarBusy: Decodable, Equatable {
    case unknown
    case known([CalendarBusyInterval])

    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if (try? value.decode(String.self)) != nil { self = .unknown }
        else { self = .known(try value.decode([CalendarBusyInterval].self)) }
    }
}

struct CalendarBusyResponse: Decodable { var busy: [String: CalendarBusy]? }
