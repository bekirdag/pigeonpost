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
            create: { [weak account] label in
                guard let account else { throw AuthError.sessionExpired }
                try await account.client.createNest(label: label)
            },
            orgNests: { [weak account] in
                guard let account, account.features.orgs else { return [] }
                struct Answer: Decodable { struct Org: Decodable { let id: String }; let orgs: [Org] }
                var found: [NestView] = []
                for org in try await account.client.send("/v1/orgs", as: Answer.self).orgs {
                    // An organization without Nests enabled answers 403; the others still count.
                    if let list = try? await account.client.orgNests(org.id) {
                        found += list.nests.map { $0.view(org: org.id) }
                    }
                }
                return found
            },
            purchases: purchases))
    }
}

extension NestDetailServices {
    init(account: Account, nest id: String) {
        func live<T>(_ work: @escaping @MainActor (PostboxClient) async throws -> T) -> @MainActor () async throws -> T {
            { [weak account] in
                guard let account else { throw AuthError.sessionExpired }
                return try await work(account.client)
            }
        }
        self.init(
            nest: live { try await $0.nest(id) },
            files: live { try await $0.nestFiles(id) },
            approvals: live { try await $0.nestApprovals(id) },
            usage: live { try await $0.nestUsage(id) },
            upload: { [weak account] name, data in
                guard let account else { throw AuthError.sessionExpired }
                _ = try await account.client.uploadNestFile(id, name: name, data: data)
            },
            deleteFile: { [weak account] file in
                guard let account else { throw AuthError.sessionExpired }
                try await account.client.deleteNestFile(id, file: file)
            },
            decide: { [weak account] approval, decision, hash in
                guard let account else { throw AuthError.sessionExpired }
                try await account.client.decideNestApproval(id, approval: approval, decision: decision, argsHash: hash)
            },
            action: { [weak account] action in
                guard let account else { throw AuthError.sessionExpired }
                try await account.client.nestAction(id, action)
            },
            rename: { [weak account] label in
                guard let account else { throw AuthError.sessionExpired }
                try await account.client.renameNest(id, label: label)
            },
            export: live { try await $0.exportNest(id) },
            delete: { [weak account] confirm in
                guard let account else { throw AuthError.sessionExpired }
                try await account.client.deleteNest(id, confirm: confirm)
            },
            changed: { [weak account] in await account?.nests.reload() })
    }
}

extension Account {
    func nestDetail(_ nest: NestView) -> NestDetailStore {
        #if DEBUG
        if Fixtures.enabled { return NestFixtures.detail(nest) }
        #endif
        return NestDetailStore(nest: nest, services: NestDetailServices(account: self, nest: nest.nestId))
    }

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
