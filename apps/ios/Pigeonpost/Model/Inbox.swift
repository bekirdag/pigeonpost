//  The mailbox this app is acting as: what is in it, and everything that changes it.
//
//  The server is the truth. Each listing is adopted whole rather than merged into, which is what
//  makes the poll safe to run against a screen somebody is reading; the only state of the app's own
//  is `pending`, the seconds between pressing send and the next poll.

import Foundation
import Observation

@MainActor
@Observable
final class Inbox {
    let account: Account

    private(set) var messages: [Message] = []
    private(set) var contacts: [Contact] = []
    private(set) var vocabulary: Vocabulary?
    private(set) var policy: InboxPolicy?
    private(set) var serverThreads: [ServerThread] = []
    private(set) var archived: Set<String> = []
    private(set) var pending: [PendingMessage] = []

    /// The assembled list, rebuilt when something it is made of changes rather than on every read —
    /// a search filters this on each keystroke.
    private(set) var conversations: [Conversation] = []

    private(set) var loading = false
    private(set) var hasLoaded = false
    /// Set when the last attempt to reach the postbox failed. What is on screen stays on screen;
    /// this is what says why nothing new has arrived.
    private(set) var offline = false

    /// A transient message for the person, in the app's own voice.
    var toast: String?

    var filter = "" { didSet { rebuild() } }
    var viewingArchive = false

    init(account: Account) {
        self.account = account
    }

    private var client: PostboxClient { account.client }
    private var me: Mailbox? { account.me }

    // ---- loading ---------------------------------------------------------------------------

    func loadAll() async {
        guard let me else { return }
        loading = !hasLoaded
        async let inbox: Void = loadInbox()
        async let contacts: Void = loadContacts()
        async let archive: Void = loadArchive()
        async let threads: Void = loadThreads()
        _ = await (inbox, contacts, archive, threads)
        _ = me
        loading = false
        hasLoaded = true
        rebuild()
    }

    private func loadInbox() async {
        guard let me else { return }
        do {
            adopt(try await client.inbox(identity: me.address))
            offline = false
        } catch let error as APIError {
            offline = true
            if !hasLoaded { toast = error.errorDescription }
        } catch {
            offline = true
        }
    }

    private func loadContacts() async {
        guard let me else { return }
        do {
            let body = try await client.contacts(identity: me.address)
            contacts = body.contacts ?? []
            vocabulary = body.vocabulary
            policy = body.policy ?? policy
        } catch {
            contacts = []
        }
    }

    private func loadThreads() async {
        guard let me else { return }
        do {
            serverThreads = try await client.threads(identity: me.address)
        } catch {
            // A postbox that does not know about threads yet answers 404/501 here. Everything still
            // works: threads are then whatever the messages themselves say, and a peer with one
            // conversation — all such a postbox can produce — shows no thread list at all.
            serverThreads = []
        }
    }

    private func loadArchive() async {
        guard let me else { return }
        do {
            archived = try await client.archive(identity: me.address)
        } catch {
            // An archive we could not read must not hide anything. Failing open shows a
            // conversation that should have been filed; failing closed hides one that should not
            // be. Only one of those loses mail.
            archived = []
        }
    }

    /// Take a server listing as the truth, and retire any optimistic row it now accounts for.
    private func adopt(_ body: InboxResponse) {
        messages = body.messages ?? []
        policy = body.policy ?? policy
        let known = Set(messages.map(\.messageId))
        pending.removeAll { row in
            row.status != .failed && (row.sentCopyId.map(known.contains) ?? false)
        }
        rebuild()
    }

    private func rebuild() {
        conversations = ConversationBuilder.build(
            messages: messages,
            pending: pending,
            contacts: contacts,
            ownAgents: account.ownAgents,
            acting: me
        )
    }

    /// Reset to nothing. Called when the acting mailbox changes: the previous mailbox's mail must
    /// not be on screen for even one frame while the new one loads.
    func reset() {
        messages = []
        contacts = []
        serverThreads = []
        archived = []
        pending = []
        conversations = []
        hasLoaded = false
        filter = ""
        viewingArchive = false
    }

