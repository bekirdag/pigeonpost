//  One conversation, on a desktop.
//
//  The bubbles, the markdown and the attachment rows are the phone's — `MessageBubble` and its
//  neighbours are SwiftUI over shared models and need nothing platform-specific. What the Mac adds
//  is a composer that sends on Return, drag-and-drop onto the thread, and a window that can be made
//  as wide as the conversation deserves.

import SwiftUI
import UniformTypeIdentifiers
import AppKit

struct MacThreadView: View {
    let peer: String
    /// Which subject is being read, chosen in the sidebar. `nil` means the whole conversation.
    let subthread: String?

    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @State private var draft = ""
    @State private var staged: [StagedFile] = []
    @State private var dropping = false
    /// What the toolbar's search field is looking for, inside this conversation.
    @State private var find = ""
    /// The debounced query used for matching and highlighting. Typing remains a cheap field update;
    /// parsing every markdown block in a long conversation starts only after the user pauses.
    @State private var settledFind = ""
    /// Which hit is being shown, as an index into `matches`.
    @State private var matchIndex = 0
    @State private var floorY = CGFloat.infinity
    @State private var viewportHeight: CGFloat = 0
    @State private var positioned = false
    @State private var scrollEndRequest = 0
    @State private var nativeAtBottom = false

    private var conversation: Conversation? { inbox.conversation(with: peer) }
    private var subthreads: [Subthread] { inbox.subthreads(of: peer) }

    private var shown: [ThreadMessage] {
        guard let conversation else { return [] }
        return subthread.map { id in
            conversation.messages.filter { ($0.threadId ?? "") == id }
        } ?? conversation.messages
    }

    /// The messages the find bar has matched, in the order they appear.
    ///
    /// A find bar, not a filter. Filtering hides everything around a hit, which is most of what
    /// makes a hit worth finding — you search a conversation to read the part *near* the words, not
    /// to see the words alone.
    private var matchIDs: [String] {
        ConversationSearch.matchingMessageIDs(in: shown, query: settledFind)
    }

    private var currentMatch: String? {
        guard !matchIDs.isEmpty else { return nil }
        return matchIDs[min(matchIndex, matchIDs.count - 1)]
    }

    /// Step to the next hit, wrapping. Wrapping rather than stopping at the end, because a find bar
    /// that goes dead on the last match makes you retype the word to start again.
    private func step(_ by: Int) {
        guard !matchIDs.isEmpty else { return }
        matchIndex = (matchIndex + by + matchIDs.count) % matchIDs.count
    }

    private var isAtBottom: Bool {
        positioned && (nativeAtBottom || floorY <= viewportHeight + 36)
    }

