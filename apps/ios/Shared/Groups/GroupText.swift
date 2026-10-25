//  What a group's history says, in words. Pure functions over the history, so the hand-run tests
//  can check every sentence against the web client's (site-inbox/groups.js `noticeText`,
//  `commitLines` and the warning table) without a screen or a network.

import Foundation

/// One line of a group conversation, ready to draw.
struct GroupLine: Identifiable, Equatable {
    enum Kind: Equatable {
        case day
        case notice
        /// A verified membership change; `security` marks a new device on an existing member — the
        /// visible defence against a postbox minting devices (P-2 §9, §12).
        case commit(security: Bool)
        case warning
        case message
    }

    let id: String
    let kind: Kind
    var text: String = ""
    var fingerprint: String?
    var item: GroupItem?
    /// Whether to draw the sender's name over this message (first in a run from that sender).
    var showSender = false
}

enum GroupText {
    /// `names` turns an address into what a person recognises; it answers "You" for this mailbox.
    static func notice(_ item: GroupItem, names: (String) -> String) -> String? {
        let meta = item.metadata ?? NoticeMeta()
        let actor = item.actor.map(names) ?? "Someone"
        let who = meta.address.map(names) ?? "someone"
        switch item.notice {
        case "group_created": return "\(actor) created the group"
        case "member_invited": return "\(actor) invited \(who)"
        case "member_left": return "\(who) left"
        case "member_removed": return "\(actor) removed \(who)"
        case "member_banned": return "\(actor) removed and banned \(who)"
        case "role_changed":
            switch meta.role {
            case "admin": return "\(actor) made \(who) an admin"
            case "restricted": return "\(actor) restricted \(who)"
            default: return "\(who) is now a member"
            }
        case "settings_changed":
            let changed = meta.changed ?? []
            if changed.contains("post_policy") {
                return meta.postPolicy == "admins" ? "\(actor) set posting to admins only" : "\(actor) let everyone post"
            }
            if changed.contains("name") { return "\(actor) renamed the group to “\(meta.name ?? "")”" }
            if changed.contains("join_policy") {
                return meta.joinPolicy == "public" ? "\(actor) made the group public" : "\(actor) made the group invite only"
            }
            return "\(actor) changed the group settings"
        case "timer_changed":
            if let secs = meta.ephemeralSecs, secs > 0 {
                return "\(actor) set messages to disappear after \(GroupTimers.label(secs))"
            }
            return "\(actor) turned off disappearing messages"
        case "ownership_transferred": return "\(actor) made \(who) the owner"
        case "call_started": return "\(actor) started a \(meta.kind == "video" ? "video" : "voice") call"
        case "call_ended":
            if let duration = meta.duration, duration > 0 { return "Call ended · \(Int((Double(duration) / 60).rounded())) min" }
            return "Call ended"
        case "link_reset": return "\(actor) reset the invite link"
        case "you_joined": return "You joined this group. Earlier messages are not available on this device."
        case "device_removed": return "\(who)'s device was removed"
        // Shown from the verified commit instead; the server's word is only a cross-check.
        case "member_joined", "device_added": return nil
        default: return nil
        }
    }

    static func warning(_ code: String?) -> String {
        switch code {
        case "membership_mismatch":
            return "Security warning: a membership change did not match the postbox's record. Compare safety numbers in group info."
        case "commit_rejected": return "Security warning: this device refused an invalid group change."
        case "sender_mismatch": return "Security warning: a message's sender did not match its device."
        case "action_mismatch": return "Security warning: a message's action did not match its encrypted contents."
        case "own_message_mismatch": return "Security warning: the postbox's copy did not match the encrypted message saved on this device."
        case "resynced": return "This device lost the group's keys and is being added again. Earlier messages stay unreadable here."
        default: return "Security warning."
        }
    }

    /// The lines a verified commit produces: who really joined, and which existing members added a
    /// new device (with that device's fingerprint). `me` and `myDevice` are this device, which is
    /// never announced to itself.
    static func commitLines(_ list: [GroupItem], at commit: GroupItem, me: String, myDevice: String?,
                            names: (String) -> String) -> [(text: String, fingerprint: String?, security: Bool)] {
        guard let committer = commit.committer else { return [] }
        var knownBefore: Set<String> = [me]
        for earlier in list {
            if earlier == commit { break }
            if earlier.kind == .commit {
                if let by = earlier.committer?.mailbox { knownBefore.insert(by) }
                for device in earlier.added ?? [] { knownBefore.insert(device.mailbox) }
            }
            if earlier.kind == .message, let sender = earlier.sender { knownBefore.insert(sender) }
        }
        var order: [String] = []
        var byMailbox: [String: [GroupDeviceInfo]] = [:]
        for device in commit.added ?? [] {
            if byMailbox[device.mailbox] == nil { order.append(device.mailbox) }
            byMailbox[device.mailbox, default: []].append(device)
        }
        var out: [(String, String?, Bool)] = []
        for mailbox in order {
            let devices = byMailbox[mailbox] ?? []
            if knownBefore.contains(mailbox) || mailbox == committer.mailbox {
                for device in devices where !(mailbox == me && device.deviceId == myDevice) {
                    out.append(("\(names(mailbox)) added a new device", device.fingerprint, true))
                }
            } else {
                out.append(("\(names(mailbox)) joined", nil, false))
            }
        }
        return out
    }

