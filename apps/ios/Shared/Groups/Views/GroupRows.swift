//  Groups in the conversation list: a group's row, and the invitations waiting above them.
//
//  The same row shapes as a conversation's (`ConversationRow`, `DesktopConversationRow`), so a
//  group sits among people without looking like a different app; what marks it as a group is its
//  face — two letters of its name on a square-ish tile — and the timer and mute marks the web shows.

import SwiftUI

/// A group's face: its initials on one of five tones, as `groupAvatar` draws it on the web.
struct GroupFace: View {
    let id: String
    let name: String
    var size: CGFloat = 38

    var body: some View {
        let tone = GroupText.tone(id)
        RoundedRectangle(cornerRadius: size * 0.3)
            .fill(Theme.tones[(tone - 1) % Theme.tones.count])
            .frame(width: size, height: size)
            .overlay(
                Text(GroupText.initials(name))
                    .font(.geist(size * 0.34, .bold))
                    .foregroundStyle(Theme.toneInk[(tone - 1) % Theme.toneInk.count])
            )
            .accessibilityHidden(true)
    }
}

/// Selection values for groups share the conversation list's selection with peers. A group id
/// (`grp_…`) can never be a peer address, which always starts with `/`.
enum GroupSelection {
    static func isGroup(_ selection: String?) -> Bool { selection?.hasPrefix("grp_") == true }
}

struct GroupRow: View {
    let group: GroupSummary
    var isSelected = false
    var compact = false

    @Environment(GroupStore.self) private var groups

    var body: some View {
        HStack(alignment: .center, spacing: compact ? 11 : 12) {
            GroupFace(id: group.id, name: group.name, size: compact ? 38 : 40)
            VStack(alignment: .leading, spacing: compact ? 2 : 3) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(group.name)
                        .font(.geist(compact ? 13.5 : 14.5, .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if let at = group.lastAt, at > 0 {
                        Text(Time.listTime(at))
                            .font(.geist(11.5))
                            .foregroundStyle(Theme.muted)
                            .fixedSize()
                    }
                }
                HStack(alignment: .center, spacing: 6) {
                    Text(groups.preview(group))
                        .font(.geist(compact ? 13 : 13.5))
                        .foregroundStyle(Theme.secondary)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if group.ephemeralSecs != nil {
                        Image(systemName: "timer")
                            .font(.geist(11.5))
                            .foregroundStyle(Theme.muted)
                            .accessibilityLabel("Disappearing messages: \(GroupTimers.label(group.ephemeralSecs))")
                    }
                    if group.muted {
                        Image(systemName: "bell.slash")
                            .font(.geist(11.5))
                            .foregroundStyle(Theme.muted)
                            .accessibilityLabel("Muted")
                    }
                    if group.unread > 0, groups.open != group.id {
                        UnreadBadge(count: group.unread, inverted: isSelected)
                    }
                }
            }
        }
        .padding(.vertical, compact ? 0 : 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("group:" + group.name)
    }
}

/// "Invitations (2)", folding open to one card per invitation with Join and Decline.
struct GroupInvitations: View {
    @Environment(GroupStore.self) private var groups

    var body: some View {
        @Bindable var groups = groups
        let invited = groups.invitations
        if !invited.isEmpty {
            Button {
                withAnimation(.easeOut(duration: 0.15)) { groups.invitationsOpen.toggle() }
            } label: {
                HStack(spacing: 12) {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(Theme.tones[1])
                        .frame(width: 38, height: 38)
                        .overlay(Image(systemName: "person.3").font(.geist(14, .semibold)).foregroundStyle(Theme.toneInk[1]))
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Invitations (\(invited.count))")
                            .font(.geist(14, .semibold))
                            .foregroundStyle(Theme.ink)
                        Text(groups.invitationsOpen ? "Choose to join or decline" : "Group invitations waiting for you")
                            .font(.geist(13))
                            .foregroundStyle(Theme.secondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.down")
                        .font(.geist(12, .semibold))
                        .foregroundStyle(Theme.secondary)
                        .rotationEffect(.degrees(groups.invitationsOpen ? 180 : 0))
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("groupInvitations")
            .accessibilityValue(groups.invitationsOpen ? "expanded" : "collapsed")
            if groups.invitationsOpen {
                ForEach(invited) { group in
                    InvitationCard(group: group)
                }
            }
        }
    }
}

struct InvitationCard: View {
    let group: GroupSummary
    @Environment(GroupStore.self) private var groups
    @State private var working = false

    private var detail: String {
        var bits: [String] = []
        if let by = group.my?.invitedBy { bits.append("From " + groups.name(by)) }
        // The invitation came from somebody this mailbox does not know: said, not hidden.
        if group.my?.filtered == true { bits.append("someone you don't know") }
        bits.append("\(group.members) members")
        return bits.joined(separator: " · ")
    }

    var body: some View {
        HStack(spacing: 10) {
            GroupFace(id: group.id, name: group.name, size: 34)
            VStack(alignment: .leading, spacing: 2) {
                Text(group.name).font(.geist(13.5, .semibold)).foregroundStyle(Theme.ink).lineLimit(1)
                Text(detail).font(.geist(12)).foregroundStyle(Theme.secondary).lineLimit(2)
            }
            Spacer(minLength: 4)
            Button("Decline") { answer(false) }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityLabel("Decline \(group.name)")
            Button("Join") { answer(true) }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accent)
                .controlSize(.small)
                .accessibilityLabel("Join \(group.name)")
        }
        .disabled(working)
        .padding(10)
        .background(Theme.ground, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.stroke, lineWidth: 1))
    }

    private func answer(_ accept: Bool) {
        working = true
        Task {
            await groups.answer(group, accept: accept)
            working = false
        }
    }
}

/// One row of the conversation list: a person (or agent) or a group, newest activity first.
enum InboxEntry: Identifiable {
    case conversation(Conversation)
    case group(GroupSummary)

    var id: String {
        switch self {
        case let .conversation(conversation): return conversation.peer
        case let .group(group): return group.id
        }
    }

    var last: Int {
        switch self {
        case let .conversation(conversation): return conversation.last
        case let .group(group): return group.lastAt ?? group.createdAt ?? 0
        }
    }

    /// Groups join the list by their last activity; conversations keep the order they came in.
    static func merge(_ conversations: [Conversation], _ groups: [GroupSummary]) -> [InboxEntry] {
        guard !groups.isEmpty else { return conversations.map { .conversation($0) } }
        var out: [InboxEntry] = []
        var pending = groups.sorted { ($0.lastAt ?? 0) > ($1.lastAt ?? 0) }[...]
        for conversation in conversations {
            while let next = pending.first, (next.lastAt ?? next.createdAt ?? 0) >= conversation.last {
                out.append(.group(next))
                pending = pending.dropFirst()
            }
            out.append(.conversation(conversation))
        }
        out.append(contentsOf: pending.map { .group($0) })
        return out
    }
}
