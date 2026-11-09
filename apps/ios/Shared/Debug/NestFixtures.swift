#if DEBUG
//  `-fixtures -nests=sale|owned|setup`: the Buy Nests tab and the Nest screens without a network,
//  with the real copy and a storefront that answers as a real one would. Used for the App Store
//  review screenshot of the Nest subscriptions (they cannot be reviewed without one).

import Foundation

@MainActor
enum NestFixtures {
    static let token = UUID(uuidString: "7b0c2a59-4a4e-4b8e-9f51-2f3d8c9a1e64")!

    static let packages: [NestPackage] = [
        NestPackage(code: "nest.ai.plus.monthly", name: "Pigeonpost AI Plus", rank: 3, slots: 3, storageBytes: 20 << 30,
                    activeHours: 40, inputTokens: 5_000_000, outputTokens: 500_000, model: "managed", sellable: true,
                    appleProductId: "dev.pigeonpost.inbox.nests.ai.plus.monthly"),
        NestPackage(code: "nest.ai.starter.monthly", name: "Pigeonpost AI", rank: 2, slots: 1, storageBytes: 5 << 30,
                    activeHours: 10, inputTokens: 1_000_000, outputTokens: 100_000, model: "managed", sellable: true,
                    appleProductId: "dev.pigeonpost.inbox.nests.ai.starter.monthly"),
        NestPackage(code: "nest.host.starter.monthly", name: "Empty Nest", rank: 1, slots: 1, storageBytes: 5 << 30,
                    activeHours: 10, inputTokens: 0, outputTokens: 0, model: "none", sellable: true,
                    appleProductId: "dev.pigeonpost.inbox.nests.host.starter.monthly"),
    ]

    static func snapshot(_ state: String) -> NestSnapshot {
        let owned = state == "owned" || state == "setup"
        let funding = owned ? NestFunding(entitlementId: "ent_fixture", source: "apple", package: "nest.ai.starter.monthly",
                                          status: "active", paidThrough: Int(Date().addingTimeInterval(20 * 86400).timeIntervalSince1970)) : nil
        let nests = state == "owned" ? [NestView(nestId: "nst_0123456789abcdef0123456789abcdef", address: "/bekir/agent1",
            label: "Pigeonpost AI", state: "running", funding: "package", status: "ready", statusReason: nil,
            pausedReason: nil, deleteAfter: nil)] : []
        let setups = state == "setup" ? [NestSetup(setupId: "nsu_fixture", label: "Pigeonpost AI", status: "pending",
                                                    attempts: 2, lastError: "nestd_failed")] : []
        return NestSnapshot(allowance: NestAllowance(funding: funding, slots: owned ? 1 : 0, used: nests.count, duplicates: []),
                            package: owned ? packages[1] : nil, nests: nests, setups: setups)
    }

    static func make(_ state: String) -> NestStore {
        let purchases = FixtureNestPurchases()
        var current = snapshot(state)
        return NestStore(services: NestServices(
            subject: { "fixture" },
            catalog: {
                NestCatalog(catalogVersion: "nests-2026-11", packages: packages,
                            rails: .init(apple: true, google: false, web: true), appAccountToken: token.uuidString)
            },
            snapshot: { current },
            intent: { code in
                let p = packages.first { $0.code == code }!
                return NestIntent(intentId: "npi_fixture", appleProductId: p.appleProductId, appAccountToken: token.uuidString)
            },
            claim: { _ in current = snapshot("owned"); return current },
            retry: { _ in current = snapshot("owned"); return current },
            create: { _ in },
            orgNests: { [] },
            purchases: purchases))
    }

