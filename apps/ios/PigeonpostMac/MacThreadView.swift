//  One conversation, on a desktop.
//
//  The bubbles, the markdown and the attachment rows are the phone's — `MessageBubble` and its
//  neighbours are SwiftUI over shared models and need nothing platform-specific. What the Mac adds
//  is a composer that sends on Return, drag-and-drop onto the thread, and a window that can be made
//  as wide as the conversation deserves.

import SwiftUI
import UniformTypeIdentifiers

struct MacThreadView: View {
    let peer: String
    /// Which subject is being read, chosen in the sidebar. `nil` means the whole conversation.
    let subthread: String?

    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(\.scenePhase) private var scenePhase
    @Environment(CallLog.self) private var callLog: CallLog?
    @State private var blocking: BlockTarget?
    private struct ComposerDraft { var text = ""; var files: [StagedFile] = []; var attempt: MessageSendAttempt? }
    @State private var drafts: [ScrollContext: ComposerDraft] = [:]
    @State private var sending: Set<ScrollContext> = []
    private var composerKey: ScrollContext { ScrollContext(mailbox: account.me?.address, peer: peer, subject: subthread) }
    private var actionScope: MessageActionScope { MessageActionScope(mailbox: account.me?.address, peer: peer, subject: subthread) }
    private var action: MessageComposerAction? { inbox.action(in: actionScope) }
    private var editing: Bool { action?.kind == .edit }
    private var sendPending: Bool { sending.contains(composerKey) || (editing && inbox.actionSending) }
    private var draft: String {
        get { editing ? action?.text ?? "" : drafts[composerKey]?.text ?? "" }
        nonmutating set {
            if let action, editing { inbox.updateMessageAction(action.id, text: newValue) }
            else { drafts[composerKey, default: ComposerDraft()].text = newValue }
        }
    }
    private var staged: [StagedFile] {
        get { editing ? [] : drafts[composerKey]?.files ?? [] }
        nonmutating set { if !editing { drafts[composerKey, default: ComposerDraft()].files = newValue } }
    }
    @State private var dropping = false
    /// Counts finished sends; the composer takes focus back on each.
    @State private var sent = 0
    /// What the toolbar's search field is looking for, inside this conversation.
    @State private var find = ""
    /// Which hit is being shown, as an index into `matches`.
    @State private var matchIndex = 0

    private var conversation: Conversation? { inbox.conversation(with: peer) }
    private var subthreads: [Subthread] { inbox.subthreads(of: peer) }

    private var shown: [ThreadMessage] {
        guard let conversation else { return [] }
        let messages = subthread.map { id in
            conversation.messages.filter { ($0.threadId ?? "") == id }
        } ?? conversation.messages
        // Missed calls are lines in the whole conversation and in its General subject only.
        let general = subthread.map { id in subthreads.first { $0.id == id }?.isDefault ?? true } ?? true
        return CallLogEntry.merge(messages, general ? (callLog?.lines(for: conversation) ?? []) : [])
    }

    /// The messages the find bar has matched, in the order they appear.
    ///
    /// A find bar, not a filter. Filtering hides everything around a hit, which is most of what
    /// makes a hit worth finding — you search a conversation to read the part *near* the words, not
    /// to see the words alone.
    private var matches: [ThreadMessage] {
        let needle = find.trimmed
        guard !needle.isEmpty else { return [] }
        return shown.filter { $0.body.localizedCaseInsensitiveContains(needle) }
    }

    private var currentMatch: String? {
        guard !matches.isEmpty else { return nil }
        return matches[min(matchIndex, matches.count - 1)].id
    }

    /// Step to the next hit, wrapping. Wrapping rather than stopping at the end, because a find bar
    /// that goes dead on the last match makes you retype the word to start again.
    private func step(_ by: Int) {
        guard !matches.isEmpty else { return }
        matchIndex = (matchIndex + by + matches.count) % matches.count
    }

