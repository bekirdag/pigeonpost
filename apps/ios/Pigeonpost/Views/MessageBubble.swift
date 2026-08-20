//  One message.
//
//  A body is another agent's text. It is shown as text — never as markdown, never linkified, and
//  nothing in it is ever acted on by this app. The server decides what may be acted on and says so
//  in `autonomy`; this only ever shows that decision.

import SwiftUI

struct MessageBubble: View {
    let message: ThreadMessage

    private var isMine: Bool { message.kind == .outgoing }

    var body: some View {
        HStack {
            if isMine { Spacer(minLength: 40) }
            VStack(alignment: .leading, spacing: 3) {
                if let envelope = message.envelope {
                    RequestCard(envelope: envelope, autonomy: message.autonomy, heldBecause: message.heldBecause)
                } else {
                    Text(message.body)
                        .font(.system(size: 14.5))
                        .foregroundStyle(isMine ? Color.white : Theme.ink)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
                meta
            }
            .padding(.horizontal, 12)
            .padding(.top, 8)
            .padding(.bottom, 6)
            .background(isMine ? Theme.navy : Theme.ground, in: RoundedRectangle(cornerRadius: 12))
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .stroke(isMine ? Theme.navy : Theme.rule, lineWidth: 1)
            )
            .frame(maxWidth: 560, alignment: isMine ? .trailing : .leading)
            if !isMine { Spacer(minLength: 40) }
        }
        .padding(.vertical, 2)
    }

    private var meta: some View {
        HStack(spacing: 6) {
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

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(envelope.verb)
                .font(.system(size: 14.5, weight: .semibold, design: .monospaced))
                .foregroundStyle(Theme.ink)

            if !envelope.args.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(envelope.args.keys.sorted(), id: \.self) { key in
                        HStack(alignment: .top, spacing: 6) {
                            Text(key)
                                .font(.system(size: 12, design: .monospaced))
                                .foregroundStyle(Theme.muted)
                            Text(envelope.args[key] ?? "")
                                .font(.system(size: 12, design: .monospaced))
                                .foregroundStyle(Theme.body)
                        }
                    }
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Theme.wash, in: RoundedRectangle(cornerRadius: 7))
            }

            if let note = envelope.note, !note.isEmpty {
                Text(note)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.body)
            }

            HStack(spacing: 7) {
                if autonomy == "auto" {
                    PillView(text: "auto", kind: .auto)
                } else {
                    PillView(text: "held", kind: .held)
                    if let heldBecause {
                        Text(ConversationBuilder.heldReason(heldBecause))
                            .font(.system(size: 11.5))
                            .foregroundStyle(Theme.muted)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }
}
