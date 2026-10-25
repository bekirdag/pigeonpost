import SwiftUI

/// A human approves the machine's own credential. Codes, previews and confirmation proofs live
/// only in this sheet; its store checks the authorization again after every suspension.
struct DeviceApprovalScreen: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(Session.self) private var session
    @Environment(Account.self) private var account
    @State private var store: DeviceApprovalStore
    @State private var input: String
    @State private var inputProblem: String?
    @State private var confirmation: UUID?
    @State private var started = false
    @FocusState private var enteringCode: Bool
    private let initialCode: DevicePairingCode?

    init(session: Session, code: DevicePairingCode? = nil) {
        #if DEBUG
        _store = State(initialValue: DeviceApprovalFixtures.store(session: session))
        #else
        _store = State(initialValue: DeviceApprovalStore(session: session))
        #endif
        _input = State(initialValue: code?.displayValue ?? "")
        initialCode = code
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("ABCD-EFGH", text: Binding(get: { input }, set: { value in
                        guard value != input else { return }
                        store.changeCode(); confirmation = nil; inputProblem = nil
                        input = String(value.prefix(128))
                    }))
                    .font(.system(.body, design: .monospaced))
                    .focused($enteringCode)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.characters)
                    #endif
                    .disabled(store.busy)
                    .accessibilityLabel("Machine code")
                    .accessibilityIdentifier("device-approval-code")
                    Button("Review request", action: resolve)
                        .disabled(store.busy || input.isEmpty || (store.retryAt.map { $0 > Date() } ?? false))
                        .accessibilityIdentifier("device-approval-review")
                    if let inputProblem { Text(inputProblem).foregroundStyle(.red) }
                } header: { Text("Code from your machine") }
                if let preview = store.preview { details(preview) }
                if store.busy {
                    HStack { ProgressView(); Text(store.phase == .verifying ? "Verifying sign-in…" : "Checking request…") }
                        .accessibilityIdentifier("device-approval-busy")
                }
                if let message = store.message {
                    Section { Text(message).accessibilityIdentifier("device-approval-message") }
                }
                decision
            }
            .accessibilityIdentifier("device-approval-form")
            .font(.geist(14)).tint(Theme.navy).sheetSurface()
            .navigationTitle("Sign in a machine").inlineTitle()
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { close() }.disabled(store.busy)
                }
            }
        }
        #if os(macOS)
        .frame(width: 540, height: 680)
        #endif
        .interactiveDismissDisabled(store.busy)
        .confirmationDialog("Approve this machine?", isPresented: Binding(get: { confirmation != nil }, set: { if !$0 { confirmation = nil } }), titleVisibility: .visible) {
            // Capture this proof in the presented action. A new code, account or preview retires it.
            if let proof = confirmation {
                Button("Approve machine") { confirmation = nil; Task { await store.approve(proof: proof) } }
                Button("Cancel", role: .cancel) { confirmation = nil }
            }
        } message: { Text("Only continue if you started this request and the terminal shows the same confirmation number.") }
        .task {
            guard !started else { return }; started = true
            if let initialCode { await store.resolve(initialCode) }
        }
        .task {
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(1)) } catch { return }
                store.synchronize(); store.refreshExpiry()
            }
        }
        .onChange(of: session.authorizationId) { _, _ in store.synchronize(); confirmation = nil }
        .onChange(of: store.proofId) { _, _ in confirmation = nil }
        .onChange(of: account.features.orgs) { _, enabled in if !enabled { close() } }
        .onDisappear {
            // A system authentication presentation may cover this sheet. Its verified result is
            // scoped by the store; ordinary dismissal retires every code and proof immediately.
            if !store.busy { store.retire(); input = ""; confirmation = nil }
        }
    }

    @ViewBuilder private func details(_ preview: DevicePairingPreview) -> some View {
        Section {
            LabeledContent("Organization", value: preview.org.name)
            Text(preview.handle).font(.geist(20, .semibold)).textSelection(.enabled)
                .accessibilityIdentifier("device-approval-handle")
            if preview.createsAgent == true, preview.state != "approved" {
                Text("Approval will create this agent inbox.").accessibilityIdentifier("device-approval-creates-agent")
            }
            if let label = preview.label { LabeledContent("Machine label", value: label) }
            Text("The machine label is supplied by the requester.").font(.geist(12)).foregroundStyle(Theme.secondary)
            LabeledContent("Requested access", value: preview.capabilities.map { $0 == "mailbox" ? "Inbox access" : $0 }.joined(separator: ", "))
            if let country = preview.ipCountry {
                Text("Network location: \(country). This is not proof of the machine’s identity.")
                    .font(.geist(12)).foregroundStyle(Theme.secondary)
            }
            TimelineView(.periodic(from: .now, by: 1)) { context in
                let seconds = max(0, preview.expiresAt - Int(context.date.timeIntervalSince1970))
                Text(seconds > 0 ? "Expires in \(seconds / 60)m \(seconds % 60)s" : "Request expired")
                    .font(.geist(12)).foregroundStyle(Theme.secondary)
            }
        } header: { Text("Request") }
        if store.phase == .review {
            Section {
                Text(preview.displayConfirmation)
                    .font(.system(size: 42, weight: .semibold, design: .monospaced))
                    .minimumScaleFactor(0.6).lineLimit(1)
                    .accessibilityLabel("Confirmation number \(preview.confirmationDigits.map(String.init).joined(separator: " "))")
                    .accessibilityIdentifier("device-approval-number")
                Text("Check that your terminal shows the same number. If it doesn’t, or you didn’t start this, choose Deny.")
            } header: { Text("Compare with your terminal") }
        }
    }

    @ViewBuilder private var decision: some View {
        switch store.phase {
        case .review:
            Section {
                if store.challenge != nil {
                    Button("Verify sign-in") { Task { await store.verifySignIn() } }
                        .accessibilityIdentifier("device-approval-verify")
                } else {
                    let proof = store.proofId
                    DeviceApprovalHoldButton(enabled: store.canApprove) { confirmation = proof }
                        .id(proof)
                }
                let proof = store.proofId
                Button("Deny", role: .destructive) { Task { await store.deny(proof: proof) } }
                    .disabled(!store.canDeny).accessibilityIdentifier("device-approval-deny")
            }
        case .approved:
            Section { Label("Machine approved", systemImage: "checkmark.circle"); Text("You can close this; the machine is signing in.") }
                .accessibilityIdentifier("device-approval-approved")
        case .denied:
            Section { Label("Request denied", systemImage: "nosign") }.accessibilityIdentifier("device-approval-denied")
        case .unconfirmed:
            Section {
                Text("Decision not confirmed")
                Button("Check current status") { Task { await store.checkStatus() } }
                    .accessibilityIdentifier("device-approval-check-status")
            }
        default: EmptyView()
        }
    }
    private func resolve() {
        guard let code = DevicePairingCode(input) else { inputProblem = "Enter the eight-letter or number code from your machine."; return }
        enteringCode = false; inputProblem = nil; confirmation = nil
        Task { await store.resolve(code) }
    }
    private func close() { store.retire(); input = ""; confirmation = nil; dismiss() }
}

