//  SDS §5.4 in the inbox: the org badge, the Company heading with Browse rooms, and the org
//  directory under the compose field. Pure display over `Account.organizations`; the postbox
//  decides who belongs where.

import SwiftUI

/// The org's mark, worn on a member mailbox's avatar. Initials until org logos can be uploaded.
struct OrgBadge: View {
    let org: OrganizationMembership
    var size: CGFloat = 14

    var body: some View {
        Text(org.monogram)
            .font(.geist(size * 0.52, .bold))
            .minimumScaleFactor(0.5)
            .lineLimit(1)
            .foregroundStyle(Color.white)
            .frame(width: size, height: size)
            .background(Theme.navy, in: RoundedRectangle(cornerRadius: size * 0.28))
            .overlay(RoundedRectangle(cornerRadius: size * 0.28).stroke(Theme.sidebar, lineWidth: 1.5))
            .accessibilityHidden(true)
    }
}

extension View {
    /// A member mailbox's avatar carries its org's badge in the corner.
    func orgBadge(_ org: OrganizationMembership?, size: CGFloat = 14) -> some View {
        overlay(alignment: .bottomTrailing) {
            if let org { OrgBadge(org: org, size: size).offset(x: 3, y: 3) }
        }
    }
}

/// The Company heading over the org's rooms: the org's name and a way to browse the rest.
struct CompanyHeader: View {
    let orgs: [OrganizationMembership]
    let browse: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "building.2")
                .font(.geist(11, .semibold))
                .foregroundStyle(Theme.muted)
                .accessibilityHidden(true)
            Text(title)
                .font(.geist(11.5, .semibold))
                .foregroundStyle(Theme.muted)
                .textCase(.uppercase)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 0)
            // A tapped shape rather than a Button: a Button inside a List row header claims the
            // whole row on iOS and collapses the column on the Mac (see DesktopInboxView).
            Text("Browse rooms")
                .font(.geist(12, .semibold))
                .foregroundStyle(Theme.navy)
                .fixedSize()
                .contentShape(Rectangle())
                .onTapGesture(perform: browse)
                .accessibilityAddTraits(.isButton)
                .accessibilityIdentifier("company-browse-rooms")
        }
        .padding(.top, 6)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("company-section")
    }

    private var title: String {
        orgs.count == 1 ? "Company · \(orgs[0].name)" : "Company"
    }
}

/// Colleagues and company agents matching what has been typed into a "to" field.
///
/// Searches the acting mailbox's org first. Results arrive after a short pause in typing, and a
/// late answer for older text is dropped, so the list never describes something no longer typed.
struct OrgDirectorySuggestions: View {
    @Environment(Account.self) private var account
    let text: String
    let pick: (String) -> Void

    @State private var found: [OrganizationDirectoryEntry] = []

    var body: some View {
        Group {
            if !shown.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(shown) { entry in
                        HStack(spacing: 10) {
                            Avatar(peer: entry.handle, size: 26, name: entry.name)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(entry.title)
                                    .font(.geist(13.5, .medium))
                                    .foregroundStyle(Theme.ink)
                                    .lineLimit(1)
                                Text(entry.isAgent ? "\(entry.handle) · Agent" : entry.handle)
                                    .font(.geistMono(11.5))
                                    .foregroundStyle(Theme.muted)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                            Spacer(minLength: 0)
                        }
                        .padding(.vertical, 4)
                        .contentShape(Rectangle())
                        .onTapGesture { pick(entry.recipient) }
                        .accessibilityElement(children: .combine)
                        .accessibilityAddTraits(.isButton)
                        .accessibilityIdentifier("org-directory:" + entry.handle)
                    }
                }
                .accessibilityIdentifier("org-directory")
            }
        }
        .task(id: Key(text: text, mailbox: account.me?.address, orgs: account.organizations.map(\.id))) {
            await search()
        }
    }

    private struct Key: Equatable { let text: String; let mailbox: String?; let orgs: [String] }

    private var shown: [OrganizationDirectoryEntry] {
        OrganizationSurface.directoryQuery(text) == nil ? [] : OrganizationSurface.suggestions(found, typed: text)
    }

    private func search() async {
        guard let query = OrganizationSurface.directoryQuery(text) else { found = []; return }
        let orgs = OrganizationSurface.directoryOrgs(for: account.me?.address, in: account.organizations)
        guard !orgs.isEmpty else { found = []; return }
        #if DEBUG
        if Fixtures.enabled {
            found = OrganizationSurfaceFixtures.directory.filter {
                $0.handle.lowercased().contains(query.lowercased()) || ($0.name ?? "").lowercased().contains(query.lowercased())
            }
            return
        }
        #endif
        // A pause in typing before asking: the directory is rate limited per account.
        try? await Task.sleep(nanoseconds: 250_000_000)
        guard !Task.isCancelled else { return }
        let client = account.scopedClient(acting: true)
        var collected: [OrganizationDirectoryEntry] = []
        for org in orgs.prefix(3) {
            if let entries = try? await client.organizationDirectory(org.id, query: query) { collected += entries }
            guard !Task.isCancelled else { return }
        }
        found = collected
    }
}

#if DEBUG
/// `-org-surface`: the acting mailbox is a member of a fixture org, with one org room and a
/// directory, for the §5.4 screens and their UI tests. Synthetic names only.
@MainActor enum OrganizationSurfaceFixtures {
    static var enabled: Bool { Fixtures.enabled && CommandLine.arguments.contains("-org-surface") }
    static let organizationId = "org_room_fixture"

    static func memberships(me: String) -> [OrganizationMembership] {
        [OrganizationMembership(id: organizationId, name: "Example Co", namespace: "/example", status: "active",
                                myRoles: [], myMailbox: .init(address: me, handle: "/example/member"))]
    }

    static let directory: [OrganizationDirectoryEntry] = [
        OrganizationDirectoryEntry(handle: "/example/ada", address: "/k/fixtureada000000000000000", kind: "member", name: "Ada Example"),
        OrganizationDirectoryEntry(handle: "/example/alan", address: "/k/fixturealan00000000000000", kind: "member", name: "Alan Example"),
        OrganizationDirectoryEntry(handle: "/example/build", address: "/k/fixturebuild0000000000000", kind: "agent", name: "Build agent"),
    ]
}
#endif
