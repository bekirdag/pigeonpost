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
    private(set) var quota: Quota?

    /// Messages this device has acknowledged but has not yet seen the server report as read.
    ///
    /// In memory only. It is a correction to listings that are already in flight, not a second
    /// record of what has been read — the server holds that, and this is empty again the moment
    /// the server agrees.
    private var acked: Set<String> = []

    /// Told when mail arrives that this inbox had not listed before, **and the app is not in front
    /// of anybody**.
    ///
    /// Set by the platform that wants to announce it. The Mac app posts a system notification from
    /// here, because on a desktop the app is usually the first to know; the phone leaves announcing
    /// to the postbox and APNs, which can also reach it while it is closed. While the app *is* in
    /// front of somebody neither of those is right, and `announcement` is what happens instead.
    var onArrival: (([Message]) -> Void)?

    /// Mail to say something about without taking anybody off the screen they are on.
    ///
    /// Cleared by whoever showed it. Keyed by the message id, so the same message arriving twice —
    /// once down the poll and once through APNs — replaces its own line rather than stacking a
    /// second one.
    var announcement: Announcement?

    struct Announcement: Identifiable, Equatable {
        /// The message id, which is also what makes this replace rather than repeat.
        let id: String
        let peer: String
        let title: String
        let body: String
    }

    /// Which conversation is on screen, so arrivals in it are not announced. Reading a message and
    /// being told about it at the same moment is the notification nobody wants.
    var reading: String?

    /// The ids of the previous listing. A listing is adopted whole, so anything in this one that
    /// was not in that one is new. The *first* listing only seeds this: mail that was waiting
    /// before the app opened is history, not news, and announcing all of it at launch is how a
    /// notification centre gets muted.
    private var listed: Set<String>?
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
        async let usage: Void = refreshQuota()
        _ = await (inbox, contacts, archive, threads, usage)
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
        // A listing is adopted whole, which is what makes the poll safe against a screen somebody
        // is reading. But a long poll opened *before* an ack was sent comes back carrying the state
        // from before it — so adopting it verbatim resurrects the unread mark on a message that was
        // just read. Holding the ids this device has acked, until the server's own listing agrees,
        // is what stops the badge coming back a second after it cleared.
        let incoming = (body.messages ?? []).map { message -> Message in
            guard acked.contains(message.messageId), !message.isRead else { return message }
            return message.markedRead()
        }
        // Anything the server now reports as read needs remembering no longer.
        let confirmed = Set(
            (body.messages ?? []).filter { $0.isRead }.map(\.messageId)
        )
        acked.subtract(confirmed)
        announce(incoming)
        messages = incoming
        policy = body.policy ?? policy
        let known = Set(messages.map(\.messageId))
        pending.removeAll { row in
            row.status != .failed && (row.sentCopyId.map(known.contains) ?? false)
        }
        rebuild()
    }

    /// Hand anything newly arrived to whoever is announcing mail on this platform.
    private func announce(_ incoming: [Message]) {
        let ids = Set(incoming.map(\.messageId))
        defer { listed = ids }
        guard let listed else { return }
        let fresh = incoming.filter { message in
            !listed.contains(message.messageId)
                && !message.isOutgoing
                && !message.isRead
                && message.peerKey != reading
        }
        guard !fresh.isEmpty else { return }
        tell(about: fresh)
    }

    /// Say it in the way that suits where the person is.
    ///
    /// In front of the app: a line at the top of the screen they are already looking at. Anywhere
    /// else: the platform's own notification, which is the only one that can reach a closed window
    /// or a locked phone. Never both, and — because `announce` has already dropped anything for the
    /// conversation on screen — never for the message being read.
    func tell(about fresh: [Message]) {
        guard let latest = fresh.last else { return }
        guard AppLife.isActive else {
            onArrival?(fresh)
            return
        }
        announcement = Announcement(
            id: latest.messageId,
            peer: latest.peerKey,
            title: fresh.count > 1
                ? "\(PeerFace.displayName(latest.peerKey)) and \(fresh.count - 1) more"
                : PeerFace.displayName(latest.peerKey),
            body: ConversationBuilder.preview(body: latest.body)
        )
    }

    /// The same line, for a notification that arrived through APNs while the app was open.
    ///
    /// The poll announces everything in the mailbox on screen; this covers the rest — mail for
    /// another of the account's mailboxes, which nothing here is watching and which would otherwise
    /// be a system banner over an app perfectly capable of saying it itself.
    func tell(remote peer: String, title: String, body: String, messageId: String) {
        guard peer != reading else { return }
        announcement = Announcement(id: messageId, peer: peer, title: title, body: body)
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
        // Another mailbox's acknowledgements say nothing about this one's mail.
        acked = []
        // And nothing it was about to say about them.
        announcement = nil
        // A new mailbox has its own history; none of the previous one's mail is news here either.
        listed = nil
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
        stageQuota()
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

    /// Everything waiting, for a badge to count.
    ///
    /// Deliberately not built from `visible`, which is narrowed by the search field and by whether
    /// the archive is open. A badge that drops to nothing because somebody typed in a search box is
    /// not counting anything a person would recognise.
    var unreadCount: Int {
        conversations.reduce(0) { total, conversation in
            archived.contains(conversation.peer) ? total : total + conversation.unread
        }
    }

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
        let ids = unread.map(\.messageId)
        acked.formUnion(ids)
        markRead(ids)
        guard !Fixtures.enabled else { return }
        confirm(ids, as: me.address)
    }

    /// Tell the postbox, from a task of this inbox's own rather than from the caller's.
    ///
    /// This used to be a loop in `acknowledge`, and not one ack ever left the device. The caller is
    /// a `.task(id:)` on the conversation whose id is made partly of the unread count — so marking
    /// the mail read locally, on the line above, changes that id, SwiftUI cancels the task that did
    /// it, and every `await` after that point is cancelled before it can reach the network.
    /// `try?` then swallowed the `CancellationError` and the loop ran to its end sending nothing.
    /// Measured on an iPhone 16 Pro with a probe build: `Task.isCancelled` was already true on the
    /// line after the local mark, every time.
    ///
    /// So the mark cleared on screen and the server never heard. It stayed cleared only because
    /// `acked` corrects the listings in flight — and `acked` is emptied by `reset()`, which is what
    /// switching mailbox does. Come back, and every mark is there again, exactly as reported.
    ///
    /// An unstructured `Task` does not inherit its parent's cancellation, which is the whole point:
    /// this outlives the screen that asked for it. The identity is captured rather than read later,
    /// so a mailbox switch mid-flight still acknowledges the mailbox the mail was read in.
    private func confirm(_ ids: [String], as identity: String) {
        let client = self.client
        Task {
            for id in ids {
                // Twice, and then let it go. A failed ack loses nothing — the message is still
                // there — but it is a mark that comes back, which is worth one more attempt over
                // a dropped connection.
                for attempt in 0..<2 {
                    do {
                        try await client.ack(identity: identity, messageId: id)
                        break
                    } catch {
                        guard attempt == 0 else { break }
                        try? await Task.sleep(nanoseconds: 400_000_000)
                    }
                }
            }
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
    /// Remove one message from this mailbox for good.
    ///
    /// Locally first, like `acknowledge`: the message is gone from the screen the moment it was
    /// asked for, and the next listing either confirms that or puts it back. Waiting on a round
    /// trip to remove something somebody just deleted makes the app feel like it disagreed.
    func deleteMessage(id: String) async {
        guard let me else { return }
        guard !Fixtures.enabled else {
            toast = "Deleted — from the fixture mailbox, which is only in memory."
            return
        }
        let removed = messages.first { $0.id == id }
        messages.removeAll { $0.id == id }
        rebuild()
        do {
            try await client.deleteMessage(identity: me.address, messageId: id)
            await refreshQuota()
        } catch {
            // Put it back rather than leaving a hole the next poll would fill unpredictably.
            if let removed {
                messages.append(removed)
                messages.sort { $0.at < $1.at }
                rebuild()
            }
            toast = "Could not delete that message."
        }
    }

    /// How full this mailbox is. Refreshed after anything that changes it, and on load — the quota
    /// refuses *senders*, so without asking, the holder is the last to know.
    func refreshQuota() async {
        #if DEBUG
        if stageQuota() { return }
        #endif
        guard let me, !Fixtures.enabled else { return }
        quota = try? await client.quota(identity: me.address)
    }

    #if DEBUG
    /// Stage the usage section for a screenshot. Synchronous and called from the fixture path too:
    /// with `-fixtures` the app installs a mailbox rather than loading one, so anything that only
    /// happens in `load()` never happens at all.
    @discardableResult
    func stageQuota() -> Bool {
        guard let staged = Fixtures.quotaState else { return false }
        let limit = 20 * 1024 * 1024
        let used = staged == "full" ? limit : limit / 100 * 87
        // Same key strategy the real client uses; a bare decoder would fail on `used_bytes` and
        // `try?` would swallow it, leaving the section invisible with nothing to explain it.
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        quota = try? decoder.decode(Quota.self, from: Data("""
        {"used_bytes": \(used), "limit_bytes": \(limit), "warn_at_bytes": \(limit / 5 * 4), "tier": "free"}
        """.utf8))
        return true
    }
    #endif

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

    func send(_ text: String, to peer: String, threadId: String?, files: [StagedFile] = []) async {
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
            // Uploaded before the send names them. A failure here stops the message rather than
            // sending it without the files it was about: half a message is worse than none,
            // because the sender cannot tell which half arrived.
            var ids: [String] = []
            for file in files {
                let uploaded = try await client.uploadAttachment(
                    identity: me.address,
                    data: file.data,
                    filename: file.name,
                    mediaType: file.mediaType
                )
                ids.append(uploaded.id)
            }
            let sent = try await client.sendMessage(
                from: me.address, to: peer, body: wire, threadId: threadId, attachments: ids)
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

    /// Delete a subject and the mail in it, from this mailbox.
    ///
    /// Irreversible, and only here — the other side keeps its copy, the same way archiving a peer
    /// does not reach into theirs. The listing is reloaded rather than patched: the messages went
    /// with it, and a local edit that guessed which ones would be a second opinion about what the
    /// server just did.
    func deleteThread(_ id: String, with peer: String) async throws {
        guard let me else { throw AuthError.sessionExpired }
        try await client.deleteThread(identity: me.address, id: id)
        await loadThreads()
        await loadAll()
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
            untrusted: untrusted,
            attachments: attachments
        )
    }
}
