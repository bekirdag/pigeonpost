import Foundation

extension CalendarStore {
    /// Both app targets use this initializer; isolated transports keep the same account boundary.
    convenience init(account: Account, client: PostboxClient? = nil, now: @escaping () -> Date = Date.init) {
        self.init(client: client ?? account.client, now: now, scope: { [weak account] in
            guard let account, account.session.status == .signedIn, account.features.calendar,
                  let mailbox = account.me?.address else { return nil }
            var subject = account.session.subject
            #if DEBUG
            if Fixtures.enabled { subject = subject ?? "synthetic-calendar-fixture" }
            #endif
            guard let subject, !subject.isEmpty else { return nil }
            return CalendarAccessScope(subject: subject, authorization: account.session.authorizationId,
                                       actor: account.actingGeneration, mailbox: mailbox)
        })
    }
}
