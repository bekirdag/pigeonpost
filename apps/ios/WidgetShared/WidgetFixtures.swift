//  Sample content: the widget gallery's preview, the placeholder while a widget loads, the app's
//  debug gallery, and the tests. Made-up people only — nothing here comes from an account.

import Foundation

enum WidgetFixtures {
    static func snapshot(now: Date = Date(), previews: Bool = false, nests: Bool = true, agenda: Bool = true) -> WidgetSnapshot {
        let minute: TimeInterval = 60
        let calendar = Calendar.current
        let today = calendar.startOfDay(for: now)
        func at(_ hour: Int, _ minute: Int = 0, day: Int = 0) -> Date {
            calendar.date(byAdding: DateComponents(day: day, hour: hour, minute: minute), to: today) ?? now
        }
        let rows: [WidgetConversation] = [
            WidgetConversation(peer: "/ada", name: "Ada Yılmaz", lastAt: now - 4 * minute, unread: 2,
                               preview: "Signed the offer letter. Sending it to /wodo/hr now."),
            WidgetConversation(peer: "/bekir/agent1", name: "Atlas", kind: .agent, lastAt: now - 31 * minute, unread: 1,
                               preview: "Two contracts need your approval."),
            WidgetConversation(peer: "/emre", name: "Emre Dağ", lastAt: now - 3 * 3600, unread: 0,
                               preview: "Thanks, see you at 16:00."),
            WidgetConversation(peer: "/wodo/hr", name: "Wodo HR", lastAt: now - 26 * 3600, unread: 0,
                               preview: "Your contract is ready to sign."),
            WidgetConversation(peer: "/selin", name: "Selin Kaya", lastAt: now - 50 * 3600, unread: 0,
                               preview: "Photos from Saturday attached."),
            WidgetConversation(peer: "/k/7f3a9c", name: "/k/7f3a9c", lastAt: now - 4 * 86400, unread: 0, preview: "ping"),
            WidgetConversation(peer: "/mert", name: "Mert Öz", lastAt: now - 5 * 86400, unread: 0, preview: "Lunch next week?"),
            WidgetConversation(peer: "/deniz", name: "Deniz Arslan", lastAt: now - 9 * 86400, unread: 0, preview: "Done."),
        ]
        return WidgetSnapshot(
            generatedAt: now,
            owner: "fixture",
            mailbox: WidgetMailbox(address: "/k/fixture", title: "/bekir/main"),
            showsPreviews: previews,
            unreadCount: 3,
            conversations: rows,
            nests: nests ? WidgetNests(available: true, items: [
                WidgetNest(id: "nest_1", name: "Research desk", tone: .working, status: "Working…", runningHours: 41.5, hoursLimit: 120),
                WidgetNest(id: "nest_2", name: "Invoices", tone: .attention, status: "Needs you", runningHours: 12, hoursLimit: 120),
                WidgetNest(id: "nest_3", name: "Travel", tone: .ready, status: "Ready", runningHours: nil, hoursLimit: 120),
            ]) : nil,
            agenda: agenda ? WidgetAgenda(events: [
                WidgetEvent(eventId: "ev_1", occurrenceId: nil, title: "Design review", startsAt: max(now + 20 * minute, at(10)),
                            endsAt: max(now + 80 * minute, at(11)), allDay: false, location: "Pigeonpost room", hasMeeting: true),
                WidgetEvent(eventId: "ev_2", occurrenceId: "occ_2", title: "Call with Ada", startsAt: max(now + 3 * 3600, at(16)),
                            endsAt: max(now + 3.5 * 3600, at(16, 30)), allDay: false, location: nil, hasMeeting: true),
                WidgetEvent(eventId: "ev_3", occurrenceId: nil, title: "Contract signing", startsAt: at(9, 30, day: 1),
                            endsAt: at(10, 30, day: 1), allDay: false, location: "Wodo office, Kadıköy", hasMeeting: false),
            ]) : nil
        ).redacted()
    }

    /// No Nests on the account: the "Get a Nest" state.
    static func noNests(now: Date = Date()) -> WidgetSnapshot {
        var snapshot = snapshot(now: now)
        snapshot.nests = WidgetNests(available: true, items: [])
        return snapshot
    }
}