    /// The conversation as lines: day separators, notices, verified membership changes, warnings and
    /// messages, with expired items dropped by this device's clock before anything is drawn.
    static func lines(_ history: [GroupItem], me: String, myDevice: String?, now: Int,
                      names: (String) -> String, digits: (String) -> String) -> [GroupLine] {
        let list = GroupActions.visible(history, now: now)
        var out: [GroupLine] = []
        var lastDay: String?
        var lastSender: String?
        let calendar = Calendar.current
        let dayFormat = DateFormatter()
        dayFormat.setLocalizedDateFormatFromTemplate("EEEEdMMMM")
        for item in list {
            if let at = item.createdAt, at > 0 {
                let date = Date(timeIntervalSince1970: TimeInterval(at))
                let comps = calendar.dateComponents([.year, .month, .day], from: date)
                let day = "\(comps.year ?? 0)-\(comps.month ?? 0)-\(comps.day ?? 0)"
                if day != lastDay {
                    lastDay = day
                    out.append(GroupLine(id: "day:" + day, kind: .day, text: dayFormat.string(from: date)))
                    lastSender = nil
                }
            }
            switch item.kind {
            case .notice:
                if let text = notice(item, names: names) {
                    out.append(GroupLine(id: item.id, kind: .notice, text: text, item: item))
                    lastSender = nil
                }
            case .commit:
                for (index, line) in commitLines(list, at: item, me: me, myDevice: myDevice, names: names).enumerated() {
                    out.append(GroupLine(id: item.id + ":\(index)", kind: .commit(security: line.security), text: line.text,
                                         fingerprint: line.fingerprint.map(digits), item: item))
                }
                lastSender = nil
            case .warning:
                out.append(GroupLine(id: item.id, kind: .warning, text: warning(item.warning), item: item))
                lastSender = nil
            case .message:
                let mine = item.sender == me
                out.append(GroupLine(id: item.id, kind: .message, item: item,
                                     showSender: !mine && item.sender != lastSender))
                lastSender = item.sender
            }
        }
        return out
    }

    /// The list row's second line: the newest readable message, said the web's way.
    static func preview(_ history: [GroupItem], me: String, now: Int = Int(Date().timeIntervalSince1970),
                        names: (String) -> String) -> String? {
        let visible = GroupActions.visible(history, now: now)
        for item in visible.reversed() where item.kind == .message {
            if item.deleted == true { return "Message deleted" }
            if item.undecryptable == true { return "Encrypted message" }
            let who = item.sender == me ? "You" : (names(item.sender ?? "").split(whereSeparator: { $0 == " " || $0 == "/" }).first.map(String.init) ?? "Someone")
            let what = (item.text?.isEmpty == false ? item.text : nil) ?? item.files?.first.map { "📎 " + $0.name } ?? ""
            return who + ": " + what
        }
        return nil
    }

    /// The group's face: two letters from its name and a tone from its id, as the web draws it.
    static func initials(_ name: String) -> String {
        let parts = name.split(whereSeparator: \.isWhitespace)
        let first = parts.first.flatMap { $0.first.map(String.init) } ?? ""
        let second: String
        if parts.count > 1 { second = parts[1].first.map(String.init) ?? "" }
        else { second = parts.first.flatMap { $0.dropFirst().first.map(String.init) } ?? "" }
        let letters = first + second
        return letters.isEmpty ? "··" : letters.uppercased()
    }

    /// 1…5, like the web's group tone.
    static func tone(_ id: String) -> Int {
        var hash: UInt32 = 0
        for unit in id.unicodeScalars { hash = hash &* 31 &+ UInt32(truncatingIfNeeded: unit.value) }
        return Int(hash % 5) + 1
    }

    /// A safety number in groups of five, so two people can read it to each other.
    static func spaced(_ digits: String) -> String {
        var out = ""
        for (index, character) in digits.enumerated() {
            if index > 0, index % 5 == 0 { out.append(" ") }
            out.append(character)
        }
        return out
    }

    static func explain(_ error: Error) -> String {
        guard let api = error as? APIError else { return "Something went wrong." }
        switch api.code {
        case "not_admin": return "Only admins can do that."
        case "owner_only": return "Only the owner can do that."
        case "human_required": return "This needs you to be signed in yourself, not an agent key."
        case "posting_restricted": return "Only admins can send messages here."
        case "banned": return "That mailbox is banned from this group."
        case "group_full": return "The group is full."
        case "invalid_link": return "This invite link is no longer valid."
        case "invite_required": return "This group is invite only."
        case "already_member": return "Already a member."
        case "group_deleted": return "This group was deleted."
        case "revision_conflict": return "Someone else changed this at the same time. Try again."
        case "not_member": return "You are not a member of this group."
        case "rate_limited": return "Too many requests. Wait a moment and try again."
        case "quota_exceeded": return "This group is out of storage."
        case "outbox_full": return "Retry or dismiss an unconfirmed send before sending more messages."
        case "send_retry_expired": return "This retry is over 24 hours old. Check whether it arrived before copying it into a new message."
        case "send_unavailable": return "This send cannot be retried on this device. Check whether it arrived before copying it into a new message."
        case "feature_disabled": return "Groups are not enabled for this account."
        case "not_found": return "Not found."
        default: return api.detail ?? "Something went wrong."
        }
    }
}
