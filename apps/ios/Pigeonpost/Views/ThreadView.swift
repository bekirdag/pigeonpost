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

    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(PushService.self) private var push

    private struct ComposerDraft {
        var text = ""
        var files: [StagedFile] = []
    }
    @State private var drafts: [HistoryKey: ComposerDraft] = [:]
    @State private var loading: [HistoryKey: Int] = [:]
    @State private var sending: Set<HistoryKey> = []
    private var composerKey: HistoryKey { HistoryKey(mailbox: account.me?.address, peer: peer, subthread: subthread) }
    private var draft: String {
        get { drafts[composerKey]?.text ?? "" }
        nonmutating set { drafts[composerKey, default: ComposerDraft()].text = newValue }
    }
    private var staged: [StagedFile] {
        get { drafts[composerKey]?.files ?? [] }
        nonmutating set { drafts[composerKey, default: ComposerDraft()].files = newValue }
    }
    private var loadingPhotos: Int { loading[composerKey, default: 0] }
    private var sendPending: Bool { sending.contains(composerKey) }

    @State private var subthread: String?
    /// One sheet at a time. Two stacked `.sheet` modifiers on one view are not reliably both
    /// honoured — see `ConversationsView`, where the same shape lost Settings entirely.
    @State private var sheet: Sheet?
    @State private var picking = false
    @State private var pickingPhotos = false
    /// What the photo picker last handed back. Emptied by `stage(_:)` as soon as the bytes are in
    /// `staged`, so this never holds a selection between one attachment and the next.
    @State private var photos: [PhotosPickerItem] = []
    private enum Sheet: String, Identifiable {
        case info, newThread
        var id: String { rawValue }
    }
    @State private var composing = false

    @State private var latestRequest = 0

    private var conversation: Conversation? { inbox.conversation(with: peer) }
    private var subthreads: [Subthread] { inbox.subthreads(of: peer) }

    private var shown: [ThreadMessage] {
        guard let conversation else { return [] }
        guard let subthread else { return conversation.messages }
        return conversation.messages.filter { ($0.threadId ?? "") == subthread }
    }

    var body: some View {
        ConversationHistoryView(messages: shown, latestRequest: latestRequest, account: account, inbox: inbox)
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
        .safeAreaInset(edge: .top, spacing: 0) { subjects }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            composer.modifier(ReportsItsPlace(measure: \.minY, report: LandingReport.composer))
        }
        .navigationTitle(conversation?.name ?? PeerFace.displayName(peer))
        .navigationBarTitleDisplayMode(.inline)
        // Glass rather than paint. This was opaque for a while, because the doodle showed through
        // the bar and ran under the title — but the answer to a pattern competing with a title is
        // to blur the pattern, not to hide it. A material does that and keeps the depth; painting
        // it flat threw the depth away to solve the legibility.
        .toolbarBackground(.ultraThinMaterial, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbar {
            // One button, not two. Archiving is a decision about this conversation and belongs
            // beside the other decisions about it — known, trusted, blocked — rather than sitting
            // in the bar as a thing to hit by accident on the way to reading.
            ToolbarItem(placement: .topBarTrailing) {
                Button { sheet = .info } label: { Image(systemName: "info.circle") }
                    .accessibilityLabel("About this sender")
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
            if subthread == nil { subthread = subthreads.first?.id }
            if Fixtures.sheet == "peer" { sheet = .info }
        }
    }

    /// Re-acknowledge when the subject changes or new mail lands, but not on every render.
    private var taskKey: String {
        "\(peer)|\(subthread ?? "")|\(conversation?.unread ?? 0)"
    }

    private var subjects: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(subthreads.count > 1 ? subthreads : []) { thread in
                    Button {
                        subthread = thread.id
                    } label: {
                        HStack(spacing: 5) {
                            Text(thread.name)
                                .font(.system(size: 13, weight: .medium))
                            if thread.unread > 0 {
                                Circle().fill(Theme.blue).frame(width: 6, height: 6)
                            }
                        }
                        .padding(.horizontal, 11)
                        .padding(.vertical, 6)
                        .background {
                            if thread.id == subthread {
                                Capsule().fill(Theme.navy)
                            } else {
                                Capsule().fill(.thinMaterial)
                            }
                        }
                        .foregroundStyle(thread.id == subthread ? Color.white : Theme.body)
                        .overlay(Capsule().stroke(Theme.rule, lineWidth: thread.id == subthread ? 0 : 1))
                    }
                    .buttonStyle(.plain)
                }
                Button { sheet = .newThread } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "plus")
                            .font(.system(size: 12, weight: .semibold))
                        // Named while it stands alone: a bare + above a conversation could mean
                        // anything. Once there are subjects beside it, the chips say what it adds.
                        if subthreads.count <= 1 {
                            Text("New thread").font(.system(size: 13, weight: .medium))
                        }
                    }
                    .padding(.horizontal, 11)
                    .padding(.vertical, 7)
                    .background(.thinMaterial, in: Capsule())
                    .overlay(Capsule().stroke(Theme.rule, lineWidth: 1))
                    .foregroundStyle(Theme.body)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("New thread")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
        .background(.ultraThinMaterial)
        .overlay(alignment: .bottom) { Divider().background(Theme.rule) }
    }

    private var composer: some View {
        VStack(spacing: 6) {
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
                    .padding(.horizontal, 12)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            composerRow
        }
        .padding(.vertical, 8)
        .background(.bar)
        .overlay(alignment: .top) { Divider().background(Theme.rule) }
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
                    .font(.system(size: 17))
                    .foregroundStyle(Theme.muted)
                    .frame(width: 32, height: 36)
                    .contentShape(Rectangle())
            }
            .accessibilityLabel("Attach")
            .disabled(sendPending)

            MessageEditor(text: Binding(get: { draft }, set: { draft = $0 }), focused: $composing,
                          enabled: !sendPending, pasteImages: pasteImages)
                .overlay(alignment: .topLeading) {
                    if draft.isEmpty {
                        Text("Write a message").font(.system(size: 15)).foregroundStyle(Theme.muted)
                            .padding(.horizontal, 13).padding(.vertical, 9).allowsHitTesting(false)
                    }
                }
                .background(Theme.wash, in: RoundedRectangle(cornerRadius: 18))
                .overlay(RoundedRectangle(cornerRadius: 18).stroke(Theme.rule, lineWidth: 1))
                .id(composerKey)

            Button(action: send) {
                Image(systemName: "paperplane.fill")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 36, height: 36)
                    .background(sendable ? Theme.navy : Theme.muted.opacity(0.4), in: Circle())
            }
            .disabled(!sendable)
            .accessibilityLabel("Send")
        }
        .padding(.horizontal, 12)
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
            if await inbox.send(text, to: peer, threadId: threadId, files: files) {
                drafts[key] = nil
                if composerKey == key { clearComposer() }
            }
        }
    }

    private func pasteImages(_ providers: [NSItemProvider]) {
        let key = composerKey
        guard !sendPending else { return }
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

    private func clearComposer() {
        latestRequest += 1
        draft = ""
    }
}