    var body: some View {
        let activeMatch = currentMatch
        VStack(spacing: 0) {
            ScrollViewReader { scroller in
                ScrollView {
                    LazyVStack(spacing: 2) {
                        ForEach(Array(shown.enumerated()), id: \.element.id) { index, message in
                            if index == 0 || !Time.sameDay(shown[index - 1].at, message.at) {
                                Text(Time.dayLabel(message.at))
                                    .font(.system(size: 11, weight: .medium))
                                    .foregroundStyle(Theme.muted)
                                    .padding(.vertical, 6)
                            }
                            MessageBubble(
                                message: message,
                                highlight: settledFind,
                                isFound: message.id == activeMatch
                            )
                                .id(message.id)
                        }
                        MacScrollEndAnchor(request: scrollEndRequest, isAtEnd: $nativeAtBottom)
                            .frame(height: 1)
                            .id(Self.floor)
                            .background {
                                GeometryReader { geometry in
                                    Color.clear.preference(
                                        key: ThreadFloorPreferenceKey.self,
                                        value: geometry.frame(in: .named(Self.scrollSpace)).maxY
                                    )
                                }
                            }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                }
                // Behind the messages and not behind the composer, which is where the phone puts it
                // too. The pattern is the paper a conversation is written on; the composer is a
                // control sitting on top of the paper, not part of it.
                .background { DoodleBackground() }
                .coordinateSpace(name: Self.scrollSpace)
                .background {
                    GeometryReader { geometry in
                        Color.clear.preference(
                            key: ThreadViewportPreferenceKey.self,
                            value: geometry.size.height
                        )
                    }
                }
                .onPreferenceChange(ThreadFloorPreferenceKey.self) { floorY = $0 }
                .onPreferenceChange(ThreadViewportPreferenceKey.self) { viewportHeight = $0 }
                // Stated as a property of the scroll view, not as an event. `onAppear` fires
                // before the scroll view has measured its content, which is why a long
                // conversation kept opening somewhere in the middle.
                // Explicitly, after the first layout. `onAppear` runs before the scroll view has
                // measured its content, which is what made this unreliable rather than wrong, and
                // the declarative anchor that replaces it on the phone is unusable here — see
                // `AnchoredToBottom`.
                .task(id: scrollContext) {
                    positioned = false
                    find = ""
                    settledFind = ""
                    matchIndex = 0
                    await Task.yield()
                    scroller.scrollTo(Self.floor, anchor: .bottom)
                    // Lazy rows can finish measuring after the first scroll. Re-anchor once after
                    // that pass; otherwise a long thread can stop a little above its true floor,
                    // and the first composer wrap magnifies that gap into the old mid-thread jump.
                    try? await Task.sleep(nanoseconds: 80_000_000)
                    scrollEndRequest &+= 1
                    scroller.scrollTo(Self.floor, anchor: .bottom)
                    await Task.yield()
                    positioned = true
                }
                // Follow new messages only while the reader is already at the end, or when the new
                // row is their own optimistic send. Incoming mail must not throw somebody reading
                // history away from the older messages they deliberately scrolled to.
                .onChange(of: shown.count) { _, _ in
                    guard isAtBottom || shown.last?.kind == .outgoing else { return }
                    scrollToFloorAfterLayout(scroller)
                }
                // Growing the composer or adding its attachment strip reduces the viewport. Keep
                // the newest message visible only when the reader had already left the floor in
                // view; a draft must never move somebody who is reading history.
                .onChange(of: draft) { _, _ in
                    guard isAtBottom else { return }
                    scrollToFloorAfterLayout(scroller)
                }
                .onChange(of: staged.count) { _, _ in
                    guard isAtBottom else { return }
                    scrollToFloorAfterLayout(scroller)
                }
                .task(id: find) {
                    let query = ConversationSearch.query(find)
                    guard !query.isEmpty else {
                        settledFind = ""
                        matchIndex = 0
                        return
                    }
                    do { try await Task.sleep(nanoseconds: 180_000_000) }
                    catch { return }
                    guard !Task.isCancelled else { return }
                    settledFind = query
                    matchIndex = 0
                }
                .onChange(of: settledFind) { _, _ in
                    if let first = matchIDs.first {
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
            }
            Divider()
            composer
        }
        .background(Theme.wash)
        // A file dropped onto the conversation is the natural desktop gesture for sending one, and
        // it is the reason a Mac app beats the web app here at all.
        .onDrop(of: [.fileURL], isTargeted: $dropping) { providers in
            var accepted = false
            for provider in providers {
                guard provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) else {
                    continue
                }
                accepted = true
                provider.loadDataRepresentation(forTypeIdentifier: UTType.fileURL.identifier) { data, _ in
                    guard let data, let url = DroppedFile.url(from: data) else { return }
                    Task { @MainActor in stage(url) }
                }
            }
            return accepted
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
        }
        .task(id: taskKey) { await inbox.acknowledge(peer: peer, subthread: subthread) }

    }

    /// Re-acknowledge when the subject changes or new mail lands, but not on every render.
    private var taskKey: String { "\(peer)|\(subthread ?? "")|\(conversation?.unread ?? 0)" }

    /// Find in this conversation: a field, the tally, and the two ways through it.
    private var findBar: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 11))
                .foregroundStyle(Theme.muted)
                .fixedSize()