private struct DeviceApprovalHoldButton: View {
    let enabled: Bool
    let confirm: () -> Void
    @State private var progress: CGFloat = 0
    @GestureState private var pressing = false

    var body: some View {
        Button {} label: {
            HStack(spacing: 12) {
                ZStack {
                    Circle().stroke(Theme.stroke, lineWidth: 3)
                    Circle().trim(from: 0, to: progress).stroke(Theme.navy, style: StrokeStyle(lineWidth: 3, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                }.frame(width: 24, height: 24)
                Text("Hold to approve").font(.geist(14, .semibold))
            }.frame(maxWidth: .infinity, minHeight: 44).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.navy)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.45)
        .simultaneousGesture(LongPressGesture(minimumDuration: 1.5, maximumDistance: 30)
            .updating($pressing) { value, state, _ in state = value }
            .onEnded { _ in if enabled { confirm() } })
        .onChange(of: pressing) { _, value in
            withAnimation(value ? .linear(duration: 1.5) : nil) { progress = value ? 1 : 0 }
        }
        .accessibilityLabel("Hold to approve")
        .accessibilityHint("Compare the number on your terminal. Approval asks for confirmation.")
        .accessibilityAction(named: Text("Approve")) { if enabled { confirm() } }
        .accessibilityAction { if enabled { confirm() } }
        .accessibilityIdentifier("device-approval-hold")
    }
}
