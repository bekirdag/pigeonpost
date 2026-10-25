//  The one question every block asks (SDS §15.2), and the banner a blocked conversation carries.
//
//  Presented from the thread header, the sender panel, a message's menu, the call log, the Filtered
//  list and a group's member sheet — each builds a `BlockTarget` and shows this, so the words and
//  the choices are the same wherever somebody decides it.

import SwiftUI

struct BlockSheet: View {
    let target: BlockTarget
    /// Told after the block is saved (and the report, if asked for, sent).
    var onBlocked: () -> Void = {}

    @Environment(Inbox.self) private var inbox
    @Environment(FilteredMail.self) private var filtered: FilteredMail?
    @Environment(\.dismiss) private var dismiss

    @State private var report = false
    @State private var wide = false
    @State private var working = false
    @State private var error: String?

    private var wildcard: String? { Blocking.wildcard(for: target.peer) }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("They won’t be able to message or call you, and won’t be told.")
                        .font(.geist(14.5))
                        .foregroundStyle(Theme.body)
                    Toggle("Also report as spam", isOn: $report)
                        .accessibilityIdentifier("blockReport")
                    if let wildcard {
                        Toggle("Block everyone at \(String(wildcard.dropLast(2)))", isOn: $wide)
                            .accessibilityIdentifier("blockWide")
                    }
                } footer: {
                    Text(wide
                         ? "Everyone whose address starts with \(String((wildcard ?? "").dropLast(2))) is refused, including names you have not seen yet."
                         : "You can unblock them later in Settings → Blocked.")
                }
                if let error {
                    Text(error)
                        .font(.geist(13))
                        .foregroundStyle(Theme.Pill.blockedText)
                }
            }
            .sheetSurface()
            .navigationTitle("Block \(target.name)?")
            .inlineTitle()
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                        .accessibilityIdentifier("blockCancel")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(working ? "Blocking…" : "Block", role: .destructive) { block() }
                        .disabled(working)
                        .accessibilityIdentifier("blockConfirm")
                }
            }
        }
        .accessibilityIdentifier("blockSheet")
        .onAppear { report = target.reportByDefault }
        .phoneDetents([.medium, .large])
    }

    private func block() {
        working = true
        error = nil
        let peer = wide ? (wildcard ?? target.peer) : target.peer
        Task {
            if let failure = await Blocking.block(peer, inbox: inbox) {
                error = failure
                working = false
                return
            }
            var said = "Blocked. They won’t be able to message or call you."
            if report {
                said = await Blocking.report(target, inbox: inbox, filtered: filtered?.messages ?? [])
            }
            filtered?.drop(peer: target.peer)
            inbox.toast = said
            working = false
            onBlocked()
            dismiss()
        }
    }
}

/// Over the composer of a conversation with somebody blocked: why there is no composer, and the way
/// back. The postbox would refuse nothing that is sent *to* them, but writing to somebody you have
/// told the postbox to refuse is almost always a slip, and their answer could never arrive.
struct BlockedBanner: View {
    let conversation: Conversation
    @Environment(Inbox.self) private var inbox
    @State private var working = false

    /// The rule doing the blocking: their own row, or their namespace's.
    private var rule: String { conversation.contact?.peer ?? conversation.peer }

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "hand.raised")
                .font(.geist(15, .semibold))
                .foregroundStyle(Theme.Pill.blockedText)
            VStack(alignment: .leading, spacing: 2) {
                Text(rule.hasSuffix("/*") ? "Everyone at \(String(rule.dropLast(2))) is blocked" : "You blocked \(conversation.name)")
                    .font(.geist(14, .semibold))
                    .foregroundStyle(Theme.ink)
                Text("They can’t message or call you.")
                    .font(.geist(12.5))
                    .foregroundStyle(Theme.muted)
            }
            Spacer(minLength: 8)
            Button(working ? "Unblocking…" : "Unblock") {
                working = true
                Task {
                    let done = await Blocking.unblock(rule, inbox: inbox)
                    inbox.toast = done ? "Unblocked." : "Could not unblock. Please try again."
                    working = false
                }
            }
            .font(.geist(14, .semibold))
            .disabled(working)
            .accessibilityIdentifier("unblockBanner")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity)
        .background(Theme.wash)
        .overlay(alignment: .top) { Rectangle().fill(Theme.rule).frame(height: 1) }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("blockedBanner")
    }
}