    var body: some View {
        // Keep one snapshot for this render. Filtering again from inside every lazy row makes
        // changing a subject quadratic and can mix two mailbox snapshots during a transition.
        let messages = shown
        let foundMessage = currentMatch
        let scrollContext = ScrollContext(mailbox: account.me?.address, peer: peer, subject: subthread)
        let scrollRequest = ScrollRequest(context: scrollContext, last: messages.last?.id, count: messages.count)

        VStack(spacing: 0) {
            ScrollViewReader { scroller in
                ScrollView {
                    LazyVStack(spacing: 16) {
                        ForEach(Array(messages.enumerated()), id: \.element.id) { index, message in
                            // A lazy row has one stable child, including its optional day label.
                            // Flattening the label and bubble makes row counts change during a
                            // scroll and forces the stack to measure off-screen message bodies.
                            VStack(spacing: 8) {
                                if index == 0 || !Time.sameDay(messages[index - 1].at, message.at) {
                                    DayDivider(label: Time.dayLabel(message.at))
                                }
                                MessageBubble(
                                    message: message,
                                    highlight: find,
                                    isFound: message.id == foundMessage,
                                    actionScope: actionScope,
                                    jumpToMessage: { id in
                                        if messages.contains(where: { $0.id == id }) { scroller.scrollTo(id, anchor: .center) }
                                        else { inbox.toast = "The original message is no longer in this thread." }
                                    }
                                )
                            }
                            .id(message.id)
                        }
                        Color.clear.frame(height: 1).id(Self.floor)
                    }
                    // The conversation design's column: centred, never wider than a line can be
                    // read across, with its margins.
                    .frame(maxWidth: 860)
                    .frame(maxWidth: .infinity)
                    .padding(.horizontal, 24)
                    .padding(.top, 16)
                    .padding(.bottom, 24)
                }
                // Behind the messages and not behind the composer, which is where the phone puts it
                // too. The pattern is the paper a conversation is written on; the composer is a
                // control sitting on top of the paper, not part of it.
                .background { DoodleBackground() }
                // Coalesce initial, subject and message scrolls into one cancellable task. The
                // old onChange callbacks could scroll a stack while its rows were being removed
                // for another mailbox. A cancelled task must not touch its old scroll proxy.
                .task(id: scrollRequest) {
                    await Task.yield()
                    guard !Task.isCancelled, !messages.isEmpty,
                          account.me?.address == scrollContext.mailbox,
                          inbox.reading == peer else { return }
                    scroller.scrollTo(Self.floor, anchor: .bottom)
                }
                // Typing restarts the walk. Keeping the old index would land you in the middle of
                // the results for a word you have only just finished typing.
                .onChange(of: find) { _, _ in
                    matchIndex = 0
                    if let first = matches.first?.id {
                        withAnimation(.easeInOut(duration: 0.15)) {
                            scroller.scrollTo(first, anchor: .center)
                        }
                    }
                }
                // Centred, not merely brought on screen: a hit at the very edge of the view is one
                // you have to look for twice.
                .onChange(of: matchIndex) { _, _ in
                    guard let current = currentMatch else { return }
                    withAnimation(.easeInOut(duration: 0.15)) {
                        scroller.scrollTo(current, anchor: .center)
                    }
                }
                .onChange(of: action?.id) { _, next in if next != nil { sent += 1 } }
            }
            // A different subject needs fresh lazy layout measurements. The composer and its
            // draft stay outside this identity boundary.
            .id(scrollContext)
            if let conversation, conversation.isBlocked {
                BlockedBanner(conversation: conversation)
            } else {
                composer
            }
        }
        .background(Theme.wash)
        // A file dropped onto the conversation is the natural desktop gesture for sending one, and
        // it is the reason a Mac app beats the web app here at all.
        .onDrop(of: [.fileURL], isTargeted: $dropping) { providers in
            for provider in providers {
                _ = provider.loadObject(ofClass: URL.self) { url, _ in
                    guard let url else { return }
                    Task { @MainActor in stage(url) }
                }
            }
            return true
        }
        .overlay {
            if dropping {
                RoundedRectangle(cornerRadius: 8)
                    .strokeBorder(Theme.navy, style: StrokeStyle(lineWidth: 2, dash: [6]))
                    .padding(6)
                    .allowsHitTesting(false)
            }
        }
        // The find bar, drawn here rather than through `.searchable`.
        //
        // `.searchable` puts an `NSSearchField` in the toolbar and that field rendered its text in
        // the light appearance while the window was dark: near-black on charcoal, measured at
        // luminance 6 against a background of 40. Nothing in this app sets an appearance, and it is
        // not a colour a caller can override — `.searchable` owns its field. A plain `TextField`
        // with this app's own colours is readable by construction, and it also lets the count and
        // the two chevrons sit *in* the bar where they belong instead of beside it.
        .toolbar {
            ToolbarItem(placement: .primaryAction) { findBar }
            // Voice and video, beside the find bar: what you do *with* this conversation.
            ToolbarItem(placement: .primaryAction) {
                HStack(spacing: 8) {
                    DirectTimerBadge(peer: peer)
                    CallButtons(peer: peer)
                }
            }
            // Block, beside the call buttons and drawn the same way: a tapped image, not a Button.
            ToolbarItem(placement: .primaryAction) {
                if let conversation, !conversation.isBlocked {
                    Image(systemName: "hand.raised")
                        .font(.geist(14, .medium))
                        .foregroundStyle(Theme.secondary)
                        .frame(width: 22, height: 22)
                        .contentShape(Rectangle())
                        .onTapGesture {
                            blocking = BlockTarget(peer: peer, name: conversation.name, origin: .thread)
                        }
                        .help("Block \(conversation.name)")
                        .accessibilityLabel("Block \(conversation.name)")
                        .accessibilityIdentifier("blockThread")
                        .accessibilityAddTraits(.isButton)
                }
            }
        }
        .sheet(item: $blocking) { target in
            BlockSheet(target: target).frame(width: 460, height: 300)
        }
        .task(id: taskKey) {
            guard scenePhase == .active else { return }
            await inbox.acknowledge(peer: peer, subthread: subthread)
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            Task { await inbox.acknowledge(peer: peer, subthread: subthread) }
        }

    }

