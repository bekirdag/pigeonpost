//  The account's mailboxes, and which one the app is acting as.
//
//  `/v1/identities` reports an address and the operator's own label. The handle — the thing trust
//  actually matches on — is only knowable from the server, one mailbox at a time, so the two calls
//  are made together and the result is what the rest of the app calls a Mailbox.

import Foundation
import Observation

@MainActor
@Observable
final class Account {
    enum Load: Equatable { case idle, loading, ready, failed(String) }

    private(set) var mailboxes: [Mailbox] = []
    private(set) var me: Mailbox?
    private(set) var load: Load = .idle

    let session: Session
    let client: PostboxClient

    private let rememberedKey = "ppi_identity"

    init(session: Session) {
        self.session = session
        self.client = PostboxClient(tokens: session)
    }

    func loadIdentities() async {
        if case .loading = load { return }
        load = .loading
        do {
            let rows = try await client.identities()
            let resolved = await withTaskGroup(of: (Int, Mailbox).self) { group in
                for (index, row) in rows.enumerated() {
                    group.addTask { [client] in
                        // A mailbox whose handle cannot be read is still a mailbox: it is the
                        // anonymous ones that a fleet is mostly made of.
                        let handle = try? await client.whoami(identity: row.address).handle
                        return (index, Mailbox(address: row.address, handle: handle ?? nil, label: row.label))
                    }
                }
                var collected: [(Int, Mailbox)] = []
                for await result in group { collected.append(result) }
                return collected.sorted { $0.0 < $1.0 }.map(\.1)
            }
            mailboxes = resolved
            me = preferred(among: resolved)
            load = .ready
        } catch let error as APIError {
            load = .failed(error.errorDescription ?? "Could not reach the postbox.")
        } catch is AuthError {
            load = .failed("Your session expired. Sign in again.")
        } catch {
            load = .failed("Could not reach the postbox.")
        }
    }

    #if DEBUG
    func installFixtures(mailboxes: [Mailbox], me: Mailbox) {
        self.mailboxes = mailboxes
        self.me = me
        load = .ready
    }
    #endif

    func act(as mailbox: Mailbox) {
        me = mailbox
        UserDefaults.standard.set(mailbox.address, forKey: rememberedKey)
    }

    /// The account's other mailboxes. On a namespace these are the sub-agents, and they are the
    /// people the owner most wants to write to.
    ///
    /// No entitlement check is wanted here. An account holds the mailboxes it holds: a free account
    /// has one anonymous mailbox and this list comes out empty on its own, while a namespace owner
    /// sees their fleet. Asking the app to decide who is paid would only add a second, weaker
    /// answer to a question the server has already settled.
    var ownAgents: [Mailbox] {
        mailboxes.filter { $0.address != me?.address }
    }

    /// Which mailbox opens by default.
    ///
    /// An explicit earlier choice wins over everything. Otherwise: a handle is a mailbox somebody
    /// deliberately named — usually the one they think of as "my inbox" — while an anonymous `/k/`
    /// address is typically an agent's. Within the operator's own namespace, `main` is the one that
    /// answers for the namespace itself, so mail sent to `/bekir` rather than to `/bekir/agent1`
    /// lands there; opening any other mailbox by default would hide exactly the mail a person sent
    /// them directly.
    private func preferred(among all: [Mailbox]) -> Mailbox? {
        let remembered = UserDefaults.standard.string(forKey: rememberedKey)
        if let remembered, let match = all.first(where: { $0.address == remembered }) { return match }

        let named = all.filter { $0.handle != nil }
        let inNamespace = { (mailbox: Mailbox) in
            mailbox.handle?.hasPrefix(Config.primaryNamespace + "/") ?? false
        }
        return named.first { $0.handle == Config.primaryNamespace + "/main" }
            ?? named.first(where: inNamespace)
            // A /github/<login> mailbox is personal by construction: the postbox mints one only for
            // a login the account has proved it controls.
            ?? named.first { $0.handle?.hasPrefix("/github/") ?? false }
            ?? named.first { $0.handle?.hasSuffix("/main") ?? false }
            ?? named.first
            ?? all.first
    }

    func signOut() {
        mailboxes = []
        me = nil
        load = .idle
        UserDefaults.standard.removeObject(forKey: rememberedKey)
        session.signOut()
    }
}
