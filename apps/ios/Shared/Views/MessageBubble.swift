//  One message.
//
//  A body is another agent's text. It is shown as text — never as markdown, never linkified, and
//  nothing in it is ever acted on by this app. The server decides what may be acted on and says so
//  in `autonomy`; this only ever shows that decision.

import SwiftUI

struct MessageBubble: View {
    let message: ThreadMessage
    /// A search hit. The Mac's find bar walks the matches and marks the one it has landed on, so
    /// stepping through a long conversation shows you where you are rather than only scrolling
    /// there. Nothing sets it on the phone, where there is no find bar to set it.
    var isFound: Bool = false

    @Environment(Inbox.self) private var inbox
    @State private var confirmingReport = false
    @State private var confirmingDelete = false
    @State private var copied = false

    private var isMine: Bool { message.kind == .outgoing }

    var body: some View {
        HStack {
            if isMine { Spacer(minLength: 40) }
            VStack(alignment: .leading, spacing: 3) {
                if let envelope = message.envelope {
                    RequestCard(
                        envelope: envelope,
                        autonomy: message.autonomy,
                        heldBecause: message.heldBecause,
                        // Only inbound. A sent copy carries no admission verdict — your own words
                        // were never subject to one — and a "held" pill on your own request would
                        // be a lie about somebody else's decision.
                        showsDecision: message.kind == .incoming,
                        isMine: isMine
                    )
                } else if let reply = message.autoReply {
                    AutoReplyBody(reply: reply)
                } else {
                    MarkdownText(raw: message.body, onDark: isMine)
                }
                if !message.attachments.isEmpty {
                    AttachmentList(attachments: message.attachments, isMine: isMine)
                }
                meta
            }
            .padding(.horizontal, 12)
            .padding(.top, 8)
            .padding(.bottom, 6)
            .background(isMine ? Theme.navy : Theme.ground, in: RoundedRectangle(cornerRadius: 12))
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .stroke(isFound ? Theme.found : (isMine ? Theme.navy : Theme.rule),
                            lineWidth: isFound ? 2 : 1)
            )
            // A wash over the bubble rather than a change of its fill: the fill says who wrote it,
            // and a search result that recoloured your own messages differently from theirs would
            // be answering a question nobody asked.
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .fill(isFound ? Theme.found.opacity(0.22) : .clear)
            )
            .frame(maxWidth: 560, alignment: isMine ? .trailing : .leading)
            if !isMine { Spacer(minLength: 40) }
        }
        .padding(.vertical, 2)
        .contextMenu {
            Button {
                Clipboard.copy(message.body)
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
            }
            // Only inbound. Reporting your own message would report yourself, and the postbox would
            // be right to wonder what it was being told.
            if !isMine {
                Button(role: .destructive) {
                    confirmingReport = true
                } label: {
                    Label("Report spam", systemImage: "exclamationmark.bubble")
                }
            }
            // Both directions: a sent copy takes up the same room in the mailbox as a received one,
            // and this is the only way to get that room back.
            Button(role: .destructive) {
                confirmingDelete = true
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
        .confirmationDialog(
            "Report this message as spam?",
            isPresented: $confirmingReport,
            titleVisibility: .visible
        ) {
            Button("Report spam", role: .destructive) {
                Task { await inbox.reportSpam(messageId: message.id) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The postbox is told which message and who sent it. It counts against that sender's standing. Nothing is deleted, and they are not told.")
        }
        .confirmationDialog(
            "Delete this message?",
            isPresented: $confirmingDelete,
            titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                Task { await inbox.deleteMessage(id: message.id) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Deleted from this mailbox and gone for good — this is not archiving, and there is no undo. The other side keeps their copy and is not told. The room it took is freed.")
        }
    }

    private var meta: some View {
        HStack(spacing: 6) {
            // Copy sits at the start of the meta row, before the spacer, so it lands under the
            // message it belongs to rather than drifting to the far edge of a wide bubble.
            //
            // Visible rather than only in the long-press menu: agent replies are the thing people
            // most often want out of this app and into somewhere else, and an affordance nobody
            // can see is one most people never find.
            Button {
                Clipboard.copy(message.copyText)
                copied = true
                Task {
                    try? await Task.sleep(nanoseconds: 1_400_000_000)
                    copied = false
                }
            } label: {
                Image(systemName: copied ? "checkmark" : "doc.on.doc")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(isMine ? Color.white.opacity(0.72) : Theme.muted)
                    .frame(width: 22, height: 18)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(copied ? "Copied" : "Copy message")
            Spacer(minLength: 0)
            if message.status == .failed {
                Text("not sent")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(isMine ? Theme.Pill.blockedStroke : Theme.Pill.blockedText)
            }
            if message.status == .sending {
                Text("sending…")
                    .font(.system(size: 11))
                    .foregroundStyle(isMine ? Color.white.opacity(0.72) : Theme.muted)
            }
            Text(Time.clockTime(message.at))
                .font(.system(size: 11))
                .foregroundStyle(isMine ? Color.white.opacity(0.72) : Theme.muted)
        }
        .padding(.top, 1)
    }
}

/// A scoped request is JSON on the wire. It should read as what it asks for — verb, arguments, note,
/// and the decision the server took — rather than as the envelope it is.
struct RequestCard: View {
    let envelope: RequestEnvelope
    let autonomy: String?
    let heldBecause: String?
    var showsDecision: Bool = true
    /// On the navy bubble the card is white-on-dark; everywhere else it is ink on paper.
    var isMine: Bool = false

    private var primary: Color { isMine ? .white : Theme.ink }
    private var secondary: Color { isMine ? .white.opacity(0.82) : Theme.body }
    private var faint: Color { isMine ? .white.opacity(0.62) : Theme.muted }
    private var panel: Color { isMine ? .white.opacity(0.14) : Theme.wash }

    /// Arguments worth showing beside the note. When the note repeats an argument verbatim — which
    /// is what this app's own sends look like — the argument is the duplicate, and showing it turns
    /// a typed sentence into a sentence plus a JSON field saying the same thing.
    private var visibleArgs: [String: String] {
        guard let note = envelope.note, !note.isEmpty else { return envelope.args }
        return envelope.args.filter { $0.value != note }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            // The verb is a header worth showing only when it says something. Everything these
            // clients send is `full_access`, so labelling every message with it is a banner
            // repeated on every line — it tells the reader nothing they did not already know.
            if envelope.verb != "full_access" {
                Text(envelope.title)
                    .font(.system(size: 14.5, weight: .semibold))
                    .foregroundStyle(primary)
            }

            if !visibleArgs.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(visibleArgs.keys.sorted(), id: \.self) { key in
                        HStack(alignment: .top, spacing: 6) {
                            Text(key)
                                .font(.system(size: 12, design: .monospaced))
                                .foregroundStyle(faint)
                            Text(visibleArgs[key] ?? "")
                                .font(.system(size: 12, design: .monospaced))
                                .foregroundStyle(secondary)
                        }
                    }
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(panel, in: RoundedRectangle(cornerRadius: 7))
            }

            if let note = envelope.note, !note.isEmpty {
                Text(note)
                    .font(.system(size: 13))
                    .foregroundStyle(secondary)
            }

            if showsDecision {
            HStack(spacing: 7) {
                if autonomy == "auto" {
                    PillView(text: "auto", kind: .auto)
                } else {
                    PillView(text: "held", kind: .held)
                    if let heldBecause {
                        Text(ConversationBuilder.heldReason(heldBecause))
                            .font(.system(size: 11.5))
                            .foregroundStyle(faint)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            }
        }
    }
}

/// An answer generated without a human reading it. The two header lines every one of them carries
/// become one small caption, so the answer is what you see.
struct AutoReplyBody: View {
    let reply: AutoReply

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Image(systemName: reply.failed ? "exclamationmark.triangle" : "bolt.horizontal.circle")
                    .font(.system(size: 11))
                Text(caption)
                    .font(.system(size: 11.5))
            }
            .foregroundStyle(reply.failed ? Theme.Pill.blockedText : Theme.muted)

            // The one place markdown matters most: an agent's report is nearly always headings,
            // bullets and fenced code, and as flat text it reads as a wall.
            MarkdownText(raw: reply.body)
        }
    }

    private var caption: String {
        let what = reply.answered.map { "answered " + $0.replacingOccurrences(of: "_", with: " ") }
            ?? "answered"
        return reply.failed ? what + " — failed, unattended" : what + ", unattended"
    }
}