    #if DEBUG
    func installFixtures(messages: [Message], contacts: [Contact], vocabulary: Vocabulary?, threads: [ServerThread]) {
        self.messages = messages
        self.contacts = contacts
        self.vocabulary = vocabulary
        self.serverThreads = threads
        hasLoaded = true
        rebuild()
    }
    #endif

    // ---- what the list shows -----------------------------------------------------------------

    var visible: [Conversation] {
        conversations.filter { conversation in
            guard archived.contains(conversation.peer) == viewingArchive else { return false }
            guard !filter.isEmpty else { return true }
            let needle = filter.lowercased()
            if conversation.name.lowercased().contains(needle) { return true }
            if conversation.peer.lowercased().contains(needle) { return true }
            return conversation.messages.contains { $0.body.lowercased().contains(needle) }
        }
    }

    var archivedCount: Int { archived.count }

    func conversation(with peer: String) -> Conversation? {
        conversations.first { $0.peer == peer }
    }

    func subthreads(of peer: String) -> [Subthread] {
        ConversationBuilder.subthreads(
            of: conversation(with: peer),
            serverThreads: serverThreads,
            peer: peer,
            messages: messages
        )
    }

    // ---- the live inbox ------------------------------------------------------------------------

    /// Long-poll: the postbox holds the request open until mail lands or the budget runs out, so
    /// this is a live inbox without a socket and without hammering the server. Runs for as long as
    /// the caller's task lives — which is while the conversation list is on screen and the app is
    /// in front of the person.
    func live() async {
        var backoff: UInt64 = 1
        while !Task.isCancelled {
            guard let me else { return }
            do {
                let body = try await client.inbox(identity: me.address, wait: Config.waitSeconds)
                if Task.isCancelled { return }
                adopt(body)
                offline = false
                backoff = 1
            } catch is CancellationError {
                return
            } catch let error as URLError where error.code == .cancelled {
                return
            } catch let error as APIError where error.status == 401 {
                // The client already spent a refresh token on this and the realm still says no.
                account.sessionExpired()
                return
            } catch {
                if Task.isCancelled { return }
                // Offline, proxy hiccup, a postbox restarting. Temporary — back off rather than
                // spin, and leave what is on screen where it is.
                offline = true
                try? await Task.sleep(nanoseconds: backoff * 1_000_000_000)
                backoff = min(backoff * 2, 30)
            }
        }
    }

    // ---- acting ---------------------------------------------------------------------------------

    /// Opening a conversation is reading it. Acknowledging clears the unread mark server-side,
    /// which is also what tells an agent sharing this mailbox that the message has been dealt with
    /// — so it is a real decision, not a UI flourish.
    ///
    /// Scoped to what is actually on screen: with several threads open on a peer, marking the others
    /// read because one of them was looked at would clear a mark nobody has seen.
    func acknowledge(peer: String, subthread: String?) async {
        guard let me else { return }
        let unread = messages.filter { message in
            message.peerKey == peer
                && !message.isOutgoing
                && !message.isRead
                && (subthread == nil || (message.threadId ?? "") == subthread)
        }
        guard !unread.isEmpty else { return }

        // Locally first: the mark is gone the moment it was read, and the server call is a
        // formality that either confirms it or is corrected by the next listing.
        markRead(unread.map(\.messageId))
        guard !Fixtures.enabled else { return }

        for message in unread {
            // Nothing is lost by a failed ack — the message is still there on the next poll.
            try? await client.ack(identity: me.address, messageId: message.messageId)
        }
    }

    private func markRead(_ ids: [String]) {
        let wanted = Set(ids)
        messages = messages.map { message in
            guard wanted.contains(message.messageId) else { return message }
            return message.markedRead()
        }
        rebuild()
    }

    /// Report one message to the postbox.
    ///
    /// Message-level on purpose: what is objectionable is a message, and the person reporting it is
    /// looking at that message. Refusing the sender altogether is a separate, heavier decision and
    /// lives on the sender panel as `block`.
    func reportSpam(messageId: String) async {
        guard let me else { return }
        guard !Fixtures.enabled else {
            toast = "Reported. Nothing left this device — this is the fixture mailbox."
            return
        }
        do {
            try await client.reportSpam(identity: me.address, messageId: messageId)
            toast = "Reported. The postbox has been told."
        } catch let error as APIError {
            toast = error.errorDescription ?? "Could not report that message."
        } catch {
            toast = "Could not report that message."
        }
    }

