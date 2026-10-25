import SwiftUI

struct MessageActionButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .padding(.horizontal, 3)
            #if os(iOS)
            .frame(minHeight: 44)
            #else
            .frame(minHeight: 28)
            #endif
            .contentShape(Rectangle())
            .opacity(configuration.isPressed ? 0.6 : 1)
    }
}

struct MessageAgentMark: View {
    let state: String
    @Environment(\.accessibilityReduceMotion) private var reducedMotion
    @State private var turning = false

    var body: some View {
        HStack(spacing: 4) {
            if state == "working" {
                Image(systemName: "gearshape")
                    .rotationEffect(.degrees(turning && !reducedMotion ? 360 : 0))
                    .animation(reducedMotion ? nil : .linear(duration: 4).repeatForever(autoreverses: false), value: turning)
                    .onAppear { turning = true }
                if reducedMotion { Text("Working…") }
            } else if state == "failed" {
                Image(systemName: "exclamationmark.triangle")
            }
        }
        .font(.geist(11, .semibold))
        .foregroundStyle(state == "working" ? Theme.green : Theme.Pill.blockedText)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(state == "working" ? "Agent working on this" : "Agent run failed")
    }
}

/// Receipt marks describe server-confirmed state; an absent/unknown state makes no claim.
struct MessageReceiptMark: View {
    let receipt: MessageReceipt

    var body: some View {
        if let state = receipt.state, ["sent", "delivered", "seen"].contains(state) {
            HStack(spacing: -5) {
                Image(systemName: "checkmark")
                if state != "sent" { Image(systemName: "checkmark") }
            }
            .font(.geist(11, .semibold))
            .foregroundStyle(state == "seen" ? Theme.green : Theme.secondary)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(state == "seen" ? receipt.seenAt.map { "Seen " + Time.clockTime($0) } ?? "Seen" : state.capitalized)
        }
    }
}

struct MessageActionBanner: View {
    let action: MessageComposerAction
    let busy: Bool
    let cancel: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: action.kind == .edit ? "pencil" : "arrowshape.turn.up.left")
            VStack(alignment: .leading, spacing: 3) {
                Text(action.kind == .edit ? "Editing message" : "Replying to message").font(.geist(12, .semibold))
                Text(action.message.replyQuote).font(.geist(12)).lineLimit(2)
            }
            Spacer(minLength: 0)
            Button(action: cancel) { Image(systemName: "xmark").frame(width: 44, height: 44) }
                .buttonStyle(.plain)
                .accessibilityLabel(action.kind == .edit ? "Cancel edit" : "Cancel reply")
                .accessibilityIdentifier("cancel-message-action")
                .disabled(busy)
                .keyboardShortcut(.cancelAction)
        }
        .foregroundStyle(Theme.secondary)
        .padding(.leading, 12)
        .background(Theme.chip, in: RoundedRectangle(cornerRadius: 10))
    }
}

struct MessageInfoSheet: View {
    let message: ThreadMessage
    let scope: MessageActionScope
    var close: (() -> Void)? = nil
    @Environment(Inbox.self) private var inbox
    @Environment(Account.self) private var account
    @Environment(\.dismiss) private var dismiss
    @State private var information: MessageInformation?
    @State private var loading = true
    @State private var failed = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if loading { ProgressView("Loading message info") }
                    else if failed {
                        Text("Could not load this message's information.").foregroundStyle(Theme.secondary)
                        Button("Try again") { Task { await load() } }
                    } else if let information {
                        if account.features.receipts && message.kind == .outgoing {
                            LabeledContent("Delivered", value: date(information.receipt?.deliveredAt))
                            LabeledContent("Seen", value: date(information.receipt?.seenAt))
                        }
                        if account.features.agentStatus, let state = information.agentStatus?.state {
                            LabeledContent("Agent", value: state.capitalized)
                        }
                        if account.features.messageEdit {
                            ForEach(Array(information.versions.enumerated()), id: \.element.id) { index, version in
                                VStack(alignment: .leading, spacing: 6) {
                                    Text(index == 0 ? "Original" : "Edit \(index)").font(.geist(13, .semibold))
                                    Text(date(version.at)).font(.geist(12)).foregroundStyle(Theme.secondary)
                                    Text(ThreadMessage(id: version.id, kind: message.kind, at: version.at, body: version.body, threadId: nil).copyText)
                                        .font(.geist(14)).textSelection(.enabled)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(12)
                                .background(Theme.ground, in: RoundedRectangle(cornerRadius: 10))
                            }
                        }
                    }
                }
                .padding(20)
                .frame(maxWidth: 640, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
            .background(Theme.wash)
            .navigationTitle("Message info")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { finish() } }
            }
        }
        .frame(minWidth: 300, minHeight: 320)
        .task { await load() }
        .onChange(of: account.me?.address) { _, _ in finish() }
    }

    private func finish() { if let close { close() } else { dismiss() } }
    private func date(_ at: Int?) -> String {
        at.map { Date(timeIntervalSince1970: TimeInterval($0)).formatted(date: .abbreviated, time: .shortened) } ?? "—"
    }
    private func load() async {
        loading = true
        failed = false
        do { information = try await inbox.messageInformation(message, in: scope) }
        catch is CancellationError { finish(); return }
        catch { failed = true }
        loading = false
    }
}
