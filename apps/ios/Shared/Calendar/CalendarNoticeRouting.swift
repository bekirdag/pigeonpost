import SwiftUI

private struct CalendarNoticeRouting: ViewModifier {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(PushService.self) private var push
    @Environment(CalendarStore.self) private var calendar: CalendarStore?
    @Environment(CallHost.self) private var calls: CallHost?
    let open: () -> Void
    let visit: (Mailbox) -> Void
    let canPresent: Bool

    private struct State: Equatable {
        let notice: CalendarNotice?
        let mailbox: String?
        let owned: [String]
        let loaded: Bool
        let enabled: Bool
    }
    private var state: State {
        State(notice: push.pendingCalendar, mailbox: account.me?.address,
              owned: account.mailboxes.map(\.address), loaded: account.features.loaded,
              enabled: account.features.calendar)
    }

    func body(content: Content) -> some View {
        content.onChange(of: state, initial: true) { _, value in route(value) }
    }

    private func route(_ state: State) {
        guard let notice = state.notice, state.loaded, account.session.status == .signedIn else { return }
        push.pendingCalendar = nil
        guard notice.belongs(to: state.owned),
              let target = account.mailboxes.first(where: { $0.address == notice.mailbox }) else {
            inbox.toast = "This calendar notice belongs to another account."
            return
        }
        guard state.enabled, let calendar else {
            inbox.toast = "Calendar is unavailable for this account."
            return
        }
        guard canPresent, !calendar.isOpen || calendar.mailbox == notice.mailbox else {
            inbox.toast = "Close the open screen and tap the calendar notice again."
            return
        }
        if target.address != account.me?.address, let calls,
           calls.mode != .hidden || calls.preparingScheduled {
            inbox.toast = "Finish the call before opening another inbox’s calendar."
            return
        }
        visit(target)
        guard account.me?.address == notice.mailbox else { return }
        calendar.configure(mailbox: notice.mailbox, enabled: true, emailEnabled: account.features.calendarEmail)
        calendar.pendingNavigation = notice.event
        open()
    }
}

extension View {
    func calendarNotices(canPresent: Bool, open: @escaping () -> Void, visit: @escaping (Mailbox) -> Void) -> some View {
        modifier(CalendarNoticeRouting(open: open, visit: visit, canPresent: canPresent))
    }
}
