import Foundation

/// Untrusted push locators. No title, token, role or authority is adopted from a notice.
struct CalendarNotice: Equatable, Identifiable {
    let id = UUID()
    let mailbox: String
    let event: CalendarEventKey?

    static func matches(_ payload: [AnyHashable: Any], messageId: String? = nil) -> Bool {
        payload["type"] as? String == "calendar"
            || ((payload["message_id"] as? String ?? messageId)?.hasPrefix("cal:") == true)
    }

    init?(payload: [AnyHashable: Any], messageId: String? = nil) {
        guard Self.matches(payload, messageId: messageId),
              let mailbox = payload["mailbox"] as? String,
              mailbox.hasPrefix("/k/"), (4...160).contains(mailbox.utf8.count),
              mailbox.dropFirst(3).utf8.allSatisfy(Self.ascii) else { return nil }
        self.mailbox = mailbox
        // Legacy notices, malformed locators and unknown payload versions open only Calendar.
        // In particular, never derive an event from the opaque occurrence inside message_id.
        if let event = payload["calendar_event_id"] as? String, Self.locator(event, prefix: "evt_"),
           payload["calendar_occurrence_id"] == nil
            || (payload["calendar_occurrence_id"] as? String).map({ Self.locator($0, prefix: "occ_") }) == true {
            self.event = CalendarEventKey(eventId: event, occurrenceId: payload["calendar_occurrence_id"] as? String)
        } else { self.event = nil }
    }

    func belongs(to addresses: [String]) -> Bool { addresses.contains(mailbox) }

    private static func locator(_ value: String, prefix: String) -> Bool {
        value.hasPrefix(prefix) && value.utf8.count > prefix.utf8.count
            && value.utf8.count <= 80 && value.utf8.allSatisfy(ascii)
    }
    private static func ascii(_ c: UInt8) -> Bool {
        (65...90).contains(c) || (97...122).contains(c) || (48...57).contains(c) || c == 95
    }
}
