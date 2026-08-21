//  Everything the app knows about who has written and who has been written to.
//
//  A port of `buildThreads` in site-inbox/app.js, kept as pure functions over their inputs so the
//  assembly can be tested without a network, a token or a view. The grouping decisions are the web
//  app's and are load-bearing:
//
//  - Threads key on the handle when the sender has one, because that is what trust matches on and
//    what a person recognises. Outbound is normalised through the same map, so writing to `/k/…` and
//    hearing back from `/bekir/agent1` stays one conversation rather than two.
//  - Your own mailboxes are *marked*, never *created* as rows. A fleet of a dozen agents that have
//    never written to each other would otherwise fill the list with a dozen empty conversations,
//    burying the handful that are real — and the more agents someone runs, the worse it gets.
//  - Identity is the message id, enforced after every source has been merged rather than trusted to
//    each source. A conversation showing the same sentence twice is the kind of wrong that makes
//    someone distrust the whole app.

import Foundation

/// One row in a conversation, whichever direction it went and whether or not the server has it yet.
struct ThreadMessage: Identifiable, Equatable {
    enum Kind: Equatable { case incoming, outgoing }
    enum Status: Equatable { case sent, sending, failed }

    let id: String
    let kind: Kind
    let at: Int
    let body: String
    let threadId: String?
    var status: Status = .sent

    // Received mail only.
    var read: Bool = true
    /// `auto` or `review`, as the server decided. Never present on a sent copy: your own words were
    /// never subject to an admission decision.
    var autonomy: String?
    var verb: String?
    var heldBecause: String?
    var address: String?
    var standing: String?
    var tier: String?

    var isRequest: Bool { verb != nil }
    /// Both directions. A request you sent is still a request, and showing it as raw JSON in your
    /// own thread would make the composer look like it had done something strange.
    var envelope: RequestEnvelope? { RequestEnvelope(body: body) }
    var autoReply: AutoReply? { kind == .incoming ? AutoReply(body: body) : nil }
}

/// A message sent since the last poll, or one that failed outright.
///
/// In memory on purpose. A message that failed to send is worth showing until the app is relaunched
/// and worth forgetting after: it does not exist anywhere else, and persisting it would recreate the
/// per-device history the server-side sent copy replaced.
struct PendingMessage: Identifiable {
    let id: String
    let mailbox: String
    let to: String
    let body: String
    let at: Int
    var status: ThreadMessage.Status
    var threadId: String?
    /// The id of the server's own copy, once the send has been answered. Holding it is what lets
    /// this row retire the moment that copy comes back, instead of the message appearing twice.
    var sentCopyId: String?
}

struct Conversation: Identifiable, Equatable {
    let peer: String
    var messages: [ThreadMessage] = []
    var unread: Int = 0
    /// Requests from this peer that the server is holding for a decision.
    var held: Int = 0
    var last: Int = 0
    /// A mailbox on this account rather than somebody else's.
    var mine: Bool = false
    var identity: Mailbox?
    var contact: Contact?

    var id: String { peer }

    var name: String {
        if mine, let identity {
            if let handle = identity.handle { return PeerFace.displayName(handle) }
            return identity.label ?? PeerFace.displayName(identity.address)
        }
        if let alias = contact?.alias, !alias.isEmpty { return alias }
        return PeerFace.displayName(peer)
    }

    var isBlocked: Bool { contact?.admission == "block" }
}

/// One subject within a conversation. A peer with only one of these shows no thread list at all,
/// exactly as it looked before threads existed.
struct Subthread: Identifiable, Equatable {
    /// The server's thread id, or "" for mail from a postbox older than threads.
    let id: String
    var title: String?
    var isDefault: Bool
    var messages: [ThreadMessage]
    var unread: Int
    var last: Int

    var name: String {
        if let title, !title.isEmpty { return title }
        return isDefault ? "General" : "Untitled"
    }
}

