//  The top of Settings: a readable name for this account's agents.
//
//  Sits above the archive because it is the one thing in Settings a new person is looking for, and
//  below nothing because it is also the only thing here that costs money — putting it anywhere else
//  would mean surfacing a price in a screen someone opened to change a setting.

import SwiftUI

struct BuyHandleSection: View {
    @Environment(Account.self) private var account
    @State private var store: HandleStore?

    var body: some View {
        Group {
            if let store {
                content(store)
            } else {
                Color.clear.frame(height: 0)
            }
        }
        .task {
            if store == nil { store = HandleStore(account: account) }
            #if DEBUG
            if let staged = Fixtures.handleState {
                store?.stage(staged)
                return
            }
            #endif
            await store?.refresh()
        }
    }

    @ViewBuilder
    private func content(_ store: HandleStore) -> some View {
        @Bindable var store = store
        switch store.phase {
        case .idle, .loading:
            Section {
                HStack(spacing: 10) {
                    ProgressView()
                    Text("Checking your handle…")
                        .font(.system(size: 14))
                        .foregroundStyle(Theme.muted)
                }
            }

        case .unavailable:
            // Deliberately nothing. A store that cannot sell should not advertise.
            EmptyView()

        case let .owned(namespace, renews):
            Section {
                LabeledContent("Your handle") {
                    Text(namespace)
                        .font(.system(size: 15, weight: .semibold, design: .monospaced))
                        .foregroundStyle(Theme.ink)
                }
                if let renews {
                    LabeledContent("Renews", value: renews.formatted(date: .abbreviated, time: .omitted))
                        .font(.system(size: 14))
                }
            } header: {
                Text("Handle")
            } footer: {
                Text("Every mailbox on this account can use \(namespace)/… as its address. Manage or cancel the subscription in the App Store.")
            }

        case let .forSale(displayPrice):
            Section {
                HStack(spacing: 2) {
                    Text("/")
                        .font(.system(size: 16, weight: .semibold, design: .monospaced))
                        .foregroundStyle(Theme.muted)
                    TextField("yourname", text: $store.wantedName)
                        .font(.system(size: 16, design: .monospaced))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .submitLabel(.done)
                }
                Button {
                    Task { await store.buy() }
                } label: {
                    HStack {
                        Text("Buy for \(displayPrice) a year")
                            .font(.system(size: 15, weight: .semibold))
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(Theme.muted)
                    }
                }
                .disabled(HandleStore.tidy(store.wantedName).isEmpty)
                Button("Restore a purchase") { Task { await store.restore() } }
                    .font(.system(size: 14))
            } header: {
                Text("Handle")
            } footer: {
                Text("Your mailboxes have cryptographic addresses, which are exact but unreadable. A handle is a readable name that stands in for them — /yourname/agent1 instead of /k/fd7qzt3z…. Renews yearly; cancel any time in the App Store.")
            }

        case .buying:
            Section {
                HStack(spacing: 10) {
                    ProgressView()
                    Text("Talking to the App Store…")
                        .font(.system(size: 14))
                        .foregroundStyle(Theme.muted)
                }
            }

        case let .failed(why):
            Section {
                Text(why)
                    .font(.system(size: 14))
                    .foregroundStyle(Theme.Pill.blockedText)
                Button("Try again") { Task { await store.refresh() } }
                    .font(.system(size: 14))
            } header: {
                Text("Handle")
            }
        }
    }
}