    /// The Nest's page without a network: one file of each outcome, one card waiting, a month used.
    static func detail(_ start: NestView) -> NestDetailStore {
        var nest = start
        var approvals = [NestApproval(approvalId: "nap_fixture", tool: "send_message",
            call: .init(tool: "send_message", args: .init(to: "/bekir/main", body: "The Q3 summary is ready for you to read.")),
            argsHash: String(repeating: "a", count: 64), status: "pending",
            expiresAt: Int(Date().addingTimeInterval(86400).timeIntervalSince1970), result: nil)]
        var files = [
            NestFile(fileId: "nfl_fixture1", name: "Q3 plan.pdf", bytes: 214_000, kind: "pdf", status: "parsed", reason: nil,
                     coverage: .init(unitsTotal: 12, unitsText: 12), warnings: []),
            NestFile(fileId: "nfl_fixture2", name: "archive.zip", bytes: 1_800_000, kind: "zip", status: "partial", reason: nil,
                     coverage: .init(unitsTotal: 9, unitsText: 7), warnings: ["2 entries had no readable text"]),
        ]
        let usage = NestUsage(period: "2026-11", inputTokens: .init(used: 120_000, limit: 1_000_000),
                              outputTokens: .init(used: 9_000, limit: 100_000), runningSecs: 5_400, filesBytes: 2_014_000,
                              package: .init(code: "nest.ai.starter.monthly", activeHoursPooled: 10))
        func deciding(_ id: String, as status: String) {
            approvals = approvals.map {
                $0.approvalId == id ? NestApproval(approvalId: $0.approvalId, tool: $0.tool, call: $0.call, argsHash: $0.argsHash,
                                                   status: status, expiresAt: $0.expiresAt, result: status == "approved" ? "sent" : nil) : $0
            }
        }
        func nextState(_ state: String, _ reason: String?, _ status: String) {
            nest = NestView(nestId: nest.nestId, address: nest.address, label: nest.label, state: state, funding: nest.funding,
                            status: status, statusReason: nil, pausedReason: reason, deleteAfter: nil, role: nest.role, orgId: nest.orgId)
        }
        return NestDetailStore(nest: nest, services: NestDetailServices(
            nest: { nest },
            files: { NestFiles(files: files, usedBytes: files.reduce(0) { $0 + $1.bytes }, quotaBytes: 5 << 30) },
            approvals: { approvals },
            usage: { usage },
            upload: { name, data in
                files.insert(NestFile(fileId: "nfl_new", name: name, bytes: Int64(data.count), kind: "txt", status: "parsed",
                                      reason: nil, coverage: .init(unitsTotal: 1, unitsText: 1), warnings: []), at: 0)
            },
            deleteFile: { id in files.removeAll { $0.fileId == id } },
            decide: { id, decision, _ in deciding(id, as: decision == "approve" ? "approved" : "rejected") },
            action: { action in
                if action == "pause" { nextState("paused", "owner", "paused") } else { nextState("running", nil, "ready") }
            },
            rename: { label in
                nest = NestView(nestId: nest.nestId, address: nest.address, label: label, state: nest.state, funding: nest.funding,
                                status: nest.status, statusReason: nest.statusReason, pausedReason: nest.pausedReason,
                                deleteAfter: nest.deleteAfter, role: nest.role, orgId: nest.orgId)
            },
            export: { Data("PK".utf8) },
            delete: { _ in },
            changed: {}))
    }
}

@MainActor
final class FixtureNestPurchases: NestPurchasing {
    let updates = AsyncStream<HandleTransaction> { _ in }
    func products(_ ids: [String]) async throws -> [HandleProduct] {
        let prices = ["dev.pigeonpost.inbox.nests.ai.plus.monthly": Decimal(25),
                      "dev.pigeonpost.inbox.nests.ai.starter.monthly": Decimal(10),
                      "dev.pigeonpost.inbox.nests.host.starter.monthly": Decimal(5)]
        return ids.compactMap { id in prices[id].map {
            HandleProduct(id: id, displayPrice: $0.formatted(.currency(code: "USD")), price: $0, currencyCode: "USD")
        } }
    }
    func purchase(_ id: String, accountToken: UUID) async throws -> HandlePurchaseResult {
        .purchased(HandleTransaction(id: "fixture-txn", originalId: "fixture-txn", productId: id, accountToken: accountToken,
                                     expiresAt: Date().addingTimeInterval(30 * 86400), revoked: false))
    }
    func transactions() async -> [HandleTransaction] { [] }
    func restore() async throws {}
    func finish(_ id: String) async {}
}
#endif
