import Foundation
import SwiftUI

extension PeopleStore {
    convenience init(account: Account) {
        #if DEBUG
        let fixture = PeopleFixtureAPI()
        #endif
        self.init(scope: { [weak account] in
            guard let account, account.session.status == .signedIn, let mailbox = account.me?.address else { return nil }
            var subject = account.session.subject
            #if DEBUG
            if Fixtures.enabled { subject = subject ?? "synthetic-people-fixture" }
            #endif
            guard let subject, !subject.isEmpty else { return nil }
            return PeopleScope(subject: subject, authorization: account.session.authorizationId, actor: account.actingGeneration,
                               mailbox: mailbox, features: account.features.enabled)
        }, api: {
            #if DEBUG
            if Fixtures.enabled { return fixture }
            #endif
            return PeopleClient(client: account.scopedClient(acting: true))
        }, storage: Self.localStorage())
    }
    private static func localStorage() -> PeopleLocalStorage {
        #if DEBUG
        if Fixtures.enabled { return PeopleFixtureStorage() }
        #endif
        return PeopleFiles()
    }
}

private struct PeopleLifecycle: ViewModifier {
    @Environment(PeopleStore.self) private var people
    @Environment(Inbox.self) private var inbox
    @Environment(\.scenePhase) private var phase
    func body(content: Content) -> some View {
        content.task(id: "\(people.scope?.hashValue ?? 0)|\(phase)") {
            people.synchronize()
            #if DEBUG
            if Fixtures.enabled { people.installContactFixtureIfRequested() }
            #endif
            guard phase == .active else { return }
            let scope = people.scope
            await people.loadName(scope)
            while !Task.isCancelled {
                await people.refreshProfiles(scope, addresses: inbox.conversations.map(\.peer))
                do { try await Task.sleep(for: .seconds(60)) } catch { return }
            }
        }
    }
}
extension View { func peopleLifecycle() -> some View { modifier(PeopleLifecycle()) } }
