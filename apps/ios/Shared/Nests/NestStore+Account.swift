import Foundation

extension NestStore {
    convenience init(account: Account, purchases: NestPurchasing) {
        self.init(services: NestServices(
            subject: { [weak account] in account?.session.subject },
            catalog: { [weak account] in
                guard let account else { throw AuthError.sessionExpired }
                return try await account.client.nestCatalog()
            },
            snapshot: { [weak account] in
                guard let account else { throw AuthError.sessionExpired }
                return try await account.client.nestSnapshot()
            },
            intent: { [weak account] code in
                guard let account else { throw AuthError.sessionExpired }
                return try await account.client.nestIntent(package: code)
            },
            claim: { [weak account] id in
                guard let account else { throw AuthError.sessionExpired }
                return try await account.client.claimNestPurchase(transactionId: id)
            },
            retry: { [weak account] id in
                guard let account else { throw AuthError.sessionExpired }
                return try await account.client.retryNestSetup(id)
            },
            purchases: purchases))
    }
}

extension Account {
    /// The account's Nests and Nest purchases (Nests Phase B). One per app run, like `handles`, so
    /// the StoreKit listener that recovers unclaimed purchases lives as long as the app.
    var nests: NestStore {
        if let store = nestStoreBox as? NestStore { return store }
        #if DEBUG
        if Fixtures.enabled {
            let store = NestFixtures.make(Fixtures.nestState ?? "sale")
            nestStoreBox = store
            return store
        }
        #endif
        let store = NestStore(account: self, purchases: AppleNestPurchases())
        nestStoreBox = store
        return store
    }
}
