//  One Nest's page and the header above its conversation (Nests Phase B3-B5): the state and the
//  rules the views show. The postbox enforces every role and hash; this decides only what is
//  worth offering, so a button is not shown for something the server will refuse.

import Foundation
import Observation

@MainActor
struct NestDetailServices {
    var nest: () async throws -> NestView
    var files: () async throws -> NestFiles
    var approvals: () async throws -> [NestApproval]
    var usage: () async throws -> NestUsage
    var upload: (String, Data) async throws -> Void
    var deleteFile: (String) async throws -> Void
    /// Approval id, `approve` or `reject`, and the hash of the call the person saw.
    var decide: (String, String, String) async throws -> Void
    var action: (String) async throws -> Void
    var rename: (String) async throws -> Void
    var export: () async throws -> Data
    var delete: (String) async throws -> Void
    /// The account's list of Nests may have changed.
    var changed: () async -> Void
}

@MainActor
@Observable
final class NestDetailStore {
    /// The postbox's limit, checked first so a large file is not read into memory to be refused.
    static let maxUploadBytes: Int64 = 25 * 1024 * 1024

    private(set) var nest: NestView
    private(set) var files: NestFiles?
    private(set) var approvals: [NestApproval] = []
    /// False until the cards have been read, so "nothing is waiting" is never said without knowing.
    private(set) var approvalsLoaded = false
    private(set) var usage: NestUsage?
    private(set) var message: String?
    private(set) var busy = false
    /// Deleted, or this person's access has ended. The page closes and the header disappears.
    private(set) var gone = false
    /// A finished export waiting to be saved.
    private(set) var exported: Data?

    @ObservationIgnored private let services: NestDetailServices
    @ObservationIgnored var now: () -> Date = Date.init

    init(nest: NestView, services: NestDetailServices) {
        self.nest = nest
        self.services = services
    }

    var pending: [NestApproval] {
        let time = now().timeIntervalSince1970
        return approvals.filter { $0.status == "pending" && TimeInterval($0.expiresAt) > time }
    }
    var decided: [NestApproval] { Array(approvals.filter { $0.status != "pending" }.prefix(5)) }

    var canManage: Bool { nest.isOwner }
    var canPause: Bool { canManage && ["ready", "running", "stopped"].contains(nest.state) }
    /// Only a pause its owner chose. A funding pause lifts when the package is paid for.
    var canResume: Bool {
        canManage && nest.state == "paused" && nest.pausedReason != "unfunded" && nest.pausedReason != "over_capacity"
    }

    /// Status and approval cards: cheap, and what the conversation header polls.
    func refreshStatus() async {
        do {
            nest = try await services.nest()
        } catch let error as APIError where error.status == 404 {
            gone = true
            return
        } catch {}
        if let cards = try? await services.approvals() {
            approvals = cards
            approvalsLoaded = true
        }
    }

    func load() async {
        await refreshStatus()
        guard !gone else { return }
        do { files = try await services.files() } catch { message = NestWords.refusal(error) }
        // The Nests host may be down; the rest of the page still works without its numbers.
        usage = try? await services.usage()
    }

    func decide(_ card: NestApproval, approve: Bool) async {
        guard !approve || card.isReviewable else { return }
        await perform { try await self.services.decide(card.approvalId, approve ? "approve" : "reject", card.argsHash) }
    }

    /// `size` is what the file system says, when it says anything; `read` is only called within limits.
    func upload(name: String, size: Int64?, read: @escaping () async throws -> Data) async {
        if let size, size > Self.maxUploadBytes {
            message = NestWords.refusal(APIError(status: 413, code: "too_large", detail: nil))
            return
        }
        await perform {
            let data = try await read()
            guard !data.isEmpty, Int64(data.count) <= Self.maxUploadBytes else {
                throw APIError(status: 413, code: "too_large", detail: nil)
            }
            try await self.services.upload(name, data)
        }
    }

    func deleteFile(_ file: NestFile) async {
        await perform { try await self.services.deleteFile(file.fileId) }
    }

    func pause() async { await perform { try await self.services.action("pause") } }
    func resume() async { await perform { try await self.services.action("resume") } }

    func rename(_ text: String) async {
        guard let label = NestWords.validLabel(text) else {
            message = NestWords.refusal(APIError(status: 400, code: "bad_label", detail: nil))
            return
        }
        await perform { try await self.services.rename(label) }
        await services.changed()
    }

    func export() async {
        await perform { self.exported = try await self.services.export() }
    }

    func clearExport() { exported = nil }

    /// `typed` must be the Nest's name; the postbox checks it again and tears down for good.
    func delete(typed: String) async {
        let confirm = typed.trimmingCharacters(in: .whitespacesAndNewlines)
        guard confirm == nest.label else {
            message = NestWords.refusal(APIError(status: 400, code: "confirm_required", detail: nil))
            return
        }
        if await perform(reload: false, { try await self.services.delete(confirm) }) {
            gone = true
            await services.changed()
        }
    }

    @discardableResult
    private func perform(reload: Bool = true, _ work: () async throws -> Void) async -> Bool {
        guard !busy else { return false }
        busy = true
        defer { busy = false }
        var succeeded = false
        do {
            try await work()
            message = nil
            succeeded = true
        } catch {
            message = NestWords.refusal(error)
        }
        if reload { await load() }
        return succeeded
    }
}
