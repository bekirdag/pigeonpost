//  Products (R-35, Nests Phase B2): Buy an Inbox, Buy Nests, Services — one page, three tabs,
//  shown only when the postbox has `nests` on for the account.
//
//  Prices come from the App Store (localized by Apple) and are never written here. Buying asks the
//  postbox first (`/v1/nest-purchases/intents`), so a second charge is refused before any sheet.

import StoreKit
import SwiftUI

struct ProductsView: View {
    enum Tab: String, CaseIterable, Identifiable {
        case inbox = "Buy an Inbox"
        case nests = "Buy Nests"
        case services = "Services"
        var id: String { rawValue }
    }

    @Environment(Account.self) private var account
    let handle: HandleStore?
    let closeSettings: () -> Void
    @State private var tab: Tab
    @State private var context = "personal"
    @State private var orgs: [OrgRef] = []

    init(handle: HandleStore?, closeSettings: @escaping () -> Void, tab: Tab = .nests) {
        self.handle = handle
        self.closeSettings = closeSettings
        _tab = State(initialValue: tab)
    }

    struct OrgRef: Decodable, Identifiable, Hashable {
        let id: String
        let name: String
        var myRoles: [String]? = nil
        /// Roles the postbox allows to create and delete an organization's Nests.
        var managesNests: Bool { myRoles?.contains { ["owner", "admin", "agent_admin"].contains($0) } == true }
    }

    var body: some View {
        List {
            Section {
                Picker("Products", selection: $tab) {
                    ForEach(Tab.allCases) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("products-tabs")
                if !orgs.isEmpty {
                    Picker("For", selection: $context) {
                        Text("Personal").tag("personal")
                        ForEach(orgs) { Text($0.name).tag($0.id) }
                    }
                    .accessibilityIdentifier("products-context")
                }
            }
            switch tab {
            case .inbox:
                #if os(iOS)
                if let handle { BuyHandleSection(store: handle, closeSettings: closeSettings) }
                #else
                Section { Text("Buy a handle from Pigeonpost on your iPhone or iPad, or at pigeonpost.dev. It works here too.") }
                #endif
            case .nests:
                if context == "personal" { BuyNestsSection(store: account.nests) }
                else if let org = orgs.first(where: { $0.id == context }) { OrgNestsSection(org: org) }
            case .services:
                ServicesSection()
            }
        }
        .task {
            await account.nests.refresh()
            #if DEBUG
            if Fixtures.enabled { return }
            #endif
            if account.features.orgs {
                struct Answer: Decodable { let orgs: [OrgRef] }
                orgs = (try? await account.client.send("/v1/orgs", as: Answer.self).orgs) ?? []
            }
        }
    }
}

struct BuyNestsSection: View {
    @Bindable var store: NestStore
    @State private var managing = false
    @State private var newName = ""

    var body: some View {
        Section {
            Text("A Nest is your own AI agent with a private workspace. You talk to it in an ordinary Pigeonpost conversation.")
                .font(.subheadline).foregroundStyle(.secondary)
            if let package = store.currentPackage {
                LabeledContent("Your package", value: package.name)
                    .accessibilityIdentifier("nest-current-package")
            }
            if let paid = store.funding?.paidThrough {
                LabeledContent("Paid through", value: Date(timeIntervalSince1970: TimeInterval(paid)).formatted(date: .abbreviated, time: .omitted))
            }
            if store.fundedElsewhere {
                Text("Your Nests are paid for on the web or Google Play. Change your package there.")
                    .font(.footnote).foregroundStyle(.secondary)
            }
            if !store.duplicates.isEmpty {
                Text("You are also paying for another Nest package, and it adds nothing. Cancel it where you bought it.")
                    .font(.footnote).foregroundStyle(.orange)
                    .accessibilityIdentifier("nest-duplicate-package")
            }
        } header: { Text("Nests") }

        Section {
            ForEach(store.forSale) { package in
                NestPackageRow(package: package, price: store.price(package), current: store.isCurrent(package),
                               disabled: store.busy || store.fundedElsewhere || store.price(package) == nil) {
                    Task { await store.buy(package) }
                }
            }
            if store.forSale.isEmpty && store.activity == .loading {
                ProgressView().frame(maxWidth: .infinity)
            }
        } header: {
            Text("Packages")
        } footer: {
            Text("Monthly, renews automatically until you cancel in your App Store subscriptions. Model allowances are shared by the package's Nests.")
        }

        if !store.setups.isEmpty {
            Section("Being set up") {
                ForEach(store.setups) { setup in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(setup.label)
                            Text(setup.status == "failed" ? "Paid. Setup did not finish." : "Paid. Setting up…")
                                .font(.footnote).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Retry") { Task { await store.retry(setup) } }
                            .disabled(store.busy)
                            .accessibilityIdentifier("nest-setup-retry")
                    }
                }
            }
        }

        if store.canCreate {
            Section {
                TextField("Name", text: $newName).accessibilityIdentifier("nest-new-name")
                Button("Create") {
                    let label = newName
                    Task { await store.create(label: label); if store.message == nil { newName = "" } }
                }
                .disabled(store.busy || NestWords.validLabel(newName) == nil)
                .accessibilityIdentifier("nest-create")
            } header: { Text("New Nest") } footer: {
                Text("Your package has room for another Nest.")
            }
        }

