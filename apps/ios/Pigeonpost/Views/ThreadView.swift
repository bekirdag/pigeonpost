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

    @State private var draft = ""

    /// Bumped by every send, to hand the composer a text field that has never held anything.
    ///
    /// Emptying `draft` is not enough, and the screenshot that finally showed this says so
    /// plainly: the Send button in it is drawn in `Theme.muted`, and muted is `sendable == false`,
    /// which is an empty `draft` and no staged files. The model had cleared. The field sitting
    /// above it was still showing every word of the message that had just gone.
    ///
    /// What is proven is that gap: state emptied, field not. The mechanism behind it is inference,
    /// and worth reading as one. The likeliest is UIKit's rule about marked text — a field holding
    /// it is mid-composition, and SwiftUI will not overwrite a composition in progress, because for
    /// two-stage input (Japanese, Chinese, dictation) that would destroy what somebody is halfway
    /// through typing. iOS 17's inline predictive text puts an ordinary English sentence into that
    /// same state on a device, for the word last typed, which is every send that ends in a word;
    /// the write would then land in `draft` and stop there. It also explains why this was read as
    /// fixed twice from the code and why a suite that types and sends passes: inline prediction is
    /// off in the simulator, so the field there has no composition to protect. But no device
    /// confirmed it — a phone with predictive text turned off still emptying the field would say
    /// the cause is something else.
    ///
    /// The fix does not rest on that being the right cause. Any state the old field is holding on
    /// to goes with the old field.
    ///
    /// A new identity is the one lever SwiftUI has that reaches a field's own state: the old one
    /// is torn down with whatever it was holding, and what replaces it reads a `draft` already
    /// empty.
    /// Focus is given back on the next turn so the keyboard does not leave between two messages.
    @State private var composerLife = 0

    @State private var subthread: String?
    /// One sheet at a time. Two stacked `.sheet` modifiers on one view are not reliably both
    /// honoured — see `ConversationsView`, where the same shape lost Settings entirely.
    @State private var sheet: Sheet?
    @State private var picking = false
    @State private var pickingPhotos = false
    /// What the photo picker last handed back. Emptied by `stage(_:)` as soon as the bytes are in
    /// `staged`, so this never holds a selection between one attachment and the next.
    @State private var photos: [PhotosPickerItem] = []
    @State private var staged: [StagedFile] = []
    /// Photos whose bytes are still on their way. A photo the picker names is not a photo the app
    /// holds: `loadTransferable` is a copy, and for a big one — or one that has to come down from
    /// iCloud first — it is a copy that takes long enough to press send in the middle of. See
    /// `send`.
    @State private var loadingPhotos = 0
    /// A send that is waiting for `loadingPhotos` to reach zero. It stops the send button offering
    /// to do the same thing twice while the first one waits.
    @State private var sendPending = false

    private enum Sheet: String, Identifiable {
        case info, newThread
        var id: String { rawValue }
    }
    @FocusState private var composing: Bool

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
                                staged.removeAll { $0.id == file.id }
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

            TextField("Write a message", text: $draft, axis: .vertical)
                .lineLimit(1...5)
                .font(.system(size: 15))
                .padding(.horizontal, 12)
                .padding(.vertical, 9)
                .background(Theme.wash, in: RoundedRectangle(cornerRadius: 18))
                .overlay(RoundedRectangle(cornerRadius: 18).stroke(Theme.rule, lineWidth: 1))
                .focused($composing)
                .id(composerLife)

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
        // Photos, not everything a library holds. A video is a file like any other and Files will
        // still hand one over; what it is not is something to let somebody attach by accident, since
        // a minute of 4K is a couple of hundred megabytes and the first this app would say about it
        // is the postbox refusing the upload.
        .photosPicker(
            isPresented: $pickingPhotos,
            selection: $photos,
            matching: .images,
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
        loadingPhotos += 1
        defer { loadingPhotos -= 1 }
        let now = Date()
        var failed = 0
        for (index, item) in chosen.enumerated() {
            guard let data = try? await item.loadTransferable(type: Data.self) else {
                failed += 1
                continue
            }
            let type = item.supportedContentTypes.first
            staged.append(StagedFile(
                name: PickedImage.filename(for: type, index: index, at: now),
                mediaType: PickedImage.mediaType(for: type),
                data: data
            ))
        }
        if failed > 0 {
            inbox.toast = failed == 1
                ? "Could not read that photo."
                : "Could not read \(failed) of those photos."
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
        guard let data = try? Data(contentsOf: url) else {
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

    /// Send what is in the composer, once all of it is actually there.
    ///
    /// The wait is for photos. `stage(_ chosen:)` is asynchronous because `loadTransferable` is, and
    /// between choosing a photo and its bytes arriving there is a window — a second or two on a big
    /// one, longer on one that has to come down from iCloud — in which the composer draws no chip
    /// for it. Pressing send inside that window used to take a copy of `staged` that the photo was
    /// not in yet: the message left without it, and the photo then appeared in the strip afterwards,
    /// attached to nothing, as though it had been left behind on purpose.
    ///
    /// So a send with a photo in flight is held until the reading finishes, and `sendPending` holds
    /// the button while it waits — otherwise a second press queues a second message and the first
    /// one to reach `staged` takes the files, leaving the other to send bare text.
    ///
    /// With nothing in flight this is what it always was: clear the composer, send, one turn, no
    /// window in which the same files could be taken twice.
    private func send() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard sendable else { return }
        // Whichever subject is on screen — including the one a peer with a single conversation has,
        // where no strip is drawn. Sending into the conversation you are reading is the only
        // behaviour that does not surprise: the alternative is a reply that leaves the thread it
        // answers.
        let threadId = ConversationBuilder.targetThread(subthreads: subthreads, selected: subthread)
        clearComposer()
        guard loadingPhotos > 0 else {
            let files = staged
            staged = []
            Task { await inbox.send(text, to: peer, threadId: threadId, files: files) }
            return
        }
        sendPending = true
        Task { @MainActor in
            while loadingPhotos > 0 {
                try? await Task.sleep(nanoseconds: 20_000_000)
            }
            let files = staged
            staged = []
            sendPending = false
            // Every photo it was waiting for failed to read, and nothing was typed. The toast
            // `stage` raised has already said so; an empty message would only say it again.
            guard !text.isEmpty || !files.isEmpty else { return }
            await inbox.send(text, to: peer, threadId: threadId, files: files)
        }
    }

    /// Empty the composer in both of the places it exists — the state, and the field drawing it.
    /// See `composerLife` for why the second one is not the first one said over again.
    ///
    /// Focus is dropped and taken back rather than left alone: the field that holds it is the old
    /// field, which is about to stop existing, and re-asserting a `@FocusState` that already reads
    /// `true` is not a change and so moves nothing. Off and on again is what puts the keyboard in
    /// front of the field that replaced it. If the keyboard was already down — a file sent with
    /// nothing typed — it stays down.
    private func clearComposer() {
        latestRequest += 1
        let keyboardWasUp = composing
        draft = ""
        composing = false
        composerLife &+= 1
        guard keyboardWasUp else { return }
        Task { @MainActor in composing = true }
    }
}