enum ConversationBuilder {
    /// The whole list, newest first.
    static func build(
        messages: [Message],
        pending: [PendingMessage],
        contacts: [Contact],
        ownAgents: [Mailbox],
        acting: Mailbox?
    ) -> [Conversation] {
        var order: [String] = []
        var byPeer: [String: Conversation] = [:]

        func touch(_ peer: String) {
            if byPeer[peer] == nil {
                byPeer[peer] = Conversation(peer: peer)
                order.append(peer)
            }
        }

        // One list, both directions. `direction` comes from the server; its absence means an older
        // postbox that only ever returned received mail.
        for message in messages {
            let peer = message.peerKey
            touch(peer)
            if message.isOutgoing {
                byPeer[peer]?.messages.append(ThreadMessage(
                    id: message.messageId,
                    kind: .outgoing,
                    at: message.at,
                    body: message.body,
                    threadId: message.threadId
                ))
                continue
            }
            byPeer[peer]?.messages.append(ThreadMessage(
                id: message.messageId,
                kind: .incoming,
                at: message.at,
                body: message.body,
                threadId: message.threadId,
                read: message.isRead,
                autonomy: message.autonomy,
                verb: message.verb,
                heldBecause: message.heldBecause,
                address: message.from,
                standing: message.senderStanding,
                tier: message.senderTier
            ))
            if !message.isRead { byPeer[peer]?.unread += 1 }
            if message.autonomy == "review", message.verb != nil { byPeer[peer]?.held += 1 }
        }

        // Messages sent since the last poll, plus any that failed outright.
        for row in pending where row.mailbox == (acting?.address ?? "") {
            let peer = normalise(row.to, against: messages)
            touch(peer)
            byPeer[peer]?.messages.append(ThreadMessage(
                id: row.id,
                kind: .outgoing,
                at: row.at,
                body: row.body,
                threadId: row.threadId,
                status: row.status
            ))
        }

        // A contact you have never exchanged mail with still deserves a row — that is how you start
        // a conversation with an agent you have only been told about. Wildcards are policy, not
        // people.
        for contact in contacts where !contact.isWildcard {
            touch(contact.peer)
        }

        // Mark the rows that are your own agents, but do not create any.
        for mailbox in ownAgents {
            guard byPeer[mailbox.key] != nil else { continue }
            byPeer[mailbox.key]?.mine = true
            byPeer[mailbox.key]?.identity = mailbox
        }

        for peer in order {
            guard var conversation = byPeer[peer] else { continue }
            var seen = Set<String>()
            conversation.messages = conversation.messages.filter { message in
                // A failed local row has no server id and is still worth showing.
                guard !message.id.isEmpty else { return true }
                return seen.insert(message.id).inserted
            }
            conversation.messages.sort { $0.at < $1.at }
            conversation.last = conversation.messages.last?.at ?? 0
            conversation.contact = contact(for: peer, in: contacts)
            byPeer[peer] = conversation
        }

        // Recency first, as a messenger does — but a fleet the owner has never written to would
        // then sort in creation order, which is arbitrary. Fall back to the name so an untouched
        // list is at least alphabetical and stays put between renders.
        return order.compactMap { byPeer[$0] }.sorted { a, b in
            if a.last != b.last { return a.last > b.last }
            return a.name.localizedCaseInsensitiveCompare(b.name) == .orderedAscending
        }
    }

    /// A pending row is addressed however the user typed it. Normalise through what the server has
    /// said about this peer, so sending to `/k/…` and hearing back from `/bekir/agent1` is one
    /// conversation.
    static func normalise(_ target: String, against messages: [Message]) -> String {
        for message in messages {
            if message.peer == target || message.from == target,
               let handle = message.peerHandle ?? message.senderHandle {
                return handle
            }
            if message.peerHandle == target || message.senderHandle == target { return target }
        }
        return target
    }

    /// The peer's own contact row first, then their namespace's. Most specific wins, so an exact
    /// block on one agent still outranks trusting its whole fleet.
    static func contact(for peer: String, in contacts: [Contact]) -> Contact? {
        if let exact = contacts.first(where: { $0.peer == peer }) { return exact }
        let parts = peer.split(separator: "/").filter { !$0.isEmpty }
        guard parts.count >= 2 else { return nil }
        let wildcard = "/\(parts[0])/*"
        return contacts.first { $0.peer == wildcard }
    }