            TextField("Search this conversation", text: $find)
                .textFieldStyle(.plain)
                .font(.system(size: 12))
                .foregroundStyle(Theme.ink)
                .frame(width: 190)
                .onExitCommand { find = "" }

            if !ConversationSearch.query(find).isEmpty {
                Text(matchIDs.isEmpty
                     ? "none"
                     : "\(min(matchIndex, matchIDs.count - 1) + 1) of \(matchIDs.count)")
                    .font(.system(size: 11).monospacedDigit())
                    .foregroundStyle(Theme.muted)
                    .fixedSize()

                findStep("chevron.up", "Previous match") { step(-1) }
                findStep("chevron.down", "Next match") { step(1) }
                findStep("xmark.circle.fill", "Clear") { find = "" }
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(Theme.ground, in: RoundedRectangle(cornerRadius: 7))
        .overlay(RoundedRectangle(cornerRadius: 7).stroke(Theme.rule, lineWidth: 1))
    }

    /// Tapped images rather than buttons, for the same reason the columns use them: a `Button` in a
    /// bar like this one has repeatedly cost the list beside it its width.
    private func findStep(_ symbol: String, _ label: String, action: @escaping () -> Void) -> some View {
        Image(systemName: symbol)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(matchIDs.isEmpty && symbol != "xmark.circle.fill" ? Theme.rule : Theme.muted)
            .frame(width: 14, height: 14)
            .contentShape(Rectangle())
            .onTapGesture(perform: action)
            .help(label)
            .accessibilityLabel(label)
            .accessibilityAddTraits(.isButton)
    }

    private var composer: some View {
        VStack(spacing: 6) {
            if !staged.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(staged) { file in
                            StagedFileChip(file: file) { staged.removeAll { $0.id == file.id } }
                        }
                    }
                    .padding(.horizontal, 12)
                }
            }
            // Centred on the field.
            //
            // `.bottom` lines up the *frames*, and those are not comparable: a button's frame
            // carries its own hit-target padding, the field's carries 7pt of text inset. The
            // paperclip ended up floating above the words it sits beside. `.lastTextBaseline` put
            // it further out still, because a padded field's baseline is not where a bare glyph's
            // is. Centre is the one alignment that means the same thing for both, and for a field
            // that is one line almost always, it is also what it should look like.
            HStack(alignment: .center, spacing: 8) {
                Button { pick() } label: { Image(systemName: "paperclip") }
                    .buttonStyle(.plain)
                    .foregroundStyle(Theme.muted)
                    .help("Attach a file")

                TextField("Write a message", text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...8)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 7)
                    .background(Theme.ground, in: RoundedRectangle(cornerRadius: 8))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.rule, lineWidth: 1))
                    // Return sends, which is what every desktop messenger does; a newline needs
                    // Shift, which SwiftUI gives for free on a vertical-axis field.
                    .onSubmit(send)

                Button(action: send) { Image(systemName: "paperplane.fill") }
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(!sendable)
                    .help("Send")
            }
            .padding(.horizontal, 12)
        }
        .padding(.vertical, 8)
        .background(.bar)
    }

    private var sendable: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !staged.isEmpty
    }

    private static let floor = "thread-floor"
    private static let scrollSpace = "thread-scroll-space"

    private var scrollContext: String { "\(peer)|\(subthread ?? "")" }

    private func scrollToFloor(_ scroller: ScrollViewProxy) {
        scroller.scrollTo(Self.floor, anchor: .bottom)
    }

    /// Composer wrapping, attachment chips, and a newly inserted bubble all change the scroll
    /// view's measured height. Scrolling in the same update targets the old geometry and leaves the
    /// new bottom below the viewport; waiting one layout turn makes the anchor deterministic.
    private func scrollToFloorAfterLayout(_ scroller: ScrollViewProxy) {
        Task { @MainActor in
            // A paste can deliver many draft mutations before AppKit commits even one new field
            // height. A short coalescing delay lands after that batch; a single yield can still run
            // between the text update and its layout pass.
            try? await Task.sleep(nanoseconds: 50_000_000)
            scrollEndRequest &+= 1
            scrollToFloor(scroller)
        }
    }

    private func pick() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        guard panel.runModal() == .OK else { return }
        for url in panel.urls { stage(url) }
    }

    /// Read now rather than at send time — the same reason as on the phone: the permission to read
    /// a chosen file is scoped to the moment it was chosen.
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

    private func send() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard sendable else { return }
        let files = staged
        staged = []
        draft = ""
        // Whichever subject is on screen, including the single one a quiet peer has where no strip
        // is drawn. A reply that leaves the thread it answers is the only outcome nobody wants.
        let threadId = ConversationBuilder.targetThread(subthreads: subthreads, selected: subthread)
        Task { await inbox.send(text, to: peer, threadId: threadId, files: files) }
    }
}

