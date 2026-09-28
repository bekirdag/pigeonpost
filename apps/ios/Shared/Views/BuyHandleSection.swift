import SwiftUI

struct BuyHandleSection: View {
    let store: HandleStore
    @Environment(Account.self) private var account
    let closeSettings: () -> Void

    var body: some View {
        @Bindable var store = store
        Section {
            Text("One yearly subscription covers the number of names you choose. Change your plan here or in the App Store.")
                .font(.subheadline).foregroundStyle(Theme.body)
            if let plan = store.plan {
                LabeledContent("Current plan", value: "\(plan.capacity) \(plan.capacity == 1 ? "name" : "names") · \(plan.active ? "Active" : "Expired")")
                Text("\(store.planNameCount) registered · paid through \(Date(timeIntervalSince1970: TimeInterval(plan.expiresAt)).formatted(date: .abbreviated, time: .omitted))")
                    .font(.caption).foregroundStyle(Theme.muted)
            }
            if store.activity == .loading { progress("Loading handle plans…") }
            ForEach(store.products) { product in
                productRow(product)
            }
            if let product = store.nextProduct {
                Text("\(product.displayName ?? "Handle plan"): \(product.displayPrice) per year")
                    .font(.headline).accessibilityIdentifier("handle-product-price")
                Button(store.plan?.active == true ? "Change plan for \(product.displayPrice) a year" : "Subscribe for \(product.displayPrice) a year") {
                    Task { await store.changePlan() }
                }
                .disabled(!store.canChangePlan).accessibilityIdentifier("handle-change-plan")
            }
            if store.products.isEmpty && !store.busy {
                Text("The App Store has not returned the plans yet.").font(.subheadline)
                Button("Retry loading subscriptions") { Task { await store.refresh() } }
                    .accessibilityIdentifier("handle-retry-products")
            }
        } header: { Text("All handle plans") }
        footer: {
            Text("Prices are the total yearly price for the selected plan. You have one plan at a time. Upgrades take effect immediately; downgrades take effect at renewal. After a downgrade, the first names you registered remain active up to the new limit. Other names enter a 30-day recovery period; mailbox history stays with your account.")
        }

        Section {
            if store.availableCapacity > 0 {
                Text("\(store.availableCapacity) more \(store.availableCapacity == 1 ? "name is" : "names are") included in your current plan. Register without another payment.")
                    .font(.subheadline)
            } else {
                Text("Choose an available name. If you need more capacity, the selected plan will replace your current plan.")
                    .font(.subheadline)
            }
            HStack(spacing: 3) {
                Text("/").font(.system(.body, design: .monospaced)).foregroundStyle(Theme.muted)
                TextField("yourname", text: $store.wantedName)
                    .font(.system(.body, design: .monospaced))
                    .noAutocapitalize().autocorrectionDisabled().doneKey()
                    .disabled(store.busy)
                    .onSubmit { Task { await store.checkAvailability() } }
            }
            Button("Check availability") { Task { await store.checkAvailability() } }
                .disabled(store.busy || !HandleStore.valid(store.wantedName))
            if let result = store.checkedMessage {
                Text(result).font(.subheadline).accessibilityIdentifier("handle-availability")
            }
            Button { Task { await store.buy() } } label: {
                if !store.unassigned.isEmpty {
                    Text("Finish registration — no further payment")
                } else if store.availableCapacity > 0 {
                    Text("Register name — included in your plan")
                } else if let product = store.nextProduct {
                    Text("Subscribe and register for \(product.displayPrice) a year")
                } else {
                    Text("Subscription unavailable")
                }
            }
            .font(.system(.body).weight(.semibold)).disabled(!store.canBuy)
            .accessibilityIdentifier("handle-register")
            if store.busy && store.activity != .loading { progress(activityText) }
            if let message = store.message {
                Text(message).font(.subheadline).accessibilityIdentifier("handle-message")
            }
        } header: { Text("Register a name") }

        Section {
            Button("Restore purchases") { Task { await store.refresh(restoring: true) } }.disabled(store.busy)
            Button("Refresh") { Task { await store.refresh() } }.disabled(store.busy)
            Link("Manage subscriptions", destination: URL(string: "https://apps.apple.com/account/subscriptions")!)
            Link("Privacy policy", destination: URL(string: "https://pigeonpost.dev/app-privacy.html")!)
            Link("Terms of use", destination: URL(string: "https://pigeonpost.dev/app-terms.html")!)
        } footer: {
            Text("Apple charges your account after you confirm. Your yearly plan renews automatically unless cancelled at least 24 hours before renewal. Manage or cancel in the App Store. Existing purchases can be restored to the Pigeonpost account that bought them.")
        }
    }