    /// The conversations with one peer, most recently active first.
    ///
    /// Built from the messages rather than from the server's list alone, so it is right even against
    /// a postbox that has no thread routes; the server's list is merged in on top because a thread
    /// somebody opened and has not written in yet exists only there.
    static func subthreads(
        of conversation: Conversation?,
        serverThreads: [ServerThread],
        peer: String,
        messages: [Message]
    ) -> [Subthread] {
        var order: [String] = []
        var byId: [String: Subthread] = [:]

        func touch(_ id: String) {
            if byId[id] == nil {
                byId[id] = Subthread(id: id, title: nil, isDefault: id.isEmpty, messages: [], unread: 0, last: 0)
                order.append(id)
            }
        }

        for message in conversation?.messages ?? [] {
            // A message with no thread comes from a postbox older than threads. Grouping those
            // under one key keeps them together as the single conversation they were.
            let id = message.threadId ?? ""
            touch(id)
            byId[id]?.messages.append(message)
            if message.kind == .incoming, !message.read { byId[id]?.unread += 1 }
            if message.at > (byId[id]?.last ?? 0) { byId[id]?.last = message.at }
        }

        for thread in serverThreads where normalise(thread.peer, against: messages) == peer {
            touch(thread.threadId)
            byId[thread.threadId]?.title = thread.title
            byId[thread.threadId]?.isDefault = thread.isDefault ?? false
            if let lastAt = thread.lastAt, lastAt > (byId[thread.threadId]?.last ?? 0) {
                byId[thread.threadId]?.last = lastAt
            }
        }

        // A message with no thread id *is* the default conversation — that is what it means on a
        // postbox older than threads, and what an optimistic row means before the server has filed
        // it. So when the peer has a thread the server calls default, the id-less group belongs to
        // it rather than beside it.
        //
        // Without this, one reply to a peer who had a single conversation split the screen into two
        // subjects both called "General", with the question in one and the answer in the other.
        if let orphan = byId[""],
           let defaultId = order.first(where: { $0 != "" && (byId[$0]?.isDefault ?? false) }),
           var host = byId[defaultId] {
            host.messages.append(contentsOf: orphan.messages)
            host.messages.sort { $0.at < $1.at }
            host.unread += orphan.unread
            host.last = max(host.last, orphan.last)
            byId[defaultId] = host
            byId[""] = nil
            order.removeAll { $0.isEmpty }
        }

        return order.compactMap { byId[$0] }.sorted { $0.last > $1.last }
    }

    /// Which thread a message typed into this conversation belongs to.
    ///
    /// Whatever is on screen — sending into the conversation you are reading is the only behaviour
    /// that does not surprise. That holds when there is only one subject and no strip is shown: the
    /// message still belongs to *that* thread, and sending it with no id instead is what created a
    /// second "General" beside the first.
    ///
    /// `nil` only for the id-less group, which is a postbox with no thread routes at all.
    static func targetThread(subthreads: [Subthread], selected: String?) -> String? {
        let chosen = selected ?? subthreads.first?.id
        guard let chosen, !chosen.isEmpty else { return nil }
        return chosen
    }

    /// What a row in the list says under the name.
    static func preview(_ message: ThreadMessage) -> String {
        if let envelope = RequestEnvelope(body: message.body) {
            return "asks to " + envelope.verb.replacingOccurrences(of: "_", with: " ")
        }
        // An unattended answer previews as the answer. Its two header lines are identical on every
        // one of them, so a list of them would otherwise read as a list of the same message.
        let text = AutoReply(body: message.body)?.body ?? message.body
        let flattened = text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        return String(flattened.prefix(140))
    }

    /// Why the server is holding a request, in words.
    static func heldReason(_ code: String) -> String {
        switch code {
        case "sender_not_auto": return "this sender was never granted autonomy"
        case "verb_denied": return "that verb was not granted to this sender"
        case "verb_never_auto": return "this verb is never auto-accepted, whoever asks"
        case "not_a_request": return "not a scoped request"
        case "unknown_verb": return "not a verb this postbox knows"
        default: return code.replacingOccurrences(of: "_", with: " ")
        }
    }
}
