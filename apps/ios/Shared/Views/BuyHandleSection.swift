import SwiftUI

struct BuyHandleSection: View {
    let store: HandleStore
    @Environment(Account.self) private var account
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        @Bindable var store = store
        Section {
            if !store.ownershipLoaded && store.ownershipError == nil { progress("Loading account handles…") }
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
                    if handle.active, let mailbox = account.mailbox(inNamespace: handle.name) {
                        Button("Open \(handle.name)") { account.act(as: mailbox); dismiss() }
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
        } header: { Text("Your handles") }
        footer: { Text("Names belong to your Pigeonpost account across mobile, desktop and web. Expired names need renewal through their original provider.") }
        Section {
            if !store.loaded && store.activity == .loading {
                progress("Checking your handles…")
            }
            if store.loaded {
                LabeledContent("Active App Store subscriptions", value: "\(store.activeCount) of \(store.maximum)")
            }
            ForEach(store.handles) { handle in
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text(handle.namespace).font(.system(.body, design: .monospaced).weight(.semibold))
                            .textSelection(.enabled)
                        Spacer()
                        Text(handle.active ? "Active" : "Expired")
                            .font(.caption).foregroundStyle(Theme.muted)
                    }
                    Text("Paid through \(handle.paidThrough.formatted(date: .abbreviated, time: .omitted))")
                        .font(.caption).foregroundStyle(Theme.muted)
                    if let title = store.products.first(where: { $0.id == handle.productId })?.displayName {
                        Text("App Store: \(title)").font(.caption).foregroundStyle(Theme.muted)
                    }
                    if handle.active {
                        if let mailbox = account.mailbox(inNamespace: handle.namespace) {
                            Button {
                                account.act(as: mailbox)
                                dismiss()
                            } label: {
                                Label("Open \(mailbox.handle ?? mailbox.address)", systemImage: "tray")
                                    .font(.subheadline)
                            }
                            .accessibilityIdentifier("handle-inbox-" + handle.namespace)
                        } else if store.missingMailboxes.contains(handle.namespace) {
                            Button("Create or reload inbox") { Task { await store.repairMailbox(handle.namespace) } }
                                .font(.subheadline).disabled(store.busy)
                        }
                    } else if let product = store.products.first(where: { $0.id == handle.productId }) {
                        Button("Renew for \(product.displayPrice) a year") { Task { await store.renew(handle) } }
                            .font(.subheadline).disabled(store.busy)
                    }
                }
                .padding(.vertical, 4)
            }

            if store.enabled && (store.activeCount < store.maximum || !store.unassigned.isEmpty) {
                HStack(spacing: 3) {
                    Text("/").font(.system(.body, design: .monospaced)).foregroundStyle(Theme.muted)
                    TextField("yourname", text: $store.wantedName)
                        .font(.system(.body, design: .monospaced))
                        .noAutocapitalize().autocorrectionDisabled().doneKey()
                        .disabled(store.activity == .buying || store.activity == .claiming)
                        .onSubmit { Task { await store.checkAvailability() } }
                }
                if !store.wantedName.isEmpty && !HandleStore.valid(store.wantedName) {
                    Text("Use 1–32 letters, numbers, dots, underscores or hyphens. Start and end with a letter or number.")
                        .font(.caption).foregroundStyle(Theme.Pill.blockedText)
                }
                Button("Check availability") { Task { await store.checkAvailability() } }
                    .disabled(store.busy || !HandleStore.valid(store.wantedName))
                if let result = store.checkedMessage {
                    Text(result).font(.subheadline)
                        .foregroundStyle(store.availability?.available == true ? Theme.ink : Theme.Pill.blockedText)
                        .accessibilityIdentifier("handle-availability")
                }
                Button {
                    Task { await store.buy() }
                } label: {
                    if !store.unassigned.isEmpty {
                        Text("Finish registration — no further payment")
                    } else {
                        Text("Buy for \(store.nextProduct?.displayPrice ?? store.products.first?.displayPrice ?? "…") a year")
                    }
                }
                .font(.system(.body).weight(.semibold))
                .disabled(!store.canBuy)
                if let product = store.nextProduct ?? store.products.first, store.unassigned.isEmpty {
                    Text("Each handle: \(product.displayPrice)/year. Ten handles: \(product.total(for: 10))/year.")
                        .font(.caption).foregroundStyle(Theme.muted)
                }
            } else if store.activeCount >= store.maximum {
                Text("You have all ten handle subscriptions.")
                    .font(.subheadline).foregroundStyle(Theme.muted)
            }

            if store.activity != .none && store.activity != .loading {
                progress(activityText)
            }
            if let message = store.message {
                Text(message).font(.subheadline).foregroundStyle(Theme.body)
                    .accessibilityIdentifier("handle-message")
            }
            HStack {
                Button("Restore purchases") { Task { await store.refresh(restoring: true) } }
                Spacer()
                Button("Refresh") { Task { await store.refresh() } }
            }
            .disabled(store.busy)
            Link("Manage subscriptions", destination: URL(string: "https://apps.apple.com/account/subscriptions")!)
            Link("Privacy policy", destination: URL(string: "https://pigeonpost.dev/app-privacy.html")!)
            Link("Terms of use", destination: URL(string: "https://pigeonpost.dev/app-terms.html")!)
        } header: {
            Text("App Store subscriptions")
        } footer: {
            Text("Each name has its own yearly subscription and stays associated with that subscription. Apple charges your account after you confirm. Subscriptions renew automatically unless cancelled at least 24 hours before renewal. Manage or cancel each one in the App Store.")
        }
    }

    private var activityText: String {
        switch store.activity {
        case .checking: return "Checking availability…"
        case .buying: return "Confirm your purchase with Apple…"
        case .claiming: return "Registering your handle…"
        case .restoring: return "Restoring your purchases…"
        default: return "Checking your handles…"
        }
    }
    private func progress(_ text: String) -> some View {
        HStack(spacing: 10) { ProgressView(); Text(text).font(.subheadline).foregroundStyle(Theme.muted) }
    }
}
