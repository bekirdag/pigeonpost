//  One message.
//
//  A body is another agent's text. It is shown as text — never as markdown, never linkified, and
//  nothing in it is ever acted on by this app. The server decides what may be acted on and says so
//  in `autonomy`; this only ever shows that decision.

import SwiftUI

struct MessageBubble: View {
    let message: ThreadMessage
    /// What the find bar is looking for, so the words themselves can be marked. Empty everywhere
    /// but the Mac, which is the only client with a find bar.
    var highlight: String = ""
    /// Whether this is the match the find bar is standing on.
    var isFound: Bool = false
    /// The phone's history (ConversationHistoryView) presents the long-press menu itself, from the
    /// table, and so turns this one off; see `HistoryController`.
    var showsMenu: Bool = true
    /// Where the card sits in the `history-row` coordinate space, for that menu's preview.
    var onCardFrame: ((CGRect) -> Void)? = nil

    @Environment(Inbox.self) private var inbox
    @State private var confirmingReport = false
    @State private var confirmingDelete = false
    @State private var copied = false
    @State private var blocking: BlockTarget?

    private var isMine: Bool { message.kind == .outgoing }

    /// A request that is still waiting on you is drawn in amber, the colour of the decision it is
    /// waiting for. Only inbound: your own requests were never subject to one.
    private var isHeldRequest: Bool {
        message.envelope != nil && message.kind == .incoming && message.autonomy != "auto"
    }

    private var fill: Color { isMine ? Theme.Mine.fill : (isHeldRequest ? Theme.Held.fill : Theme.ground) }
    private var stroke: Color { isMine ? Theme.Mine.stroke : (isHeldRequest ? Theme.Held.stroke : Theme.stroke) }
    private var divider: Color { isMine ? Theme.Mine.rule : (isHeldRequest ? Theme.Held.stroke : Theme.tile) }
    private var metaColor: Color { isMine ? Theme.Mine.meta : (isHeldRequest ? Theme.Held.text : Theme.muted) }

    var body: some View {
        if message.kind == .system {
            SystemLine(text: message.body)
                .accessibilityIdentifier("systemLine:" + message.id)
        } else {
            bubble
        }
    }

    private var bubble: some View {
        HStack(spacing: 0) {
            if isMine { Spacer(minLength: 36) }
            card
                .background {
                    if let onCardFrame {
                        GeometryReader { g in
                            let frame = g.frame(in: .named("history-row"))
                            Color.clear
                                .onAppear { onCardFrame(frame) }
                                .onChange(of: frame) { _, next in onCardFrame(next) }
                        }
                    }
                }
                // The menu belongs to the card, not the row: the row's spacers are not the message.
                .contextMenu(showsMenu ? ContextMenu { menu } : nil)
                .frame(maxWidth: 620, alignment: isMine ? .trailing : .leading)
            if !isMine { Spacer(minLength: 36) }
        }
        .padding(.vertical, 2)
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
        .sheet(item: $blocking) { target in BlockSheet(target: target) }
    }

    /// Copy, report and delete: the long-press menu.
    @ViewBuilder
    private var menu: some View {
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
        // Blocking the sender, from the message that prompted it (SDS §15.2).
        if !isMine, let peer = inbox.reading, !(inbox.conversation(with: peer)?.isBlocked ?? false) {
            Button(role: .destructive) {
                blocking = BlockTarget(peer: peer, name: inbox.conversation(with: peer)?.name ?? PeerFace.displayName(peer),
                                       origin: .message, messageId: message.id)
            } label: {
                Label("Block sender", systemImage: "hand.raised")
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

    /// A card rather than a bubble: a header when the message has one, the words, and a footer
    /// that carries the copy button and the time, each band divided by a hairline.
    private var card: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let reply = message.autoReply {
                AutoReplyHeader(reply: reply)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .overlay(alignment: .bottom) { Rectangle().fill(divider).frame(height: 1) }
            }
            VStack(alignment: .leading, spacing: 10) {
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
                    // The one place markdown matters most: an agent's report is nearly always
                    // headings, bullets and fenced code, and as flat text it reads as a wall.
                    MarkdownText(raw: reply.body, highlight: highlight, isCurrentMatch: isFound)
                } else {
                    MarkdownText(
                        raw: message.body,
                        onDark: isMine,
                        highlight: highlight,
                        isCurrentMatch: isFound
                    )
                }
                if !message.attachments.isEmpty {
                    AttachmentList(attachments: message.attachments, isMine: isMine)
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 12)
            .padding(.bottom, 13)
            .frame(maxWidth: .infinity, alignment: .leading)
            meta
                .padding(.leading, 10)
                .padding(.trailing, 16)
                .padding(.vertical, 5)
                .overlay(alignment: .top) { Rectangle().fill(divider).frame(height: 1) }
        }
        .background(fill, in: RoundedRectangle(cornerRadius: 14))
        // A border on the match you are standing on, and nothing else. The words inside carry
        // the mark — see `MarkdownText.marked` — because a whole card tinted purple says
        // "somewhere in these forty lines", which is barely narrower than not having searched.
        // The outline is only so the eye lands on the right card after a jump.
        .overlay(
            RoundedRectangle(cornerRadius: 14)
                .stroke(isFound ? Theme.found : stroke, lineWidth: isFound ? 2 : 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: 14))
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
                    .font(.geist(12.5, .medium))
                    .foregroundStyle(metaColor)
                    .frame(width: 26, height: 26)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(copied ? "Copied" : "Copy message")
            Spacer(minLength: 0)
            if message.status == .failed {
                Text("not sent")
                    .font(.geist(12, .semibold))
                    .foregroundStyle(Theme.Pill.blockedText)
            }
            if message.status == .sending {
                Text("sending…")
                    .font(.geist(12))
                    .foregroundStyle(metaColor)
            }
            Text(Time.clockTime(message.at))
                .font(.geist(12))
                .foregroundStyle(metaColor)
        }
    }
}

