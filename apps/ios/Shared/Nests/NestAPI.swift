//  The Nests routes (docs/nests/phase-b.md), over the postbox client's own wire (401 retry,
//  scope checks). Every route is behind the `nests` capability and answers 404 for a Nest the
//  caller holds no role on.

import Foundation

extension PostboxClient {
    func nestCatalog() async throws -> NestCatalog {
        try await send("/v1/nest-packages", as: NestCatalog.self)
    }

    func nestSnapshot() async throws -> NestSnapshot {
        try await send("/v1/nests", as: NestSnapshot.self)
    }

    func nest(_ id: String) async throws -> NestView {
        try await send("/v1/nests/\(id)", as: NestView.self)
    }

    /// Before any purchase sheet: the postbox refuses a second charge here.
    func nestIntent(package: String, rail: String = "apple") async throws -> NestIntent {
        try await send("/v1/nest-purchases/intents", method: "POST",
                       json: ["package": package, "rail": rail], as: NestIntent.self)
    }

    /// Hand the App Store transaction to the postbox, which asks Apple what it means.
    @discardableResult
    func claimNestPurchase(transactionId: String) async throws -> NestSnapshot {
        try await send("/v1/nest-purchases/apple", method: "POST",
                       json: ["transaction_id": transactionId], as: NestSnapshot.self)
    }

    @discardableResult
    func retryNestSetup(_ id: String) async throws -> NestSnapshot {
        try await send("/v1/nest-setups/\(id)/retry", method: "POST", json: [:], as: NestSnapshot.self)
    }

    func createNest(label: String) async throws {
        _ = try await sendRaw("/v1/nests", method: "POST", json: ["label": label])
    }

    func nestAction(_ id: String, _ action: String) async throws {
        _ = try await sendRaw("/v1/nests/\(id)/\(action)", method: "POST", json: [:])
    }

    func renameNest(_ id: String, label: String) async throws {
        _ = try await sendRaw("/v1/nests/\(id)", method: "PATCH", json: ["label": label])
    }

    func deleteNest(_ id: String, confirm: String) async throws {
        _ = try await sendRaw("/v1/nests/\(id)", method: "DELETE", json: ["confirm": confirm])
    }

    func nestUsage(_ id: String) async throws -> NestUsage {
        try await send("/v1/nests/\(id)/usage", as: NestUsage.self)
    }

    func exportNest(_ id: String) async throws -> Data {
        try await sendRaw("/v1/nests/\(id)/export", timeout: 600)
    }

    func nestFiles(_ id: String) async throws -> NestFiles {
        try await send("/v1/nests/\(id)/files", as: NestFiles.self)
    }

    func uploadNestFile(_ id: String, name: String, data: Data) async throws -> NestFile {
        let raw = try await sendRaw("/v1/nests/\(id)/files", method: "POST", body: data,
                                    contentType: "application/octet-stream",
                                    headers: ["x-file-name": NestWords.fileName(name)], timeout: 300)
        return try Self.decode(NestFile.self, from: raw)
    }

    func deleteNestFile(_ id: String, file: String) async throws {
        _ = try await sendRaw("/v1/nests/\(id)/files/\(file)", method: "DELETE")
    }

    func nestApprovals(_ id: String) async throws -> [NestApproval] {
        struct Answer: Decodable { let approvals: [NestApproval] }
        return try await send("/v1/nests/\(id)/approvals", as: Answer.self).approvals
    }

    /// `decision` is `approve` or `reject`; `argsHash` is the hash of the call the person saw.
    func decideNestApproval(_ id: String, approval: String, decision: String, argsHash: String) async throws {
        _ = try await sendRaw("/v1/nests/\(id)/approvals/\(approval)/\(decision)", method: "POST",
                              json: ["args_hash": argsHash])
    }

    func orgNests(_ org: String) async throws -> OrgNestList {
        try await send("/v1/orgs/\(org)/nests", as: OrgNestList.self)
    }

    func createOrgNest(_ org: String, handle: String, label: String) async throws {
        _ = try await sendRaw("/v1/orgs/\(org)/nests", method: "POST", json: ["handle": handle, "label": label])
    }
}