        if !store.nests.isEmpty {
            Section("Your Nests") {
                ForEach(store.nests) { nest in
                    NavigationLink {
                        NestDetailView(nest: nest)
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(nest.label)
                            Text(NestWords.status(nest)).font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityIdentifier("nest-row")
                }
            }
        }

        Section {
            Button("Restore purchases") { Task { await store.restore() } }
                .disabled(store.busy)
                .accessibilityIdentifier("nest-restore")
            #if os(iOS)
            Button("Manage subscription") { managing = true }
            #else
            Link("Manage subscription", destination: URL(string: "https://apps.apple.com/account/subscriptions")!)
            #endif
        } footer: {
            if let message = store.message {
                Text(message).foregroundStyle(.red).accessibilityIdentifier("nest-message")
            } else if store.awaitingClaim {
                Text("Your payment is safe. Your Nest is set up as soon as Pigeonpost confirms it.")
            }
        }
        #if os(iOS)
        .manageSubscriptionsSheet(isPresented: $managing)
        #endif
    }
}

struct NestPackageRow: View {
    let package: NestPackage
    let price: String?
    let current: Bool
    let disabled: Bool
    let buy: () -> Void

    private var allowance: String {
        guard package.managedAI else { return "No AI connected: bring your own setup later" }
        let input = package.inputTokens.formatted(.number.notation(.compactName))
        let output = package.outputTokens.formatted(.number.notation(.compactName))
        return "Pigeonpost AI: \(input) input and \(output) output tokens a month"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Text(package.name).font(.headline)
                Spacer()
                Text(price.map { "\($0) / month" } ?? "Unavailable").font(.subheadline.weight(.semibold))
            }
            Text(package.slots == 1 ? "1 Nest" : "\(package.slots) Nests").font(.subheadline)
            Text("\(NestWords.bytes(package.storageBytes)) of files · \(package.activeHours) running hours a month")
                .font(.footnote).foregroundStyle(.secondary)
            Text(allowance).font(.footnote).foregroundStyle(.secondary)
            HStack {
                Spacer()
                if current {
                    Label("Current", systemImage: "checkmark.circle.fill").font(.subheadline).foregroundStyle(.green)
                } else {
                    Button(action: buy) { Text("Subscribe") }
                        .buttonStyle(.borderedProminent)
                        .disabled(disabled)
                        .accessibilityLabel("Subscribe to \(package.name)")
                        .accessibilityIdentifier("nest-buy-" + package.code)
                }
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
    }
}

struct ServicesSection: View {
    private let services = [
        ("Personal assistant", "Tasks, reminders, scheduling drafts and follow-ups.", "person.crop.circle.badge.checkmark"),
        ("AI CRM", "Contacts, pipeline, notes and follow-up drafts.", "chart.bar.doc.horizontal"),
        ("AI HR", "Job descriptions, candidates and interview briefs.", "person.2.badge.gearshape"),
        ("Shopping assistant", "Wish lists, comparisons and purchase recommendations.", "cart"),
        ("Marketing assistant", "Brand knowledge, campaign plans and drafts for review.", "megaphone"),
    ]

    var body: some View {
        Section {
            ForEach(services, id: \.0) { name, detail, icon in
                HStack(spacing: 12) {
                    Image(systemName: icon).frame(width: 28).foregroundStyle(.tint).accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(name)
                        Text(detail).font(.footnote).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Text("Coming soon").font(.caption).foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            }
        } header: { Text("Ready-to-use services") } footer: {
            Text("Services run on a Nest. They are not for sale yet.")
        }
    }
}

struct OrgNestsSection: View {
    @Environment(Account.self) private var account
    let org: ProductsView.OrgRef
    @State private var list: OrgNestList?
    @State private var handle = ""
    @State private var label = ""
    @State private var message: String?

    var body: some View {
        Section {
            if let list {
                LabeledContent("Nest slots", value: "\(list.allowance.used) of \(list.allowance.slots) in use")
                ForEach(list.nests) { row in
                    NavigationLink {
                        NestDetailView(nest: row.view(org: org.id))
                    } label: {
                        VStack(alignment: .leading) {
                            Text(row.label)
                            Text(row.role == "owner" ? "You manage it" : "You operate it").font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                }
            } else {
                ProgressView()
            }
        } header: { Text("\(org.name) Nests") } footer: {
            Text("Company Nests use a package held by the organization, not a personal App Store subscription. Each uses one of its agent addresses.")
        }
        .task(id: org.id) { list = try? await account.client.orgNests(org.id) }
        if org.managesNests {
            Section("New company Nest") {
                TextField("Handle (for example hr)", text: $handle).autocorrectionDisabled()
                TextField("Name", text: $label)
                Button("Create") {
                    Task {
                        do {
                            try await account.client.createOrgNest(org.id, handle: handle, label: label)
                            handle = ""; label = ""; message = nil
                            list = try? await account.client.orgNests(org.id)
                        } catch { message = NestWords.refusal(error) }
                    }
                }
                .disabled(handle.isEmpty || NestWords.validLabel(label) == nil)
                if let message { Text(message).foregroundStyle(.red).font(.footnote) }
            }
        }
    }
}