    /// Re-acknowledge when the subject changes or new mail lands, but not on every render.
    private var taskKey: String { "\(peer)|\(subthread ?? "")|\(conversation?.unread ?? 0)|\(scenePhase)" }

    /// Find in this conversation: a field, the tally, and the two ways through it.
    private var findBar: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .font(.geist(12, .medium))
                .foregroundStyle(Theme.muted)
                .fixedSize()

            TextField("Search this conversation", text: $find)
                .textFieldStyle(.plain)
                .font(.geist(13))
                .foregroundStyle(Theme.ink)
                .frame(width: 190)
                .onExitCommand { find = "" }

            if !find.trimmed.isEmpty {
                Text(matches.isEmpty
                     ? "none"
                     : "\(min(matchIndex, matches.count - 1) + 1) of \(matches.count)")
                    .font(.geist(11).monospacedDigit())
                    .foregroundStyle(Theme.muted)
                    .fixedSize()

                findStep("chevron.up", "Previous match") { step(-1) }
                findStep("chevron.down", "Next match") { step(1) }
                findStep("xmark.circle.fill", "Clear") { find = "" }
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 30)
        .background(Theme.column, in: RoundedRectangle(cornerRadius: 9))
        .overlay(RoundedRectangle(cornerRadius: 9).stroke(Theme.strokeStrong, lineWidth: 1))
    }

    /// Tapped images rather than buttons, for the same reason the columns use them: a `Button` in a
    /// bar like this one has repeatedly cost the list beside it its width.
    private func findStep(_ symbol: String, _ label: String, action: @escaping () -> Void) -> some View {
        Image(systemName: symbol)
            .font(.geist(11, .semibold))
            .foregroundStyle(matches.isEmpty && symbol != "xmark.circle.fill" ? Theme.rule : Theme.muted)
            .frame(width: 14, height: 14)
            .contentShape(Rectangle())
            .onTapGesture(perform: action)
            .help(label)
            .accessibilityLabel(label)
            .accessibilityAddTraits(.isButton)
    }

    private var composer: some View {
        VStack(spacing: 6) {
            if let action {
                MessageActionBanner(action: action, busy: sendPending) { inbox.cancelMessageAction(action.id) }
                    .frame(maxWidth: 860).frame(maxWidth: .infinity).padding(.horizontal, 24)
            }
            if !staged.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(staged) { file in
                            StagedFileChip(file: file) { if !sending.contains(composerKey) { staged.removeAll { $0.id == file.id } } }
                        }
                    }
                    .padding(.horizontal, 12)
                }
            }
            // Bottom-aligned, and that only works because the three share one height: the
            // paperclip and the send button are 34pt squares and a one-line field is 34pt too, so
            // they line up exactly on one line and the buttons stay on the last line as it grows.
            HStack(alignment: .bottom, spacing: 8) {
                Button { pick() } label: {
                    Image(systemName: "paperclip")
                        .font(.geist(16, .medium))
                        .frame(width: 34, height: 34)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.secondary)
                .help("Attach a file")
                .disabled(editing)

                MacMessageEditor(text: Binding(get: { draft }, set: { draft = $0 }), attach: { if !sendPending && !editing { staged.append(contentsOf: $0) } },
                                 reportError: { inbox.toast = $0 }, send: send, refocus: sent)
                    .overlay(alignment: .topLeading) {
                        if draft.isEmpty {
                            Text("Write a message")
                                .font(.geist(14.5))
                                .foregroundStyle(Theme.muted)
                                .padding(.horizontal, 5).padding(.vertical, 8)
                                .allowsHitTesting(false)
                        }
                    }

                Button(action: send) {
                    Image(systemName: "arrow.up")
                        .font(.geist(14, .bold))
                        .foregroundStyle(sendable ? Theme.onAccent : Theme.muted)
                        .frame(width: 34, height: 34)
                        .background(sendable ? Theme.accent : Theme.chip, in: RoundedRectangle(cornerRadius: 9))
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(!sendable)
                .help("Send")
                .accessibilityLabel(editing ? "Save edit" : "Send")
            }
            // One card around the three, as the conversation design draws its composer.
            .padding(6)
            .background(Theme.composer, in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.strokeStrong, lineWidth: 1))
            .frame(maxWidth: 860)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 24)
        }
        .padding(.top, 12)
        .padding(.bottom, 16)
        .background(Theme.wash)
        .overlay(alignment: .top) { Rectangle().fill(Theme.rule).frame(height: 1) }
        .disabled(sendPending)
    }

    private var sendable: Bool {
        !sendPending && (!draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || (!editing && !staged.isEmpty))
    }

    private static let floor = "thread-floor"

    private struct ScrollContext: Hashable {
        let mailbox: String?
        let peer: String
        let subject: String?
    }

    private struct ScrollRequest: Hashable {
        let context: ScrollContext
        let last: String?
        let count: Int
    }

    private func pick() {
        guard !editing else { return }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        guard panel.runModal() == .OK else { return }
        for url in panel.urls { stage(url) }
    }

    /// Read now rather than at send time — the same reason as on the phone: the permission to read
    /// a chosen file is scoped to the moment it was chosen.
    private func stage(_ url: URL) {
        guard !sendPending && !editing else { return }
        do { staged.append(try MacClipboardFiles.readFile(url)) }
        catch { inbox.toast = error.localizedDescription }
    }

    private func send() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard sendable else { return }
        let files = staged
        let key = composerKey
        sending.insert(key)
        if let action, editing {
            Task { @MainActor in
                defer { sending.remove(key) }
                if await inbox.saveMessageEdit(action), composerKey == key { sent += 1 }
            }
            return
        }
        let reply = action?.kind == .reply ? action : nil
        let old = drafts[key]?.attempt
        let same = old?.matches(text: text, fileIDs: files.map(\.id), replyTo: reply?.message.reference, editOf: nil) == true
        if let old, !same { inbox.abandonSend(reference: old.reference, mailbox: key.mailbox) }
        let attempt = same
            ? old! : MessageSendAttempt(text: text, fileIDs: files.map(\.id), replyTo: reply?.message.reference)
        drafts[key, default: ComposerDraft()].attempt = attempt
        // Whichever subject is on screen, including the single one a quiet peer has where no strip
        // is drawn. A reply that leaves the thread it answers is the only outcome nobody wants.
        let threadId = ConversationBuilder.targetThread(subthreads: subthreads, selected: subthread)
        Task {
            defer { sending.remove(key) }
            guard account.me?.address == key.mailbox else { return }
            if await inbox.send(text, to: peer, threadId: threadId, files: files, msgRef: attempt.reference, reply: reply) {
                drafts[key] = nil
                if composerKey == key { sent += 1 }
            }
        }
    }
}
