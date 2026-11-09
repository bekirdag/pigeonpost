//  Nests (Phase B): the postbox's answers, as the app reads them. Field names follow
//  openapi/pigeonpost.v1.yaml (tag `nests`); the client decodes snake_case into camelCase.
//
//  Nothing here decides what anybody bought or may do: the postbox verifies purchases with Apple
//  and resolves every role. The app shows what it was told.

import Foundation

struct NestPackage: Decodable, Equatable, Identifiable, Sendable {
    let code: String
    let name: String
    let rank: Int
    let slots: Int
    let storageBytes: Int64
    let activeHours: Int
    let inputTokens: Int64
    let outputTokens: Int64
    /// `managed` (Pigeonpost AI) or `none` (an empty Nest).
    let model: String
    let sellable: Bool
    let appleProductId: String
    var id: String { code }
    var managedAI: Bool { model == "managed" }
}

struct NestCatalog: Decodable, Equatable, Sendable {
    struct Rails: Decodable, Equatable, Sendable { let apple: Bool; let google: Bool; let web: Bool }
    let catalogVersion: String
    let packages: [NestPackage]
    let rails: Rails
    /// The StoreKit `appAccountToken` every Nest purchase of this account must carry.
    let appAccountToken: String
}

struct NestFunding: Decodable, Equatable, Sendable {
    let entitlementId: String
    let source: String
    let package: String
    let status: String
    let paidThrough: Int?
}

struct NestAllowance: Decodable, Equatable, Sendable {
    let funding: NestFunding?
    let slots: Int
    let used: Int
    let duplicates: [NestFunding]
}

struct NestSetup: Decodable, Equatable, Identifiable, Sendable {
    let setupId: String
    let label: String
    /// `pending` (being set up, retried automatically) or `failed` (the owner retries).
    let status: String
    let attempts: Int
    let lastError: String?
    var id: String { setupId }
}

/// One Nest with its status header (`GET /v1/nests/{nest}` and each row of `GET /v1/nests`).
struct NestView: Decodable, Equatable, Identifiable, Sendable {
    let nestId: String
    let address: String?
    let label: String
    let state: String
    let funding: String?
    /// `ready`, `working`, `needs_you`, `paused` or `unavailable`.
    let status: String?
    let statusReason: String?
    let pausedReason: String?
    let deleteAfter: Int?
    var role: String? = nil
    var orgId: String? = nil
    var id: String { nestId }
    var isOwner: Bool { role == nil || role == "owner" }
}

struct NestSnapshot: Decodable, Equatable, Sendable {
    let allowance: NestAllowance
    let package: NestPackage?
    let nests: [NestView]
    let setups: [NestSetup]
}

struct NestIntent: Decodable, Equatable, Sendable {
    let intentId: String
    let appleProductId: String
    let appAccountToken: String
}

struct NestFile: Decodable, Equatable, Identifiable, Sendable {
    struct Coverage: Decodable, Equatable, Sendable { let unitsTotal: Int?; let unitsText: Int? }
    let fileId: String
    let name: String
    let bytes: Int64
    let kind: String?
    /// `parsed`, `partial`, `rejected`, `failed` or `uploading`.
    let status: String
    let reason: String?
    let coverage: Coverage
    let warnings: [String]
    var id: String { fileId }
}

struct NestFiles: Decodable, Equatable, Sendable {
    let files: [NestFile]
    let usedBytes: Int64
    let quotaBytes: Int64
}

/// An approval card: the exact call the Nest proposed, and the hash that binds a decision to it.
struct NestApproval: Decodable, Equatable, Identifiable, Sendable {
    struct Call: Decodable, Equatable, Sendable {
        struct Args: Decodable, Equatable, Sendable { let to: String?; let body: String? }
        let tool: String
        let args: Args
    }
    let approvalId: String
    let tool: String
    let call: Call?
    let argsHash: String
    let status: String
    let expiresAt: Int
    let result: String?
    var id: String { approvalId }
}

struct NestUsage: Decodable, Equatable, Sendable {
    struct Tokens: Decodable, Equatable, Sendable { let used: Int64?; let limit: Int64? }
    struct Package: Decodable, Equatable, Sendable { let code: String; let activeHoursPooled: Int }
    let period: String?
    let inputTokens: Tokens
    let outputTokens: Tokens
    let runningSecs: Int64?
    let filesBytes: Int64
    let package: Package?
}

struct OrgNestList: Decodable, Equatable, Sendable {
    struct Row: Decodable, Equatable, Identifiable, Sendable {
        let nestId: String; let address: String?; let label: String; let state: String; let role: String
        var id: String { nestId }
    }
    struct Slots: Decodable, Equatable, Sendable { let slots: Int; let used: Int }
    let nests: [Row]
    let allowance: Slots
}

/// What to tell a person when the postbox refuses. Codes are the postbox's (docs/nests/phase-b.md).
enum NestWords {
    static func refusal(_ error: Error) -> String {
        guard let api = error as? APIError else { return error.localizedDescription }
        switch api.code {
        case "nest_package_active":
            return "A Nest package bought elsewhere already pays for your Nests. Change it where you bought it."
        case "nest_package_owned": return "You already have this package. Use Restore purchases if it is missing."
        case "purchase_already_used": return "This App Store subscription belongs to another Pigeonpost account."
        case "purchase_account_mismatch": return "This purchase was made for a different Pigeonpost account."
        case "purchase_inactive": return "The App Store has not confirmed an active payment yet."
        case "no_nest_package": return "Buy a Nest package first."
        case "nest_slots_full": return "Every Nest your package includes is in use. Upgrade or delete one."
        case "needs_payment": return "This Nest is paused until its package is paid for."
        case "over_capacity": return "Your package now has fewer Nests than you have. Delete one or upgrade."
        case "approval_changed": return "The action changed since you saw it. Look at it again."
        case "approval_closed": return "This approval was already decided, replaced or expired."
        case "recipient_not_allowed": return "A Nest may only message you or people you have allowed."
        case "storage_full": return "This Nest's storage is full."
        case "too_large", "export_too_large": return "That is too large."
        case "rate_limited", "too_many_pending": return "Too many attempts. Try again later."
        case "feature_disabled": return "Nests are not available for this account yet."
        case "billing_unavailable": return "The App Store could not be checked. Try again."
        default: return api.errorDescription ?? "Something went wrong."
        }
    }

    static func status(_ view: NestView) -> String {
        switch view.status {
        case "ready": return "Ready"
        case "working": return "Working…"
        case "needs_you": return view.statusReason == "payment" ? "Needs you: renew the package" : "Needs you"
        case "paused": return view.statusReason == "over_capacity" ? "Paused: over your package" : "Paused"
        default: return view.statusReason == "setting_up" ? "Setting up…" : "Unavailable"
        }
    }

    /// A display name for the `x-file-name` header: control characters dropped, at most 200
    /// characters, percent-encoded UTF-8. Only ever shown; the postbox never uses it as a path.
    static func fileName(_ name: String) -> String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_.~"))
        let clean = String(String.UnicodeScalarView(name.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }))
        return String(clean.prefix(200)).addingPercentEncoding(withAllowedCharacters: allowed) ?? "file"
    }

    static func bytes(_ value: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: value, countStyle: .file)
    }
}