private struct ThreadFloorPreferenceKey: PreferenceKey {
    static var defaultValue = CGFloat.infinity
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = nextValue() }
}

private struct ThreadViewportPreferenceKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = nextValue() }
}

/// `ScrollViewReader.scrollTo` is visibility-based on macOS 14: once its target is barely visible,
/// another call can be a no-op even though the document still has room below it. This sentinel is
/// part of that same document view and can therefore ask the native scroll view for its exact end.
private struct MacScrollEndAnchor: NSViewRepresentable {
    let request: Int
    @Binding var isAtEnd: Bool

    final class Coordinator {
        var handledRequest = Int.min
        weak var clipView: NSClipView?
        weak var documentView: NSView?
        var boundsObserver: NSObjectProtocol?
        var report: ((Bool) -> Void)?

        deinit {
            if let boundsObserver { NotificationCenter.default.removeObserver(boundsObserver) }
        }

        func connect(scrollView: NSScrollView) {
            let clip = scrollView.contentView
            guard clipView !== clip else { return }
            if let boundsObserver { NotificationCenter.default.removeObserver(boundsObserver) }
            clipView = clip
            documentView = scrollView.documentView
            clip.postsBoundsChangedNotifications = true
            boundsObserver = NotificationCenter.default.addObserver(
                forName: NSView.boundsDidChangeNotification,
                object: clip,
                queue: .main
            ) { [weak self] _ in self?.reportPosition() }
        }

        func reportPosition() {
            guard let clipView, let documentView else { return }
            let endY = documentView.isFlipped
                ? max(documentView.bounds.minY, documentView.bounds.maxY - clipView.bounds.height)
                : documentView.bounds.minY
            report?(abs(clipView.bounds.origin.y - endY) <= 36)
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSView {
        let view = NSView(frame: .zero)
        view.setAccessibilityElement(false)
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        context.coordinator.report = { atEnd in
            if isAtEnd != atEnd { isAtEnd = atEnd }
        }
        DispatchQueue.main.async {
            guard let scrollView = view.enclosingScrollView,
                  let document = scrollView.documentView else { return }
            context.coordinator.connect(scrollView: scrollView)
            guard context.coordinator.handledRequest != request else {
                context.coordinator.reportPosition()
                return
            }
            context.coordinator.handledRequest = request
            let clipView = scrollView.contentView
            let endY = document.isFlipped
                ? max(document.bounds.minY, document.bounds.maxY - clipView.bounds.height)
                : document.bounds.minY
            clipView.scroll(to: NSPoint(x: clipView.bounds.origin.x, y: endY))
            scrollView.reflectScrolledClipView(clipView)
            context.coordinator.reportPosition()
        }
    }
}