    /// Admit this sender by name.
    ///
    /// "Known" is not a flag on the postbox — it is having a contact row at all. A sender with one
    /// is somebody this mailbox has decided about; a sender without one is a stranger, subject to
    /// whatever the inbox policy does with strangers.
    func markKnown(peer: String) async {
        let existing = ConversationBuilder.contact(for: peer, in: contacts)
        do {
            try await saveContact(
                peer: peer,
                alias: existing?.alias,
                admission: "allow",
                // Left where it was. Knowing somebody is not the same as letting their requests run.
                autonomy: existing?.autonomy ?? "review",
                allowedVerbs: existing?.allowedVerbs ?? []
            )
            toast = "Marked as known."
        } catch {
            toast = "Could not save that."
        }
    }

    /// Trust this sender with everything this postbox will let anyone grant.
    ///
    /// Autonomy `auto` plus every grantable verb: their requests are acted on without waiting for a
    /// human. The server still refuses what it never auto-accepts for anyone — `read_credentials`,
    /// `spend`, `delete_files`, `run_shell` are not on the grantable list and cannot be put there —
    /// and the receiving machine's own permission tier, branch allowlist and daily ceiling all
    /// still apply on top. This is the most one mailbox can say about another, not a master key.
    func grantFullPermissions(peer: String, granted: Bool) async {
        let existing = ConversationBuilder.contact(for: peer, in: contacts)
        let verbs = granted ? (vocabulary?.grantable ?? []) : []
        do {
            try await saveContact(
                peer: peer,
                alias: existing?.alias,
                admission: "allow",
                autonomy: granted ? "auto" : "review",
                allowedVerbs: verbs
            )
            toast = granted
                ? "Full permissions. Their requests are acted on without asking."
                : "Back to review. Their requests wait for you."
        } catch {
            toast = "Could not save that."
        }
    }

    /// Whether this peer already has everything this postbox allows.
    func hasFullPermissions(_ peer: String) -> Bool {
        guard let contact = ConversationBuilder.contact(for: peer, in: contacts),
              contact.autonomy == "auto",
              contact.admission == "allow"
        else { return false }
        let granted = Set(contact.allowedVerbs ?? [])
        let grantable = Set(vocabulary?.grantable ?? [])
        return !grantable.isEmpty && grantable.isSubset(of: granted)
    }

    /// Whether this peer is somebody this mailbox has decided about at all.
    func isKnown(_ peer: String) -> Bool {
        ConversationBuilder.contact(for: peer, in: contacts) != nil
    }

    /// The row for this peer by name, as opposed to one their whole namespace is covered by. The
    /// difference matters when editing: a toggle that looks like it governs one sender must not
    /// silently rewrite the terms of their entire fleet.
    func exactContact(_ peer: String) -> Contact? {
        contacts.first { $0.peer == peer }
    }

    /// The namespace rule covering this peer, if one does.
    func wildcardContact(_ peer: String) -> Contact? {
        guard let exact = ConversationBuilder.contact(for: peer, in: contacts), exact.peer != peer
        else { return nil }
        return exact
    }

    /// Forget this sender by name. They revert to whatever strangers get, or to their namespace's
    /// rule if one covers them.
    func forget(peer: String) async {
        do {
            try await removeContact(peer: peer)
            toast = "Forgotten. They are a stranger again."
        } catch {
            toast = "Could not remove that sender."
        }
    }

    /// Refuse a sender's mail from here on. Their existing messages stay — nothing is deleted — but
    /// the postbox stops admitting new ones.
    func block(peer: String) async {
        let existing = ConversationBuilder.contact(for: peer, in: contacts)
        do {
            try await saveContact(
                peer: peer,
                alias: existing?.alias,
                admission: "block",
                // Blocked and auto are a contradiction the server would have to resolve on its own.
                autonomy: "review",
                allowedVerbs: []
            )
            toast = "Blocked. Their mail is refused from now on."
        } catch {
            toast = "Could not block that sender."
        }
    }

