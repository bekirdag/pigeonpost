import Foundation

// SDS §5.4: badges, Company rooms, directory autocomplete and the "Org · Role" line.

enum AuthError: Error { case sessionExpired }
@MainActor final class Session {
    func token() async throws -> String { "synthetic" }
    func renew() async throws -> String { "synthetic" }
}

func check(_ ok: Bool, _ what: String) {
    if ok { print("ok - \(what)") } else { print("FAIL - \(what)"); exit(1) }
}

@main struct OrganizationSurfaceTests {
    static func org(_ id: String, _ name: String = "Acme Inc.", status: String = "active",
                    roles: [String] = [], mailbox: String? = "/k/mine") -> OrganizationMembership {
        OrganizationMembership(id: id, name: name, namespace: "/acme", status: status, myRoles: roles,
                               myMailbox: mailbox.map { .init(address: $0, handle: "/acme/jo") })
    }

    static func entry(_ handle: String, _ address: String, kind: String = "member", name: String? = nil) -> OrganizationDirectoryEntry {
        OrganizationDirectoryEntry(handle: handle, address: address, kind: kind, name: name)
    }

    static func main() throws {
        check(OrganizationSurface.roleName(["admin", "owner"]) == "Owner", "owner outranks admin")
        check(OrganizationSurface.roleName(["billing_admin", "people_admin"]) == "People admin", "scoped admin roles keep the portal's order")
        check(OrganizationSurface.roleName([]) == "Member", "every active member is a member")
        check(OrganizationSurface.roleName(["something_new"]) == "Member", "an unknown role is not shown as a power")

        check(OrganizationSurface.monogram("Acme Inc.") == "AI", "two initials")
        check(OrganizationSurface.monogram("pigeonpost") == "P", "one word, one letter")
        check(OrganizationSurface.monogram("") == "#", "an empty name still draws a badge")
        check(OrganizationSurface.monogram("  — Ünlü Yazılım A.Ş.") == "ÜY", "non-ASCII letters, punctuation skipped")

        let acme = org("org_acme", roles: ["admin"])
        let other = org("org_other", "Other Ltd", mailbox: "/k/elsewhere")
        let suspended = org("org_gone", "Gone Co", status: "suspended", mailbox: "/k/gone")
        let orgs = [acme, other, suspended]
        check(OrganizationSurface.membership(for: "/k/mine", in: orgs) == acme, "the member mailbox's own org")
        check(OrganizationSurface.membership(for: "/k/gone", in: orgs) == nil, "a suspended org draws no badge")
        check(OrganizationSurface.membership(for: nil, in: orgs) == nil, "no mailbox, no org")
        check(OrganizationSurface.membership(for: "/k/unknown", in: orgs) == nil, "only the canonical address associates")
        check(acme.profileLine == "Acme Inc. · Admin", "the profile line")
        check(org("org_x").profileLine == "Acme Inc. · Member", "a plain member's profile line")

        check(OrganizationSurface.directoryOrgs(for: "/k/elsewhere", in: orgs).map(\.id) == ["org_other", "org_acme"],
              "the acting mailbox's org is searched first")
        check(OrganizationSurface.directoryOrgs(for: "/k/personal", in: orgs).map(\.id) == ["org_acme", "org_other"],
              "a personal mailbox searches every readable org")

        check(OrganizationSurface.directoryQuery("/acme/jo") == "acme/jo", "leading slash dropped")
        check(OrganizationSurface.directoryQuery("/") == nil, "nothing typed, no request")
        check(OrganizationSurface.directoryQuery("  ") == nil, "blank, no request")
        check(OrganizationSurface.directoryQuery("/k/abc123") == nil, "a pasted key address is not searched")
        check(OrganizationSurface.directoryQuery("/" + String(repeating: "a", count: 200))?.count == 80, "the query is bounded")

        let found = [entry("/acme/jo", "/k/1", name: "Jo Doe"), entry("/acme/jane", "/k/2"), entry("/acme/jane", "/k/2"),
                     entry("/acme/bot", "/k/3", kind: "agent", name: "Build bot")]
        let shown = OrganizationSurface.suggestions(found, typed: "/ACME/JO")
        check(shown.map(\.address) == ["/k/2", "/k/3"], "duplicates once, the fully typed entry left out")
        check(OrganizationSurface.suggestions(found, typed: "/", limit: 1).count == 1, "the limit holds")
        check(found[0].title == "Jo Doe" && found[1].title == "/acme/jane" && found[3].isAgent, "titles and kinds")

        check(OrganizationSurface.isCompanyRoom(orgId: "org_acme", orgs: orgs), "a room of a known org is a Company room")
        check(!OrganizationSurface.isCompanyRoom(orgId: "org_gone", orgs: orgs), "a suspended org's room is not")
        check(!OrganizationSurface.isCompanyRoom(orgId: "org_unknown", orgs: orgs), "an unknown org's room stays with the groups")
        check(!OrganizationSurface.isCompanyRoom(orgId: nil, orgs: orgs), "a personal group is not")

        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let wire = #"{"orgs":[{"id":"org_1","name":"Acme Inc.","namespace":"/acme","status":"active","seats":5,"my_roles":["admin"],"my_member_id":"mem_1","my_mailbox":{"address":"/k/a","handle":"/acme/jo"}}]}"#
        let decoded = try decoder.decode(OrganizationMembershipsResponse.self, from: Data(wire.utf8)).orgs
        check(decoded.count == 1 && decoded[0].roleName == "Admin" && decoded[0].myMailbox?.address == "/k/a", "the /v1/orgs shape decodes")
        let directory = #"{"entries":[{"handle":"/acme/bot","address":"/k/b","kind":"agent","name":null}]}"#
        let entries = try decoder.decode(OrganizationDirectoryResponse.self, from: Data(directory.utf8)).entries
        check(entries.first?.title == "/acme/bot" && entries.first?.isAgent == true, "the directory shape decodes")
        print("organization surface: all checks passed")
    }
}
