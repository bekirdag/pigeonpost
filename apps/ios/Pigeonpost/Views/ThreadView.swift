//  One conversation.
//
//  Where this diverges from the web app, deliberately: the web app gives a peer's several subjects a
//  pane of their own, and on a phone it stops there and waits to be told which. A phone screen that
//  is only ever a list of two or three names is a dead end, so the subjects are a strip along the
//  top of the thread instead — the same choice, made without leaving the conversation.

import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

struct ThreadView: View {
    let peer: String
    /// The subject, when somebody else chooses it. The desktop layout an iPad uses gives subjects a
    /// column of their own and passes its selection in here; the thread then draws neither its own
    /// strip of subjects nor its own title, since both are already on screen beside it. `nil` — a
    /// phone — keeps the strip, and the choice, inside the thread.
    private let chosenSubthread: Binding<String?>?

    init(peer: String, subthread: Binding<String?>? = nil) {
        self.peer = peer
        self.chosenSubthread = subthread
    }

    private var embedded: Bool { chosenSubthread != nil }

    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(PeopleStore.self) private var people: PeopleStore?
    @Environment(PushService.self) private var push
    @Environment(CallLog.self) private var callLog: CallLog?

    private struct ComposerDraft {
        var text = ""
        var files: [StagedFile] = []
        var attempt: MessageSendAttempt?
        var firstReference: String?
    }
    @State private var drafts: [HistoryKey: ComposerDraft] = [:]
    @State private var loading: [HistoryKey: Int] = [:]
    @State private var sending: Set<HistoryKey> = []
    private var composerKey: HistoryKey { HistoryKey(mailbox: account.me?.address, peer: peer, subthread: subthread) }
    private var actionScope: MessageActionScope { MessageActionScope(mailbox: account.me?.address, peer: peer, subject: subthread) }
    private var action: MessageComposerAction? { inbox.action(in: actionScope) }
    private var editing: Bool { action?.kind == .edit }
    private struct InputContext: Hashable { let key: HistoryKey; let edit: UUID? }
    private var inputContext: InputContext { InputContext(key: composerKey, edit: editing ? action?.id : nil) }
    private func inputText(_ context: InputContext) -> Binding<String> {
        Binding(get: { inputContext == context ? draft : "" }, set: { value in
            guard inputContext == context, !sendPending else { return }
            if let edit = context.edit { inbox.updateMessageAction(edit, text: value) }
            else { drafts[context.key, default: ComposerDraft()].text = value }
        })
    }
    private func inputFocus(_ context: InputContext) -> Binding<Bool> {
        Binding(get: { inputContext == context && composing }, set: { value in
            if inputContext == context { composing = value }
        })
    }
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
    private var loadingPhotos: Int { loading[composerKey, default: 0] }
    private var sendPending: Bool { sending.contains(composerKey) || (editing && inbox.actionSending) }

    @State private var ownSubthread: String?
    private var subthread: String? {
        get { chosenSubthread?.wrappedValue ?? ownSubthread }
        nonmutating set {
            if let chosenSubthread { chosenSubthread.wrappedValue = newValue } else { ownSubthread = newValue }
        }
    }
    /// One sheet at a time. Two stacked `.sheet` modifiers on one view are not reliably both
    /// honoured — see `ConversationsView`, where the same shape lost Settings entirely.
    @State private var sheet: Sheet?
    @State private var picking = false
    @State private var pickingPhotos = false
    /// What the photo picker last handed back. Emptied by `stage(_:)` as soon as the bytes are in
    /// `staged`, so this never holds a selection between one attachment and the next.
    @State private var photos: [PhotosPickerItem] = []
    private enum Sheet: Identifiable {
        case info, newThread
        case block(BlockTarget)
        var id: String {
            switch self {
            case .info: return "info"
            case .newThread: return "newThread"
            case .block(let target): return "block:" + target.id
            }
        }
    }
    @State private var composing = false

    @State private var latestRequest = 0

