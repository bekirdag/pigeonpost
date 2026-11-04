//  The organization parts of the ordinary inbox (SDS §5.4).
//
//  The enterprise dashboard is for admins. Everybody else meets their company inside the normal
//  apps: the org mailbox carries the org's badge in the switcher, org rooms sit under a Company
//  heading with a way to browse the rest, org addresses autocomplete when composing, and the
//  mailbox says whose it is ("Acme Inc. · Member").
//
//  Everything here is display. The postbox decides membership, roles and who may read what on
//  every request; nothing below widens what this device can do.

import Foundation

/// One organization this person belongs to, from `GET /v1/orgs`.
struct OrganizationMembership: Decodable, Identifiable, Equatable {
    struct OwnMailbox: Decodable, Equatable {
        let address: String
        let handle: String?
    }

    let id: String
    let name: String
    let namespace: String?
    let status: String
    let myRoles: [String]?
    let myMailbox: OwnMailbox?

    init(id: String, name: String, namespace: String?, status: String, myRoles: [String]?, myMailbox: OwnMailbox?) {
        self.id = id; self.name = name; self.namespace = namespace; self.status = status
        self.myRoles = myRoles; self.myMailbox = myMailbox
    }

    /// Suspended orgs are listed by the postbox but their rooms and directory are closed.
    var readable: Bool { ["active", "past_due", "read_only"].contains(status) }

    /// The highest role held, in the words the portal uses. Every active member is a member.
    var roleName: String { OrganizationSurface.roleName(myRoles ?? []) }

    /// "Acme Inc. · Member".
    var profileLine: String { "\(name) · \(roleName)" }

    /// One or two letters for the badge. The org logo upload (R-25) does not exist yet, so the
    /// badge is the org's initials on the org colour rather than an image.
    var monogram: String { OrganizationSurface.monogram(name) }
}

struct OrganizationMembershipsResponse: Decodable { let orgs: [OrganizationMembership] }

/// A colleague or a company agent, from `GET /v1/orgs/{id}/directory?q=`.
struct OrganizationDirectoryEntry: Decodable, Identifiable, Equatable {
    let handle: String
    let address: String
    let kind: String
    let name: String?

    var id: String { address }
    var isAgent: Bool { kind == "agent" }
    /// What the compose field takes: the handle, which is what people read and trust matches on.
    var recipient: String { handle }
    var title: String {
        if let name, !name.trimmingCharacters(in: .whitespaces).isEmpty { return name }
        return handle
    }
}

struct OrganizationDirectoryResponse: Decodable { let entries: [OrganizationDirectoryEntry] }

enum OrganizationSurface {
    /// Owner > Admin > the scoped admin roles > Member, as the portal ranks them.
    static func roleName(_ roles: [String]) -> String {
        let order: [(String, String)] = [
            ("owner", "Owner"), ("admin", "Admin"), ("people_admin", "People admin"),
            ("agent_admin", "Agent admin"), ("billing_admin", "Billing admin"),
        ]
        return order.first { roles.contains($0.0) }?.1 ?? "Member"
    }

    static func monogram(_ name: String) -> String {
        let words = name.split(whereSeparator: { $0 == " " || $0 == "-" || $0 == "." })
            .filter { $0.first?.isLetter == true || $0.first?.isNumber == true }
        let letters = words.prefix(2).compactMap { $0.first.map { String($0).uppercased() } }
        return letters.isEmpty ? "#" : letters.joined()
    }

    /// The organization whose member mailbox this is. Only the membership's own canonical address
    /// associates a mailbox with an org; a handle that merely looks like `/acme/…` does not.
    static func membership(for address: String?, in orgs: [OrganizationMembership]) -> OrganizationMembership? {
        guard let address, !address.isEmpty else { return nil }
        return orgs.first { $0.readable && $0.myMailbox?.address == address }
    }

    /// The orgs whose directory a compose field should search, acting as `address`: the acting
    /// mailbox's own org first, then the person's other orgs. Readable ones only.
    static func directoryOrgs(for address: String?, in orgs: [OrganizationMembership]) -> [OrganizationMembership] {
        let readable = orgs.filter(\.readable)
        guard let own = membership(for: address, in: readable) else { return readable }
        return [own] + readable.filter { $0.id != own.id }
    }

    /// What to search for from the compose field's text, or nil when it is not worth a request:
    /// empty, or a key address someone pasted.
    static func directoryQuery(_ input: String) -> String? {
        var text = input.trimmingCharacters(in: .whitespacesAndNewlines)
        while text.hasPrefix("/") { text.removeFirst() }
        guard !text.isEmpty, !text.hasPrefix("k/") else { return nil }
        return String(text.unicodeScalars.prefix(80).map(Character.init))
    }

    /// Suggestions in a stable order, with exact duplicates (the same address from two orgs) once,
    /// and the entry already typed in full left out.
    static func suggestions(_ entries: [OrganizationDirectoryEntry], typed: String, limit: Int = 6) -> [OrganizationDirectoryEntry] {
        var seen = Set<String>()
        let typed = typed.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return entries.filter { entry in
            guard entry.handle.lowercased() != typed, seen.insert(entry.address).inserted else { return false }
            return true
        }.prefix(limit).map { $0 }
    }

    /// The groups that belong under the Company heading: org rooms of an org this person can read.
    /// A room of an org the client does not know (another account's, or a lapsed one) stays among
    /// the ordinary groups rather than vanishing.
    static func isCompanyRoom(orgId: String?, orgs: [OrganizationMembership]) -> Bool {
        guard let orgId else { return false }
        return orgs.contains { $0.id == orgId && $0.readable }
    }

    static func validIdentifier(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 128 && value.utf8.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte) || byte == 45 || byte == 95
        }
    }
}

extension PostboxClient {
    /// `GET /v1/orgs`, account-scoped. Strict: one malformed row refuses the whole answer, so a
    /// badge is never drawn from half a response.
    func organizationMemberships() async throws -> [OrganizationMembership] {
        let orgs = try await send("/v1/orgs", as: OrganizationMembershipsResponse.self).orgs
        guard orgs.count <= 64, Set(orgs.map(\.id)).count == orgs.count,
              orgs.allSatisfy({ OrganizationSurface.validIdentifier($0.id) && !$0.name.isEmpty && $0.name.count <= 120 }) else {
            throw APIError(status: 200, code: "bad_response", detail: "The organization list could not be confirmed.")
        }
        return orgs
    }

    /// `GET /v1/orgs/{id}/directory?q=`: active members with a mailbox, and active org agents.
    func organizationDirectory(_ organizationId: String, query: String) async throws -> [OrganizationDirectoryEntry] {
        guard OrganizationSurface.validIdentifier(organizationId) else {
            throw APIError(status: 0, code: "room_input", detail: "This organization is invalid.")
        }
        let items = [URLQueryItem(name: "q", value: query), URLQueryItem(name: "limit", value: "20")]
        let entries = try await send("/v1/orgs/\(organizationId)/directory", query: items, as: OrganizationDirectoryResponse.self).entries
        guard entries.count <= 50, entries.allSatisfy({ $0.handle.hasPrefix("/") && $0.address.hasPrefix("/") }) else {
            throw APIError(status: 200, code: "bad_response", detail: "The directory could not be confirmed.")
        }
        return entries
    }
}