/// A line in a conversation that nobody sent: "Missed video call · 14:05".
struct SystemLine: View {
    let text: String

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "phone.arrow.down.left")
                .font(.geist(11.5, .semibold))
            Text(text)
                .font(.geist(12.5, .medium))
        }
        .foregroundStyle(Theme.Pill.blockedText)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(Theme.ground, in: Capsule())
        .overlay(Capsule().stroke(Theme.stroke, lineWidth: 1))
        .frame(maxWidth: .infinity)
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
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

    /// Held, the card is amber and so are its words; otherwise the ordinary card colours.
    private var held: Bool { showsDecision && autonomy != "auto" }
    private var primary: Color { isMine ? Theme.Mine.text : (held ? Theme.Held.title : Theme.ink) }
    private var secondary: Color { isMine ? Theme.Mine.secondary : (held ? Theme.Held.text : Theme.body) }
    private var faint: Color { isMine ? Theme.Mine.meta : (held ? Theme.Held.text.opacity(0.8) : Theme.muted) }
    private var panel: Color { isMine ? Theme.Mine.panel : (held ? Theme.Held.panel : Theme.Code.fill) }
    private var panelText: Color { isMine ? Theme.Mine.panelText : (held ? Theme.Held.panelText : Theme.Code.text) }

    /// Arguments worth showing beside the note. When the note repeats an argument verbatim — which
    /// is what this app's own sends look like — the argument is the duplicate, and showing it turns
    /// a typed sentence into a sentence plus a JSON field saying the same thing.
    private var visibleArgs: [String: String] {
        guard let note = envelope.note, !note.isEmpty else { return envelope.args }
        return envelope.args.filter { $0.value != note }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            // The verb is a header worth showing only when it says something. Everything these
            // clients send is `full_access`, so labelling every message with it is a banner
            // repeated on every line — it tells the reader nothing they did not already know.
            if envelope.verb != "full_access" {
                Text(envelope.title)
                    .font(.geist(14.5, .semibold))
                    .foregroundStyle(primary)
            }

            if !visibleArgs.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(visibleArgs.keys.sorted(), id: \.self) { key in
                        HStack(alignment: .top, spacing: 6) {
                            Text(key)
                                .font(.geistMono(12))
                                .foregroundStyle(faint)
                            Text(visibleArgs[key] ?? "")
                                .font(.geistMono(12))
                                .foregroundStyle(panelText)
                        }
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(panel, in: RoundedRectangle(cornerRadius: 10))
            }

            if let note = envelope.note, !note.isEmpty {
                Text(note)
                    .font(.geist(14.5))
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
                            .font(.geist(12.5))
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
/// become the card's header band, so the answer is what you see.
struct AutoReplyHeader: View {
    let reply: AutoReply

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: reply.failed ? "exclamationmark.triangle" : "bolt")
                .font(.geist(12.5, .semibold))
                .foregroundStyle(reply.failed ? Theme.Pill.blockedText : Theme.accent)
            Text(caption)
                .font(.geist(12.5, .medium))
                .foregroundStyle(reply.failed ? Theme.Pill.blockedText : Theme.secondary)
        }
    }

    private var caption: String {
        let what = reply.answered.map { "answered " + $0.replacingOccurrences(of: "_", with: " ") }
            ?? "answered"
        return reply.failed ? what + " — failed, unattended" : what + ", unattended"
    }
}