    private var conversation: Conversation? { inbox.conversation(with: peer) }
    private var subthreads: [Subthread] { inbox.subthreads(of: peer) }

    private var shown: [ThreadMessage] {
        guard let conversation else { return [] }
        // Missed calls are lines in the conversation as a whole and in its General subject — a call
        // is not about any one of the others.
        let general = subthread.map { id in subthreads.first { $0.id == id }?.isDefault ?? true } ?? true
        let lines = general ? (callLog?.lines(for: conversation) ?? []) : []
        guard let subthread else { return CallLogEntry.merge(conversation.messages, lines) }
        return CallLogEntry.merge(conversation.messages.filter { ($0.threadId ?? "") == subthread }, lines)
    }

    private var conversationTitle: String {
        if embedded { return "" }
        if let name = people?.displayName(peer, legacyName: conversation?.peopleLegacyName) { return name }
        return conversation?.name ?? PeerFace.displayName(peer)
    }

    var body: some View {
        ConversationHistoryView(messages: shown, latestRequest: latestRequest, account: account, inbox: inbox, actionScope: actionScope)
            .id(HistoryKey(mailbox: account.me?.address, peer: peer, subthread: subthread))
            .background { DoodleBackground() }
            .onChange(of: composing) { _, focused in
                if focused { latestRequest += 1 }
            }
            .overlay(alignment: .topTrailing) {
                #if DEBUG
                if Fixtures.enabled && CommandLine.arguments.contains("-ios-history") {
                    Button("Receive fixture message") { IOSUXFixtures.receive(into: inbox) }
                        .accessibilityIdentifier("history-arrival")
                }
                #endif
            }
        // Always, even for a peer with one conversation. The strip is where a second subject is
        // started, so hiding it until a second subject exists means there is no way to make one —
        // and the layout no longer changes shape underneath somebody the moment they do.
        .safeAreaInset(edge: .top, spacing: 0) { if !embedded { subjects } }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            // A blocked conversation says so where the composer would be, with the way back.
            if let conversation, conversation.isBlocked {
                BlockedBanner(conversation: conversation)
            } else {
                composer.modifier(ReportsItsPlace(measure: \.minY, report: LandingReport.composer))
            }
        }
        .navigationTitle(conversationTitle)
        .navigationBarTitleDisplayMode(.inline)
        // Glass rather than paint. This was opaque for a while, because the doodle showed through
        // the bar and ran under the title — but the answer to a pattern competing with a title is
        // to blur the pattern, not to hide it. A material does that and keeps the depth; painting
        // it flat threw the depth away to solve the legibility.
        // Painted in the conversation's own ground, with the hairline under it, as the
        // conversation design draws its header. The doodle is veiled almost to that same colour, so
        // the bar no longer has a busy pattern to hide.
        .toolbarBackground(Theme.wash, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbar {
            // One item: an empty badge in an item of its own still takes a slot, and on an iPad's
            // narrow detail column that pushed the bar into the system's overflow menu.
            ToolbarItem(placement: .topBarTrailing) {
                HStack(spacing: 8) {
                    DirectTimerBadge(peer: peer)
                    CallButtons(peer: peer)
                }
            }
            // One button, not two. Archiving is a decision about this conversation and belongs
            // beside the other decisions about it — known, trusted, blocked — rather than sitting
            // in the bar as a thing to hit by accident on the way to reading.
            // Embedded, the sender's name in the bar is already the way to this panel.
            // The sender panel, and blocking, behind one button: a third bar item is what iOS folds
            // into an overflow of its own (see `ConversationsView`). Embedded, the sender's name in
            // the bar is already the way to the panel, so only Block is here.
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    if !embedded {
                        Button { sheet = .info } label: { Label("About this sender", systemImage: "info.circle") }
                    }
                    if let conversation, !conversation.isBlocked {
                        Button(role: .destructive) {
                            sheet = .block(BlockTarget(peer: peer, name: conversation.name, origin: .thread))
                        } label: {
                            Label("Block \(conversation.name)…", systemImage: "hand.raised")
                        }
                    }
                } label: {
                    Image(systemName: embedded ? "ellipsis.circle" : "info.circle")
                }
                .accessibilityLabel(embedded ? "More" : "About this sender")
                .accessibilityIdentifier("threadMenu")
            }
        }
        .sheet(item: $sheet) { which in
            switch which {
            case .info:
                if let conversation {
                    PeerInfoSheet(conversation: conversation) { mailbox in
                        sheet = nil
                        account.act(as: mailbox)
                    }
                }
            case .newThread:
                NewThreadSheet(peer: peer) { id in subthread = id }
            case .block(let target):
                BlockSheet(target: target)
            }
        }
        .task(id: taskKey) {
            await inbox.acknowledge(peer: peer, subthread: subthread)
            // Ask here, not at launch. By the time somebody has opened a conversation they know
            // what the app is for, which is the only moment the question has an honest answer —
            // and a permission declined in front of a sign-in screen is expensive to win back.
            guard !Fixtures.enabled else { return }
            await push.askIfNeeded()
        }
        .onAppear {
            // Embedded, nothing chosen means the whole conversation, as it does on the Mac.
            if !embedded, subthread == nil { subthread = subthreads.first?.id }
            if Fixtures.sheet == "peer" { sheet = .info }
            if Fixtures.sheet == "block", let conversation {
                sheet = .block(BlockTarget(peer: peer, name: conversation.name, origin: .thread))
            }
            if Fixtures.sheet == "thread", !embedded { sheet = .newThread }
            adoptStartedConversation()
        }
        .onChange(of: inbox.started) { _, _ in adoptStartedConversation() }
        .onChange(of: inbox.unconfirmedFirstSends) { _, _ in adoptStartedConversation() }
        .onChange(of: inbox.resolvedFirstSends) { _, _ in retireResolvedFirstSends() }
        .onChange(of: action?.id) { _, next in if next != nil { composing = true } }
    }

    /// Re-acknowledge when the subject changes or new mail lands, but not on every render.
    private var taskKey: String {
        "\(peer)|\(subthread ?? "")|\(conversation?.unread ?? 0)"
    }

    private var subjects: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                ForEach(subthreads.count > 1 ? subthreads : []) { thread in
                    Button {
                        subthread = thread.id
                    } label: {
                        HStack(spacing: 6) {
                            Text(thread.name)
                                .font(.geist(13, .medium))
                            if thread.unread > 0 {
                                Circle().fill(Theme.accent).frame(width: 6, height: 6)
                            }
                        }
                        .padding(.horizontal, 10)
                        .frame(height: 30)
                        .background(thread.id == subthread ? Theme.chip : Color.clear,
                                    in: RoundedRectangle(cornerRadius: 7))
                        .foregroundStyle(thread.id == subthread ? Theme.ink : Theme.secondary)
                    }
                    .buttonStyle(.plain)
                }
                Button { sheet = .newThread } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "plus")
                            .font(.geist(12, .semibold))
                        // Named while it stands alone: a bare + above a conversation could mean
                        // anything. Once there are subjects beside it, the chips say what it adds.
                        if subthreads.count <= 1 {
                            Text("New thread").font(.geist(13, .medium))
                        }
                    }
                    .padding(.horizontal, 10)
                    .frame(height: 30)
                    .background(Theme.field, in: RoundedRectangle(cornerRadius: 7))
                    .overlay(RoundedRectangle(cornerRadius: 7).stroke(Theme.strokeStrong, lineWidth: 1))
                    .foregroundStyle(Theme.ink)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("New thread")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
        .background(Theme.wash)
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.rule).frame(height: 1) }
    }

    private var composer: some View {
        VStack(spacing: 6) {
            if let action {
                MessageActionBanner(action: action, busy: sendPending) { inbox.cancelMessageAction(action.id) }
                    .frame(maxWidth: 860).frame(maxWidth: .infinity)
                    .padding(.horizontal, embedded ? 24 : 12)
            }
            // Chosen but not yet sent, listed above the field. What is about to leave a mailbox
            // should be readable before it does, not hidden behind a count.
            if !staged.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(staged) { file in
                            StagedFileChip(file: file) {
                                if !sendPending { staged.removeAll { $0.id == file.id } }
                            }
                        }
                    }
                }
                // Lined up with the composer card under it — the same width limit and the same
                // inset — rather than 12 pt from the column's edge whatever the card did.
                .frame(maxWidth: 860, alignment: .leading)
                .frame(maxWidth: .infinity)
                .padding(.horizontal, embedded ? 24 : 12)
            }
            composerRow
        }
        .padding(.top, 10)
        .padding(.bottom, 10)
        .background(Theme.wash)
        .overlay(alignment: .top) { Rectangle().fill(Theme.rule).frame(height: 1) }
    }

    private var composerRow: some View {
        HStack(alignment: .bottom, spacing: 8) {
            // Two places a phone keeps things, and they are not the same place. Everything a person
            // photographs is in the library and almost nothing else is; Files is where a document
            // that arrived from somewhere else lives. The paperclip used to open only the second,
            // which meant sending a photo was a trip through Files > Browse > Photos, on the chance
            // somebody knew it was there at all.
            Menu {
                Button {
                    pickingPhotos = true
                } label: {
                    Label("Photo Library", systemImage: "photo.on.rectangle")
                }
                Button {
                    picking = true
                } label: {
                    Label("Files", systemImage: "folder")
                }
            } label: {
                Image(systemName: "paperclip")
                    .font(.geist(17, .medium))
                    .foregroundStyle(Theme.secondary)
                    .frame(width: 34, height: 34)
                    .contentShape(Rectangle())
            }
            .accessibilityLabel("Attach")
            .disabled(sendPending || editing)

            let context = inputContext
            MessageEditor(text: inputText(context), focused: inputFocus(context),
                          enabled: !sendPending, pasteImages: { providers in
                              if inputContext == context && !editing { pasteImages(providers) }
                          })
                .overlay(alignment: .topLeading) {
                    if draft.isEmpty {
                        Text("Write a message").font(.geist(15)).foregroundStyle(Theme.muted)
                            .padding(.horizontal, 8).padding(.vertical, 8).allowsHitTesting(false)
                    }
                }
                .id(context)

            Button(action: send) {
                Image(systemName: "arrow.up")
                    .font(.geist(15, .bold))
                    .foregroundStyle(sendable ? Theme.onAccent : Theme.muted)
                    .frame(width: 34, height: 34)
                    .background(sendable ? Theme.accent : Theme.chip, in: RoundedRectangle(cornerRadius: 9))
            }
            .disabled(!sendable)
            .accessibilityLabel(editing ? "Save edit" : "Send")
        }
        // One card around the three, as the conversation design draws its composer.
        .padding(6)
        .background(Theme.composer, in: RoundedRectangle(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.strokeStrong, lineWidth: 1))
        .frame(maxWidth: 860)
        .frame(maxWidth: .infinity)
        .padding(.horizontal, embedded ? 24 : 12)
        // Any file the system can hand over. A file is anything really — a photo, a zip, a PDF —
        // and narrowing the list here would only mean somebody cannot send the thing they have.
        .fileImporter(
            isPresented: $picking,
            allowedContentTypes: [.item],
            allowsMultipleSelection: true
        ) { result in
            guard case let .success(urls) = result else { return }
            for url in urls { stage(url) }
        }
        .photosPicker(
            isPresented: $pickingPhotos,
            selection: $photos,
            maxSelectionCount: 5,
            matching: .any(of: [.images, .videos]),
            photoLibrary: .shared()
        )
        .onChange(of: photos) { _, chosen in
            guard !chosen.isEmpty else { return }
            Task { await stage(chosen) }
        }
    }

    /// Read the chosen photos now, for the same reason a file is read now: the picker's hold on them
    /// is scoped to this moment.
    ///
    /// One at a time and in the order they were chosen, so the staged strip reads the way the
    /// selection did. `loadTransferable` is the whole of the transfer — the picker runs out of
    /// process and hands back bytes, which is why this needs no photo-library permission and why a
    /// failure here is a failure to copy rather than a failure to be allowed.
    @MainActor
    private func stage(_ chosen: [PhotosPickerItem]) async {
        let key = composerKey
        loading[key, default: 0] += 1
        defer { loading[key, default: 1] -= 1 }
        let now = Date()
        var failed = 0
        for (index, item) in chosen.enumerated() {
            guard let data = try? await item.loadTransferable(type: Data.self) else {
                failed += 1
                continue
            }
            let type = item.supportedContentTypes.first
            guard account.me?.address == key.mailbox else { return }
            guard data.count <= AttachmentThumbnails.maxBytes, drafts[key, default: ComposerDraft()].files.count < 5 else {
                failed += 1
                continue
            }
            drafts[key, default: ComposerDraft()].files.append(StagedFile(
                name: PickedImage.filename(for: type, index: index, at: now),
                mediaType: PickedImage.mediaType(for: type),
                data: data
            ))
        }
        if failed > 0 {
            inbox.toast = failed == 1
                ? "Could not attach that photo or video. Files must be at most 25 MB; attach up to 5 per message."
                : "Could not attach \(failed) items. Files must be at most 25 MB; attach up to 5 per message."
        }
        // Emptied so the same photo can be chosen again after it has been removed from the strip.
        // The guard in `onChange` is what stops this coming straight back round.
        photos = []
    }

    /// Read now, not at send time. The picker hands back a URL into another process's sandbox and
    /// permission to read it is scoped to this moment — holding the URL and opening it later is how
    /// a file becomes unreadable exactly when somebody presses send.
    private func stage(_ url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        guard !sendPending, staged.count < 5,
              let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize,
              size <= AttachmentThumbnails.maxBytes else {
            inbox.toast = "Attach up to 5 files, at most 25 MB each."
            return
        }
        guard let data = try? Data(contentsOf: url), data.count <= AttachmentThumbnails.maxBytes else {
            inbox.toast = "Could not read that file."
            return
        }
        staged.append(StagedFile(
            name: url.lastPathComponent,
            mediaType: UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
                ?? "application/octet-stream",
            data: data
        ))
    }

    /// A message needs words or a file. Sending a file with nothing typed is an ordinary thing.
    private var sendable: Bool {
        guard !sendPending else { return false }
        if editing { return !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        return !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !staged.isEmpty
            // A photo still being read is something to send. Without this the button goes dead
            // between choosing a photo and its chip appearing, which on an iCloud photo is seconds.
            || loadingPhotos > 0
    }

    private struct HistoryKey: Hashable {
        let mailbox: String?
        let peer: String
        let subthread: String?
    }

    /// Keep the complete draft until the server accepts the message. Drafts and asynchronous
    /// clipboard/photo reads belong to a mailbox and subject, including while navigating away.
    private func send() {
        guard sendable else { return }
        let key = composerKey
        if let action, editing {
            sending.insert(key)
            Task { @MainActor in
                defer { sending.remove(key) }
                _ = await inbox.saveMessageEdit(action)
            }
            return
        }
        let reply = action?.kind == .reply ? action : nil
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let threadId = ConversationBuilder.targetThread(subthreads: subthreads, selected: subthread)
        sending.insert(key)
        Task { @MainActor in
            defer { sending.remove(key) }
            while loading[key, default: 0] > 0 {
                try? await Task.sleep(nanoseconds: 50_000_000)
            }
            guard account.me?.address == key.mailbox else { return }
            let files = drafts[key]?.files ?? []
            guard !text.isEmpty || !files.isEmpty else { return }
            let old = drafts[key]?.attempt
            let same = old?.matches(text: text, fileIDs: files.map(\.id), replyTo: reply?.message.reference, editOf: nil) == true
            if let old, !same { inbox.abandonSend(reference: old.reference, mailbox: key.mailbox) }
            let attempt = same
                ? old! : MessageSendAttempt(text: text, fileIDs: files.map(\.id), replyTo: reply?.message.reference)
            drafts[key, default: ComposerDraft()].attempt = attempt
            let retryThread = drafts[key]?.firstReference == attempt.reference ? nil : threadId
            if await inbox.send(text, to: peer, threadId: retryThread, files: files, msgRef: attempt.reference, reply: reply) {
                drafts[key] = nil
                if composerKey == key && !editing { clearComposer() }
            }
        }
    }

    private func pasteImages(_ providers: [NSItemProvider]) {
        let key = composerKey
        guard !sendPending && !editing else { return }
        loading[key, default: 0] += 1
        Task { @MainActor in
            defer { loading[key, default: 1] -= 1 }
            for provider in providers.prefix(5) {
                guard let identifier = provider.registeredTypeIdentifiers.first(where: { UTType($0)?.conforms(to: .image) == true }) else { continue }
                let data: Data? = await withCheckedContinuation { continuation in
                    provider.loadDataRepresentation(forTypeIdentifier: identifier) { data, _ in continuation.resume(returning: data) }
                }
                guard account.me?.address == key.mailbox else { return }
                guard let data, !data.isEmpty, data.count <= AttachmentThumbnails.maxBytes,
                      drafts[key, default: ComposerDraft()].files.count < 5 else {
                    inbox.toast = "Could not paste that image. Attach up to 5 files, at most 25 MB each."
                    continue
                }
                let type = UTType(identifier)
                let ext = type?.preferredFilenameExtension ?? "png"
                drafts[key, default: ComposerDraft()].files.append(StagedFile(
                    name: "Pasted image-\(UUID().uuidString.prefix(8)).\(ext)",
                    mediaType: type?.preferredMIMEType ?? "image/png", data: data))
            }
        }
    }

    /// A conversation just started from New conversation: the composer takes focus, and a first
    /// message whose delivery was not confirmed comes back into it under its own reference, so Send
    /// retries that message (Discard is on the message itself).
    private func retireResolvedFirstSends() {
        for (key, value) in drafts {
            guard let reference = value.firstReference, inbox.resolvedFirstSends.contains(reference) else { continue }
            if value.attempt?.reference == reference && value.attempt?.text == value.text && value.files.isEmpty {
                drafts[key] = nil
            } else {
                // Keep anything the person has written or attached since adopting the retry.
                drafts[key]?.firstReference = nil
                if value.attempt?.reference == reference { drafts[key]?.attempt = nil }
            }
        }
    }

    private func adoptStartedConversation() {
        retireResolvedFirstSends()
        let key = composerKey
        let empty = (drafts[key]?.text ?? "").isEmpty && (drafts[key]?.files ?? []).isEmpty
            && action == nil && !sendPending && loadingPhotos == 0
        if let unconfirmed = inbox.takeUnconfirmedFirstSend(mailbox: key.mailbox, peer: peer, intoEmptyComposer: empty) {
            drafts[key, default: ComposerDraft()].text = unconfirmed.text
            drafts[key, default: ComposerDraft()].firstReference = unconfirmed.reference
            drafts[key, default: ComposerDraft()].attempt = MessageSendAttempt(
                text: unconfirmed.text, fileIDs: [], reference: unconfirmed.reference)
        }
        if inbox.takeStartedFocus(mailbox: key.mailbox, peer: peer) { composing = true }
    }

    private func clearComposer() {
        latestRequest += 1
        draft = ""
        // Straight on to the next message, as on the web: the field keeps the keyboard.
        composing = true
    }
}
