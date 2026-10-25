import Foundation

@main enum CalendarNoticeTests {
    static func main() {
        var checks = 0
        func check(_ condition: @autoclosure () -> Bool) { precondition(condition()); checks += 1 }
        let mailbox = "/k/receiver"
        let base: [AnyHashable: Any] = ["type": "calendar", "mailbox": mailbox,
            "calendar_event_id": "evt_one", "calendar_occurrence_id": "occ_opaque_20261025T090000",
            "peer": "/k/organizer", "token": "untrusted", "role": "host"]
        let notice = CalendarNotice(payload: base)!
        check(notice.mailbox == mailbox)
        check(notice.event == CalendarEventKey(eventId: "evt_one", occurrenceId: "occ_opaque_20261025T090000"))
        check(notice.belongs(to: ["/k/other", mailbox]))
        check(!notice.belongs(to: ["/k/organizer"]))
        check(!notice.belongs(to: []))
        check(CalendarNotice(payload: base)!.id != notice.id)
        var invitation = base; invitation.removeValue(forKey: "calendar_occurrence_id")
        check(CalendarNotice(payload: invitation)?.event == CalendarEventKey(eventId: "evt_one", occurrenceId: nil))
        let legacy: [AnyHashable: Any] = ["mailbox": mailbox, "message_id": "cal:opaque:reminder", "peer": "/k/organizer"]
        check(CalendarNotice(payload: legacy)?.event == nil)
        check(CalendarNotice.matches([:], messageId: "cal:evt_old:invite"))
        check(CalendarNotice(payload: ["mailbox": mailbox], messageId: "cal:opaque:reminder")?.mailbox == mailbox)
        check(!CalendarNotice.matches(["type": "call_ring", "message_id": "message_one"]))
        check(CalendarNotice(payload: ["peer": "/k/organizer", "mailbox": mailbox]) == nil)
        for invalid in ["", "/k/", "/k/one/two", "/k/one%2Ftwo", "/k/one\n", "/k/ü", "/handle", "/k/" + String(repeating: "x", count: 158)] {
            var payload = base; payload["mailbox"] = invalid
            check(CalendarNotice(payload: payload) == nil)
            check(CalendarNotice.matches(payload))
        }
        for invalid in ["", "evt_", "evt_../bad", "evt_bad\n", "evt_ü", "evt_" + String(repeating: "x", count: 80)] {
            var payload = base; payload["calendar_event_id"] = invalid
            check(CalendarNotice(payload: payload)?.event == nil)
        }
        for invalid in ["occ_", "occ_bad/value", "occ_ü", String(repeating: "x", count: 81), 7, NSNull()] as [Any] {
            var payload = base; payload["calendar_occurrence_id"] = invalid
            check(CalendarNotice(payload: payload)?.event == nil)
        }
        print("Calendar notices: \(checks) checks passed")
    }
}
