import Observation
import SwiftUI

struct CalendarLinkRequest: Identifiable, Equatable {
    let id = UUID()
    let link: CalendarLink
}

@MainActor @Observable
final class CalendarLinkRouter {
    var pending: CalendarLinkRequest?
    var signedOutGuest: CalendarLinkRequest?
    var scanned: CalendarLinkRequest?

    func receive(_ link: CalendarLink, signedIn: Bool) {
        let request = CalendarLinkRequest(link: link)
        if !signedIn, case .guest = link, signedOutGuest == nil { signedOutGuest = request }
        else { pending = request }
    }
    @discardableResult func finishScanner() -> Bool {
        guard let scanned else { return false }
        self.scanned = nil; pending = scanned
        return true
    }
}

private struct CalendarMeetingLinks: ViewModifier {
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(CallHost.self) private var calls: CallHost?
    @Environment(CalendarLinkRouter.self) private var router: CalendarLinkRouter?
    let canPresent: Bool
    let open: (CalendarLinkRequest) -> Void

    private struct Ready: Equatable {
        let request: CalendarLinkRequest?
        let loaded: Bool
        let signedIn: Bool
    }
    func body(content: Content) -> some View {
        let ready = Ready(request: router?.pending, loaded: account.features.loaded,
                          signedIn: account.session.status == .signedIn)
        content.onChange(of: ready, initial: true) { _, state in
            guard state.signedIn, let request = state.request else { return }
            if case .member = request.link, !state.loaded { return }
            router?.pending = nil
            guard canPresent else { inbox.toast = "Close the open screen and open the meeting link again."; return }
            if case .member = request.link {
                guard calls?.mode == .hidden, calls?.preparingScheduled != true else {
                    inbox.toast = "Finish the call before opening another meeting."
                    return
                }
                guard account.features.calendar else {
                    inbox.toast = "Calendar is unavailable for this account."
                    return
                }
            }
            open(request)
        }
    }
}

private struct SignedOutCalendarGuests: ViewModifier {
    @Environment(Session.self) private var session
    @Environment(CalendarLinkRouter.self) private var router
    func body(content: Content) -> some View {
        @Bindable var router = router
        content
            .onChange(of: session.status) { _, status in
                router.signedOutGuest = nil
                if status == .signedOut { router.pending = nil; router.scanned = nil }
            }
            .sheet(item: $router.signedOutGuest, onDismiss: nextGuest) { request in
                if case .guest(let url) = request.link { CalendarGuestLinkBrowser(url: url).id(request.id) }
            }
    }
    private func nextGuest() {
        guard session.status == .signedOut, router.signedOutGuest == nil,
              let pending = router.pending, case .guest = pending.link else { return }
        router.pending = nil; router.signedOutGuest = pending
    }
}

extension View {
    func calendarMeetingLinks(canPresent: Bool, open: @escaping (CalendarLinkRequest) -> Void) -> some View {
        modifier(CalendarMeetingLinks(canPresent: canPresent, open: open))
    }
    func signedOutCalendarGuests() -> some View { modifier(SignedOutCalendarGuests()) }
}
