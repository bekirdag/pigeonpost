import SwiftUI

private struct CalendarLifecycle: ViewModifier {
    @Environment(Session.self) private var session
    @Environment(Account.self) private var account
    @Environment(CalendarStore.self) private var calendar
    @Environment(\.scenePhase) private var phase

    private struct Identity: Equatable {
        let mailbox: String?
        let enabled: Bool
        let delivery: Bool
        let authorization: UUID
        let actor: UUID
    }
    private var identity: Identity {
        .init(mailbox: session.status == .signedIn ? account.me?.address : nil,
              enabled: account.features.calendar, delivery: account.features.calendarEmail,
              authorization: session.authorizationId, actor: account.actingGeneration)
    }
    func body(content: Content) -> some View {
        content
            .onChange(of: identity, initial: true) { _, identity in
                calendar.configure(mailbox: identity.mailbox, enabled: identity.enabled, emailEnabled: identity.delivery)
            }
            .task(id: "\(identity.mailbox ?? "")|\(identity.enabled)|\(identity.delivery)|\(identity.authorization)|\(identity.actor)|\(phase)") {
                calendar.configure(mailbox: identity.mailbox, enabled: identity.enabled, emailEnabled: identity.delivery)
                guard phase == .active, calendar.enabled else { return }
                #if DEBUG
                if Fixtures.enabled && !CommandLine.arguments.contains("-calendar-fixtures") { return }
                #endif
                await calendar.refresh()
                await calendar.live()
            }
    }
}

extension View {
    func calendarLifecycle() -> some View { modifier(CalendarLifecycle()) }
}

struct CalendarEntry: View {
    let count: Int
    let open: () -> Void
    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Image(systemName: "calendar")
                Text("Calendar").font(.geist(14, .medium))
                Spacer()
                if count > 0 {
                    Text("\(count)").font(.geist(12, .semibold)).padding(.horizontal, 7).padding(.vertical, 2)
                        .background(Theme.accent, in: Capsule()).foregroundStyle(Theme.onAccent)
                }
                Image(systemName: "chevron.right").font(.geist(11))
            }
            .foregroundStyle(Theme.ink).frame(maxWidth: .infinity, minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Calendar, \(count) pending invitations")
        .accessibilityIdentifier("calendarButton")
    }
}