    func send(_ text: String, to peer: String, threadId: String?) async {
        guard let me else { return }
        // Everything sent from this app asks for work, at the most it can ask for. The optimistic
        // row carries the envelope too, so what is on screen the second after pressing send is what
        // the next poll will bring back.
        let wire = RequestEnvelope.work(text)
        let row = PendingMessage(
            id: "local_" + UUID().uuidString,
            mailbox: me.address,
            to: peer,
            body: wire,
            at: Int(Date().timeIntervalSince1970),
            status: .sending,
            threadId: threadId
        )
        pending.append(row)
        rebuild()

        if Fixtures.enabled {
            update(row.id) { $0.status = .sent }
            rebuild()
            return
        }

        do {
            let sent = try await client.sendMessage(from: me.address, to: peer, body: wire, threadId: threadId)
            update(row.id) {
                $0.status = .sent
                $0.sentCopyId = sent.sentCopyId
            }
            if sent.sentCopyId == nil {
                // Nothing to reconcile against if the postbox did not keep a copy: drop the
                // optimistic row and let the next listing be the truth.
                pending.removeAll { $0.id == row.id }
            }
            await loadInbox()
        } catch let error as APIError {
            update(row.id) { $0.status = .failed }
            toast = error.sendFailureMessage
        } catch {
            update(row.id) { $0.status = .failed }
            toast = "Could not reach the postbox."
        }
        rebuild()
    }

    private func update(_ id: String, _ change: (inout PendingMessage) -> Void) {
        guard let index = pending.firstIndex(where: { $0.id == id }) else { return }
        change(&pending[index])
    }

    /// Start a conversation with an address nobody has written from yet. Returns the peer the
    /// server filed it under, which is not always what was typed — a namespace or a `/k/` address
    /// resolves to a mailbox.
    func startConversation(to peer: String, body text: String) async throws -> String {
        guard let me else { throw AuthError.sessionExpired }
        _ = try await client.sendMessage(from: me.address, to: peer, body: RequestEnvelope.work(text), threadId: nil)
        await loadAll()
        let normalised = ConversationBuilder.normalise(peer, against: messages)
        return conversation(with: normalised)?.peer ?? normalised
    }

    func openThread(with peer: String, title: String) async throws -> String {
        guard let me else { throw AuthError.sessionExpired }
        let id = try await client.openThread(identity: me.address, peer: peer, title: title)
        await loadThreads()
        rebuild()
        return id
    }

    func setArchived(_ peer: String, archived isArchived: Bool) async {
        guard let me else { return }
        // Move it first: filing something is a gesture that should feel instant, and the server
        // call either confirms it or is undone below.
        if isArchived { archived.insert(peer) } else { archived.remove(peer) }
        guard !Fixtures.enabled else { return }
        do {
            try await client.setArchived(identity: me.address, peer: peer, archived: isArchived)
            toast = isArchived ? "Archived." : "Moved back to your inbox."
        } catch {
            if isArchived { archived.remove(peer) } else { archived.insert(peer) }
            toast = "Could not update the archive."
        }
    }

    func saveContact(peer: String, alias: String?, admission: String, autonomy: String, allowedVerbs: [String]) async throws {
        guard let me else { throw AuthError.sessionExpired }
        guard !Fixtures.enabled else { return }
        try await client.putContact(
            identity: me.address,
            peer: peer,
            alias: alias,
            admission: admission,
            autonomy: autonomy,
            allowedVerbs: allowedVerbs
        )
        await loadContacts()
        rebuild()
    }

    func removeContact(peer: String) async throws {
        guard let me else { throw AuthError.sessionExpired }
        try await client.removeContact(identity: me.address, peer: peer)
        await loadContacts()
        rebuild()
    }
}

private extension Message {
    /// `read` arrives from the server and is the one field the app changes on its own, the moment a
    /// conversation is opened.
    func markedRead() -> Message {
        Message(
            messageId: messageId,
            from: from,
            to: to,
            body: body,
            direction: direction,
            peer: peer,
            peerHandle: peerHandle,
            senderHandle: senderHandle,
            threadId: threadId,
            receivedAt: receivedAt,
            sentAt: sentAt,
            read: true,
            autonomy: autonomy,
            verb: verb,
            heldBecause: heldBecause,
            alias: alias,
            matchedContact: matchedContact,
            senderStanding: senderStanding,
            senderTier: senderTier,
            senderKnown: senderKnown,
            untrusted: untrusted
        )
    }
}