    private func productRow(_ product: HandleProduct) -> some View {
        let capacity = HandleCatalog.capacity(product.id) ?? 1
        return Button { store.select(product) } label: {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(product.displayName ?? "\(capacity) \(capacity == 1 ? "name" : "names") — yearly")
                        .foregroundStyle(Theme.ink).accessibilityIdentifier("handle-product-name")
                    Text("\(product.displayPrice) per year · \(capacity) \(capacity == 1 ? "name" : "names") included")
                        .font(.caption).foregroundStyle(Theme.muted)
                }
                Spacer()
                if store.plan?.active == true && store.plan?.productId == product.id {
                    Text("Current").font(.caption).foregroundStyle(Theme.muted)
                }
                if store.nextProduct?.id == product.id {
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(Color.accentColor).accessibilityLabel("Selected")
                }
            }.contentShape(Rectangle())
        }
        .buttonStyle(.plain).disabled(store.busy)
        .accessibilityIdentifier("handle-product-" + product.id)
    }

    private var activityText: String {
        switch store.activity {
        case .checking: return "Checking availability…"
        case .buying: return "Confirm with Apple…"
        case .claiming: return "Saving your subscription and name…"
        case .restoring: return "Restoring your purchases…"
        default: return "Checking your handles…"
        }
    }
    private func progress(_ text: String) -> some View {
        HStack(spacing: 10) { ProgressView(); Text(text).font(.subheadline).foregroundStyle(Theme.muted) }
    }
}

/// Account ownership is separate from the App Store purchase form.
struct AccountHandlesSection: View {
    let store: HandleStore
    let closeSettings: () -> Void
    @Environment(Account.self) private var account

    var body: some View {
        Section {
            if !store.ownershipLoaded && store.ownershipError == nil { HStack { ProgressView(); Text("Loading account handles…") } }
            ForEach(store.accountHandles) { handle in
                VStack(alignment: .leading, spacing: 6) {
                    Text(handle.name).font(.system(.body, design: .monospaced).weight(.semibold))
                        .textSelection(.enabled).accessibilityIdentifier("account-handle-" + handle.namespace)
                    Text("\(handle.active ? "Active" : "Expired") · \(handle.provider)")
                        .font(.caption).foregroundStyle(Theme.muted)
                    if let date = handle.paidThrough {
                        Text("\(handle.active ? "Paid through" : "Expired on") \(date.formatted(date: .abbreviated, time: .omitted))")
                            .font(.caption).foregroundStyle(Theme.muted)
                    }
                    #if os(iOS)
                    if let subscription = store.handles.first(where: { $0.namespace == handle.name }),
                       let product = store.products.first(where: { $0.id == subscription.productId }) {
                        if let title = product.displayName {
                            Text("App Store: \(title)").font(.caption).foregroundStyle(Theme.muted)
                        }
                        if !subscription.active {
                            Button("Renew for \(product.displayPrice) a year") { Task { await store.renew(subscription) } }
                                .disabled(store.busy)
                        }
                    }
                    #endif
                    if handle.active, let mailbox = account.mailbox(inNamespace: handle.name) {
                        Button("Open \(handle.name)") { account.act(as: mailbox); closeSettings() }
                    } else if handle.active {
                        Button("Load or create inbox") { Task { await store.repairMailbox(handle.name) } }.disabled(store.busy)
                    }
                }.padding(.vertical, 4)
            }
            if store.ownershipLoaded && store.accountHandles.isEmpty && store.ownershipError == nil {
                Text("No handles on this Pigeonpost account yet.").foregroundStyle(Theme.muted)
            }
            if let error = store.ownershipError { Text(error).font(.subheadline) }
            Button("Refresh account handles") { Task { await account.loadIdentities(); await store.refresh() } }.disabled(store.busy)
            #if os(iOS)
            if let message = store.message { Text(message).font(.subheadline) }
            Link("Manage subscriptions", destination: URL(string: "https://apps.apple.com/account/subscriptions")!)
            #endif
        } header: { Text("Your handles") }
        footer: { Text("Names belong to your Pigeonpost account across mobile, desktop and web. Expired names need renewal through their original provider.") }
    }
}
