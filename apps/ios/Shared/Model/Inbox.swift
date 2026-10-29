//  The mailbox this app is acting as: what is in it, and everything that changes it.
//
//  The server is the truth. Each listing is adopted whole rather than merged into, which is what
//  makes the poll safe to run against a screen somebody is reading; the only state of the app's own
//  is `pending`, the seconds between pressing send and the next poll.

import Foundation
import CryptoKit
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
    var readingSubthread: String?

    /// The ids of the previous listing. A listing is adopted whole, so anything in this one that
    /// was not in that one is new. The *first* listing only seeds this: mail that was waiting
    /// before the app opened is history, not news, and announcing all of it at launch is how a
    /// notification centre gets muted.
    private var listed: Set<String>?
    private(set) var pending: [PendingMessage] = []
    /// The conversation New conversation just started, for the window that started it to open.
    private(set) var started: StartedConversation?
    /// First messages the postbox did not confirm, waiting for their conversation's composer.
    private(set) var unconfirmedFirstSends: [UnconfirmedFirstSend] = []
    /// Whether that composer should take focus once the conversation is on screen.
    @ObservationIgnored private var startedFocus: (mailbox: String, peer: String)?
    /// The message reference of the send that started `started`, to find where the server filed it.
    @ObservationIgnored private var startedReference: String?
    private struct UploadSignature: Equatable {
        let digest: Data
        let name: String
        let mediaType: String
    }
    private struct SendUploads {
        let signature: [UploadSignature]
        var attachments: [UploadedAttachment]
    }
    private struct UploadCacheKey: Hashable {
        let mailbox: String
        let reference: String
    }
    @ObservationIgnored private var sendUploads: [UploadCacheKey: SendUploads] = [:]

    /// The assembled list, rebuilt when something it is made of changes rather than on every read —
    /// a search filters this on each keystroke.
    private(set) var conversations: [Conversation] = []

    private(set) var loading = false
    private(set) var hasLoaded = false
    @ObservationIgnored private var mailboxGeneration = UUID()
    @ObservationIgnored private var activeLoad: UUID?
    @ObservationIgnored private var mailboxIdentity: String?
    /// Set when the last attempt to reach the postbox failed. What is on screen stays on screen;
    /// this is what says why nothing new has arrived.
    private(set) var offline = false

    /// A transient message for the person, in the app's own voice.
    var toast: String?
    private(set) var composerAction: MessageComposerAction?
    private var fixtureMessageVersions: [String: [MessageVersion]] = [:]
    private(set) var actionOperation: UUID?
    var actionSending: Bool { actionOperation != nil }

    var filter = "" { didSet { rebuild() } }
    var viewingArchive = false

    init(account: Account) {
        self.account = account
        mailboxIdentity = account.me?.address
    }

    private var client: PostboxClient { account.client }
    private var me: Mailbox? { account.me }

    /// The generation also distinguishes A → B → A from an earlier visit to A.
    private struct MailboxRequest: Equatable {
        let identity: String
        let generation: UUID
    }

    private var mailboxRequest: MailboxRequest? {
        me.map { MailboxRequest(identity: $0.address, generation: mailboxGeneration) }
    }

    private func isCurrent(_ request: MailboxRequest) -> Bool {
        !Task.isCancelled && request == mailboxRequest
    }

    private func isCancellation(_ error: Error) -> Bool {
        error is CancellationError || (error as? URLError)?.code == .cancelled
    }

    // ---- loading ---------------------------------------------------------------------------

    func loadAll() async {
        guard !Task.isCancelled else { return }
        // Account changes can also come from Settings or another window, not only the picker.
        // A second window on the same inbox must not invalidate the first window's live task.
        if mailboxIdentity != me?.address { reset() }
        guard let request = mailboxRequest, isCurrent(request) else { return }
        let load = UUID()
        activeLoad = load
        loading = true
        defer {
            // A cancelled/older load must never dismiss a newer load's progress indicator.
            if activeLoad == load, request == mailboxRequest {
                loading = false
                activeLoad = nil
            }
        }
        async let inbox: Void = loadInbox(for: request)
        async let contacts: Void = loadContacts(for: request)
        async let archive: Void = loadArchive(for: request)
        async let threads: Void = loadThreads(for: request)
        async let usage: Void = loadQuota(for: request)
        _ = await (inbox, contacts, archive, threads, usage)
        guard isCurrent(request), activeLoad == load else { return }
        rebuild()
    }

    private func loadInbox(for captured: MailboxRequest? = nil) async {
        guard let request = captured ?? mailboxRequest, isCurrent(request) else { return }
        do {
            let body = try await client.inbox(identity: request.identity)
            guard isCurrent(request) else { return }
            adopt(body)
            hasLoaded = true
            offline = false
        } catch {
            guard isCurrent(request), !isCancellation(error) else { return }
            offline = true
            if !hasLoaded { toast = (error as? APIError)?.errorDescription ?? "Could not load this inbox. Try again." }
        }
    }

    private func loadContacts(for captured: MailboxRequest? = nil) async {
        guard let request = captured ?? mailboxRequest, isCurrent(request) else { return }
        do {
            let body = try await client.contacts(identity: request.identity)
            guard isCurrent(request) else { return }
            contacts = body.contacts ?? []
            vocabulary = body.vocabulary
            policy = body.policy ?? policy
        } catch {
            guard isCurrent(request), !isCancellation(error) else { return }
            contacts = []
        }
    }

    private func loadThreads(for captured: MailboxRequest? = nil) async {
        guard let request = captured ?? mailboxRequest, isCurrent(request) else { return }
        do {
            let threads = try await client.threads(identity: request.identity)
            guard isCurrent(request) else { return }
            serverThreads = threads
        } catch {
            guard isCurrent(request), !isCancellation(error) else { return }
            // A postbox that does not know about threads yet answers 404/501 here. Everything still
            // works: threads are then whatever the messages themselves say, and a peer with one
            // conversation — all such a postbox can produce — shows no thread list at all.
            serverThreads = []
        }
    }

    private func loadArchive(for captured: MailboxRequest? = nil) async {
        guard let request = captured ?? mailboxRequest, isCurrent(request) else { return }
        do {
            let archive = try await client.archive(identity: request.identity)
            guard isCurrent(request) else { return }
            archived = archive
        } catch {
            guard isCurrent(request), !isCancellation(error) else { return }
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
        let now = Int(Date().timeIntervalSince1970)
        let incoming = (body.messages ?? []).filter { ($0.expiresAt ?? .max) > now }.map { message -> Message in
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
        // A send whose answer was lost, that the postbox's own listing now shows it stored, was
        // delivered after all: the listing's copy is the message, and retrying it is moot.
        let stored = Set(messages.filter(\.isOutgoing).compactMap(\.msgRef))
        pending.removeAll { row in
            if row.status == .failed { return row.msgRef.map(stored.contains) ?? false }
            return row.sentCopyId.map(known.contains) ?? false
        }
        unconfirmedFirstSends.removeAll { stored.contains($0.reference) }
        followStartedConversation()
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
                && !(AppLife.isActive && message.peerKey == reading
                    && (readingSubthread == nil || (message.threadId ?? "") == readingSubthread))
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
        mailboxGeneration = UUID()
        mailboxIdentity = me?.address
        activeLoad = nil
        loading = false
        offline = false
        toast = nil
        composerAction = nil
        fixtureMessageVersions = [:]
        actionOperation = nil
        reading = nil
        readingSubthread = nil
        quota = nil
        policy = nil
        vocabulary = nil
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
        started = nil
        unconfirmedFirstSends = []
        startedFocus = nil
        startedReference = nil
        conversations = []
        hasLoaded = false
        filter = ""
        viewingArchive = false
    }

    #if DEBUG
    func installFixtures(messages: [Message], contacts: [Contact], vocabulary: Vocabulary?, threads: [ServerThread]) {
        self.messages = messages
        if CommandLine.arguments.contains("-message-actions"), !messages.contains(where: { $0.messageId == "action-seen" }) {
            let now = Int(Date().timeIntervalSince1970)
            let rows: [[String: Any]] = ["seen", "working", "failed"].enumerated().map { index, state in
                var row: [String: Any] = ["message_id": "action-" + state, "direction": "out", "peer": "/bekir/agent1",
                    "thread_id": "t-agent1", "body": state.capitalized + " fixture", "sent_at": now - 3 + index,
                    "receipt": ["state": state == "seen" ? "seen" : "delivered", "delivered_at": now - 2, "seen_at": now - 1]]
                if state != "seen" { row["agent_status"] = ["state": state, "at": now - 1] }
                return row
            }
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            if let data = try? JSONSerialization.data(withJSONObject: rows), let extra = try? decoder.decode([Message].self, from: data) {
                self.messages += extra
                self.messages.sort { $0.at < $1.at }
            }
        }
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
        guard let request = mailboxRequest, isCurrent(request) else { return }
        var backoff: UInt64 = 1
        while isCurrent(request) {
            do {
                let body = try await client.inbox(identity: request.identity, wait: Config.waitSeconds)
                guard isCurrent(request) else { return }
                adopt(body)
                hasLoaded = true
                offline = false
                backoff = 1
            } catch is CancellationError {
                return
            } catch let error as URLError where error.code == .cancelled {
                return
            } catch let error as APIError where error.status == 401 {
                guard isCurrent(request) else { return }
                // The client already spent a refresh token on this and the realm still says no.
                account.sessionExpired()
                return
            } catch {
                guard isCurrent(request) else { return }
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
        guard AppLife.isActive, let me else { return }
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
        // A message that never reached the postbox exists only here: discarding it is local.
        if let row = pending.first(where: { $0.id == id && $0.mailbox == me.address && $0.status == .failed }) {
            pending.removeAll { $0.id == row.id }
            if let reference = row.msgRef { abandonSend(reference: reference, mailbox: row.mailbox) }
            unconfirmedFirstSends.removeAll { $0.mailbox == row.mailbox && $0.reference == row.msgRef }
            rebuild()
            return
        }
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
        guard let request = mailboxRequest, !Fixtures.enabled else { return }
        await loadQuota(for: request)
    }

    private func loadQuota(for request: MailboxRequest) async {
        guard isCurrent(request) else { return }
        let loaded = try? await client.quota(identity: request.identity)
        guard isCurrent(request) else { return }
        quota = loaded
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

    /// How a send ended. `rejected` is the postbox refusing it outright — nothing was delivered;
    /// `unconfirmed` is everything else that was not a success, where it may or may not have been.
    enum SendOutcome: Equatable {
        case sent
        case refused
        case rejected(APIError)
        case unconfirmed
    }

    @discardableResult
    func send(_ text: String, to peer: String, threadId: String?, files: [StagedFile] = [],
              msgRef: String = MessageReference.make(), reply: MessageComposerAction? = nil) async -> Bool {
        await deliver(text, to: peer, threadId: threadId, files: files, msgRef: msgRef, reply: reply) == .sent
    }

    private func deliver(_ text: String, to peer: String, threadId: String?, files: [StagedFile] = [],
                         msgRef: String = MessageReference.make(), reply: MessageComposerAction? = nil,
                         announce: Bool = true) async -> SendOutcome {
        guard let me, let request = mailboxRequest,
              !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !files.isEmpty else { return .refused }
        if let reply {
            guard reply.kind == .reply, reply.scope.mailbox == me.address, reply.scope.peer == peer else { return .refused }
        }
        let targetThread = reply?.message.threadId ?? threadId
        let uploadKey = UploadCacheKey(mailbox: request.identity, reference: msgRef)
        let signature = files.map { UploadSignature(digest: Data(SHA256.hash(data: $0.data)), name: $0.name, mediaType: $0.mediaType) }
        if let cached = sendUploads[uploadKey], cached.signature != signature {
            toast = "This send changed. Start a new attempt."
            return .refused
        }
        // Only digests and server ids are retained, never a second copy of the files.
        // Refuse before uploading if this session already has too many unfinished file sends.
        if !files.isEmpty, sendUploads[uploadKey] == nil, sendUploads.count >= 64 {
            toast = "Too many unfinished file sends. Finish an existing send first."
            return .refused
        }
        var uploads = sendUploads[uploadKey] ?? SendUploads(signature: signature, attachments: [])
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
            threadId: targetThread,
            msgRef: msgRef,
            replyTo: reply.map { action in MessageReplyReference(
                msgRef: action.message.msgRef, messageId: action.message.id,
                from: action.message.kind == .outgoing ? me.address : action.message.address,
                direction: action.message.kind == .outgoing ? "out" : "in", excerpt: nil,
                quote: action.message.replyQuote) }
        )
        pending.removeAll { $0.mailbox == me.address && $0.msgRef == msgRef && $0.status == .failed }
        pending.append(row)
        rebuild()

        if Fixtures.enabled {
            #if DEBUG
            // `-first-send-unconfirmed` stages a first send whose answer was lost.
            if reply == nil, threadId == nil, CommandLine.arguments.contains("-first-send-unconfirmed") {
                update(row.id) { $0.status = .failed }
                rebuild()
                return .unconfirmed
            }
            #endif
            update(row.id) { $0.status = .sent }
            if let reply { cancelMessageAction(reply.id) }
            rebuild()
            return .sent
        }

        var outcome = SendOutcome.unconfirmed
        do {
            // Uploaded before the send names them. A failure here stops the message rather than
            // sending it without the files it was about: half a message is worse than none,
            // because the sender cannot tell which half arrived.
            var ids: [String] = []
            for (index, file) in files.enumerated() {
                let uploaded: UploadedAttachment
                if uploads.attachments.indices.contains(index) { uploaded = uploads.attachments[index] }
                else {
                    uploaded = try await client.uploadAttachment(identity: me.address, data: file.data, filename: file.name, mediaType: file.mediaType)
                    uploads.attachments.append(uploaded)
                    sendUploads[uploadKey] = uploads
                }
                ids.append(uploaded.id)
                update(row.id) { $0.attachments.append(MessageAttachment(id: uploaded.id, filename: uploaded.filename, mediaType: uploaded.mediaType, bytes: uploaded.bytes)) }
            }
            let sent = try await client.sendMessage(
                from: me.address, to: peer, body: wire, threadId: targetThread, attachments: ids,
                msgRef: msgRef, replyTo: reply?.message.reference, quote: reply?.message.replyQuote)
            update(row.id) {
                $0.status = .sent
                $0.sentCopyId = sent.sentCopyId
            }
            if sent.sentCopyId == nil {
                // Nothing to reconcile against if the postbox did not keep a copy: drop the
                // optimistic row and let the next listing be the truth.
                pending.removeAll { $0.id == row.id }
            }
            outcome = .sent
            sendUploads[uploadKey] = nil
            if isCurrent(request) {
                if let reply { cancelMessageAction(reply.id) }
                Task { await loadInbox(for: request) }
            }
        } catch let error as APIError {
            update(row.id) { $0.status = .failed }
            // A refusal is the postbox saying no; a timeout or a server error may have come after
            // the message was stored, so it is not confirmed either way.
            outcome = (400..<500).contains(error.status) && error.status != 408 ? .rejected(error) : .unconfirmed
            if announce, isCurrent(request) { toast = error.sendFailureMessage }
        } catch {
            update(row.id) { $0.status = .failed }
            if announce, isCurrent(request) { toast = "Could not reach the postbox." }
        }
        rebuild()
        return outcome
    }

    func action(in scope: MessageActionScope) -> MessageComposerAction? {
        guard let action = composerAction, action.scope == scope, scope.mailbox == me?.address else { return nil }
        return action
    }

    func abandonSend(reference: String, mailbox: String?) {
        guard let mailbox else { return }
        sendUploads[UploadCacheKey(mailbox: mailbox, reference: reference)] = nil
    }

    func messageInformation(_ message: ThreadMessage, in scope: MessageActionScope) async throws -> MessageInformation {
        guard let request = mailboxRequest, request.identity == scope.mailbox,
              conversation(with: scope.peer)?.messages.contains(where: { $0.id == message.id }) == true else {
            throw CancellationError()
        }
        if Fixtures.enabled {
            return MessageInformation(messageId: message.id, msgRef: message.msgRef,
                direction: message.kind == .outgoing ? "out" : "in", sentAt: message.at,
                versions: fixtureMessageVersions[message.id] ?? [MessageVersion(messageId: message.id, body: message.body, at: message.at)],
                replyTo: message.replyTo, receipt: message.receipt, agentStatus: message.agentStatus)
        }
        let information = try await client.messageInformation(identity: request.identity, messageId: message.id)
        guard isCurrent(request) else { throw CancellationError() }
        return information
    }

    func beginMessageAction(_ kind: MessageComposerAction.Kind, message: ThreadMessage, in scope: MessageActionScope) {
        guard !actionSending, scope.mailbox == me?.address, me != nil,
              message.kind != .system, message.status == .sent, !message.id.hasPrefix("local_"),
              message.expiresAt.map({ $0 > Int(Date().timeIntervalSince1970) }) ?? true,
              conversation(with: scope.peer)?.messages.contains(where: { $0.id == message.id }) == true else { return }
        if kind == .edit && !message.isEditable() { return }
        composerAction = MessageComposerAction(kind: kind, scope: scope, message: message,
                                               text: kind == .edit ? message.editingText : "")
    }

    func updateMessageAction(_ id: UUID, text: String) {
        guard composerAction?.id == id, !actionSending else { return }
        composerAction?.text = text
    }

    func cancelMessageAction(_ id: UUID) {
        if composerAction?.id == id { composerAction = nil }
    }

    @discardableResult
    func saveMessageEdit(_ action: MessageComposerAction) async -> Bool {
        guard !actionSending, action.kind == .edit, composerAction?.id == action.id,
              let request = mailboxRequest, request.identity == action.scope.mailbox,
              !action.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
        guard action.message.isEditable(),
              conversation(with: action.scope.peer)?.messages.contains(where: { $0.id == action.message.id }) == true else {
            toast = "That message is no longer editable."
            return false
        }
        let operation = UUID()
        actionOperation = operation
        defer { if actionOperation == operation { actionOperation = nil } }
        let old = action.attempt
        let attempt = old?.matches(text: action.text, fileIDs: [], replyTo: nil, editOf: action.message.reference) == true
            ? old! : MessageSendAttempt(text: action.text, fileIDs: [], editOf: action.message.reference)
        composerAction?.attempt = attempt
        let wire = action.message.bodyForEdit(action.text)
        if Fixtures.enabled {
            if let index = messages.firstIndex(where: { $0.messageId == action.message.id }) {
                if fixtureMessageVersions[action.message.id] == nil {
                    fixtureMessageVersions[action.message.id] = [MessageVersion(messageId: action.message.id, body: messages[index].body, at: action.message.at)]
                }
                messages[index].body = wire
                messages[index].editedAt = Int(Date().timeIntervalSince1970)
                messages[index].editCount = (messages[index].editCount ?? 0) + 1
                fixtureMessageVersions[action.message.id]?.append(MessageVersion(messageId: UUID().uuidString, body: wire, at: Int(Date().timeIntervalSince1970)))
                rebuild()
            }
            cancelMessageAction(action.id)
            return true
        }
        do {
            _ = try await client.sendMessage(from: request.identity, to: action.scope.peer, body: wire,
                                             threadId: action.message.threadId, msgRef: attempt.reference,
                                             editOf: action.message.reference)
            guard isCurrent(request) else { return true }
            cancelMessageAction(action.id)
            await loadInbox(for: request)
            return true
        } catch let error as APIError {
            if isCurrent(request) {
                toast = error.code == "edit_window_closed" ? "That message is no longer editable." : error.sendFailureMessage
            }
        } catch {
            if isCurrent(request) { toast = "Could not save the edit. Try again." }
        }
        return false
    }

    private func update(_ id: String, _ change: (inout PendingMessage) -> Void) {
        guard let index = pending.firstIndex(where: { $0.id == id }) else { return }
        change(&pending[index])
    }

    /// Start a conversation with an address, sending its first message, and say where it is.
    ///
    /// This used to send, reload, and hand back the typed address for the caller to select. A
    /// namespace (`/bekir`) is filed under the mailbox it resolves to (`/bekir/main`), so that
    /// selected a conversation that did not exist: "Pick a conversation". An unconfirmed send handed
    /// back nothing at all, leaving the dialog up with an error and the message perhaps stored.
    /// Now `started` names the conversation the moment the postbox has answered, bound to the
    /// mailbox that sent it, and every window opens it from there. The send's own optimistic row is
    /// already in it, so the message is on screen.
    ///
    /// When the listing says the server filed it under another peer (a namespace or a `/k/` address
    /// resolves to a mailbox), `started` moves there. A send whose delivery is not confirmed opens
    /// too, showing the message as not sent, with its words waiting in `unconfirmedFirstSends` for
    /// that conversation's composer under the same reference — so Send retries this message rather
    /// than sending a second one. Only an outright refusal stays in the dialog, and leaves nothing
    /// behind. Switching mailbox or account while this is in flight drops the result.
    func startConversation(to typed: String, body text: String) async -> StartOutcome {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let me, let request = mailboxRequest, !text.isEmpty else { return .failed(nil) }
        let reference = MessageReference.make()
        let key = ConversationBuilder.normalise(typed, against: messages)
        let outcome = await deliver(text, to: typed, threadId: nil, msgRef: reference, announce: false)
        // The answer belongs to the mailbox that asked. Another one on screen now opens nothing.
        guard request == mailboxRequest else { return .failed(nil) }
        switch outcome {
        case .sent:
            break
        case .refused:
            return .failed(nil)
        case .rejected(let error):
            // Nothing was delivered and nothing is waiting: no conversation is left behind for an
            // address that was refused, and the dialog keeps the words to correct.
            pending.removeAll { $0.mailbox == me.address && $0.msgRef == reference }
            abandonSend(reference: reference, mailbox: me.address)
            rebuild()
            return .failed(error.sendFailureMessage)
        case .unconfirmed:
            unconfirmedFirstSends.removeAll { $0.mailbox == me.address && $0.peer == key }
            unconfirmedFirstSends.append(UnconfirmedFirstSend(mailbox: me.address, peer: key, text: text, reference: reference))
            toast = "Delivery not confirmed. Send it again to retry, or discard it."
        }
        started = StartedConversation(id: UUID(), mailbox: me.address, peer: key)
        startedFocus = (me.address, key)
        startedReference = reference
        guard !Fixtures.enabled else { return .opened(key) }
        // Also after an unconfirmed send: the listing may show it was stored after all. Whichever
        // listing brings the sent copy back — this one or the send's own — moves `started` to it.
        await loadInbox(for: request)
        guard request == mailboxRequest, let started, started.mailbox == me.address else { return .opened(key) }
        return .opened(started.peer)
    }

    /// Move `started` to the peer the server filed its first message under, if that is not where
    /// it was opened. Called as each listing is adopted, in the same turn as the optimistic row is
    /// retired, so the opened conversation never empties out from under the window between the two.
    private func followStartedConversation() {
        guard let reference = startedReference, let current = started, current.mailbox == me?.address,
              let copy = messages.last(where: { $0.isOutgoing && $0.msgRef == reference }) else { return }
        startedReference = nil
        guard copy.peerKey != current.peer else { return }
        started = StartedConversation(id: UUID(), mailbox: current.mailbox, peer: copy.peerKey, replacing: current.peer)
        if startedFocus?.peer == current.peer { startedFocus = (current.mailbox, copy.peerKey) }
        unconfirmedFirstSends = unconfirmedFirstSends.map { waiting in
            guard waiting.mailbox == current.mailbox, waiting.peer == current.peer else { return waiting }
            return UnconfirmedFirstSend(mailbox: waiting.mailbox, peer: copy.peerKey, text: waiting.text, reference: waiting.reference)
        }
    }

    enum StartOutcome: Equatable {
        /// Open this conversation; its first message is in it, sent or waiting to be retried.
        case opened(String)
        /// Stay in the dialog, saying this if there is something to say.
        case failed(String?)
    }

    /// The unconfirmed first message for this conversation, once, for its composer.
    func takeUnconfirmedFirstSend(mailbox: String?, peer: String) -> UnconfirmedFirstSend? {
        guard let mailbox, mailbox == me?.address,
              let index = unconfirmedFirstSends.firstIndex(where: { $0.mailbox == mailbox && $0.peer == peer }) else { return nil }
        return unconfirmedFirstSends.remove(at: index)
    }

    /// Whether this conversation was just started here and its composer should take focus. Once.
    func takeStartedFocus(mailbox: String?, peer: String) -> Bool {
        guard let mailbox, mailbox == me?.address, let focus = startedFocus,
              focus.mailbox == mailbox, focus.peer == peer else { return false }
        startedFocus = nil
        return true
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

    /// Drop disappearing messages whose time has come, by this device's clock (SDS §17): what the
    /// postbox deletes every 30 s must not stay on this screen until the next listing.
    func pruneExpired() {
        let now = Int(Date().timeIntervalSince1970)
        guard messages.contains(where: { ($0.expiresAt ?? .max) <= now }) else { return }
        messages.removeAll { ($0.expiresAt ?? .max) <= now }
        rebuild()
    }

    /// The thread a peer's messages go to by default, for its settings (the timer).
    func defaultThread(with peer: String) -> ServerThread? {
        serverThreads.first { $0.peer == peer && $0.isDefault == true } ?? serverThreads.first { $0.peer == peer }
    }

    func reloadThreads() async {
        await loadThreads()
        rebuild()
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
        guard !Fixtures.enabled else {
            // The fixture mailbox keeps its decisions in memory, so a block or a trust change can
            // still be seen to take effect.
            contacts.removeAll { $0.peer == peer }
            contacts.append(Contact(peer: peer, alias: alias, admission: admission, autonomy: autonomy,
                                    allowedVerbs: allowedVerbs))
            rebuild()
            return
        }
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

    /// The fixture mailbox's way of forgetting a contact, which never reaches a server.
    func forgetFixtureContact(_ peer: String) {
        guard Fixtures.enabled else { return }
        contacts.removeAll { $0.peer == peer }
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
        var copy = self
        copy.read = true
        return copy
    }
}
