import Foundation

extension HandleStore {
    @MainActor
    convenience init(account: Account) {
        self.init(services: HandleServices(
            subject: { [weak account] in account?.session.subject },
            offer: { [weak account] in
                guard let account else { throw AuthError.sessionExpired }
                return try await account.client.handleOffer()
            },
            availability: { [weak account] name in
                guard let account else { throw AuthError.sessionExpired }
                return try await account.client.checkHandle(name)
            },
            claim: { [weak account] transaction, name in
                guard let account else { throw AuthError.sessionExpired }
                return try await account.client.claimHandle(transactionId: transaction, namespace: name)
            },
            ensureMailbox: { [weak account] name in await account?.ensureMailbox(inNamespace: name) != nil },
            purchases: AppleHandlePurchases(),
            accountHandles: { [weak account] in
                guard let account else { throw AuthError.sessionExpired }
                return try await account.client.accountHandles()
            }
        ))
    }
}
