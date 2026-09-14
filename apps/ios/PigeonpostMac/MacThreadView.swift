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
    @State private var draft = ""
    @State private var staged: [StagedFile] = []
    @State private var dropping = false
    /// What the toolbar's search field is looking for, inside this conversation.
    @State private var find = ""
    /// Which hit is being shown, as an index into `matches`.
    @State private var matchIndex = 0

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
                                highlight: find,
                                isFound: message.id == currentMatch
                            )
                                .id(message.id)
                        }
                        Color.clear.frame(height: 1).id(Self.floor)
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                }
                // Behind the messages and not behind the composer, which is where the phone puts it
                // too. The pattern is the paper a conversation is written on; the composer is a
                // control sitting on top of the paper, not part of it.
                .background { DoodleBackground() }
                // Stated as a property of the scroll view, not as an event. `onAppear` fires
                // before the scroll view has measured its content, which is why a long
                // conversation kept opening somewhere in the middle.
                // Explicitly, after the first layout. `onAppear` runs before the scroll view has
                // measured its content, which is what made this unreliable rather than wrong, and
                // the declarative anchor that replaces it on the phone is unusable here — see
                // `AnchoredToBottom`.
                .task(id: peer) {
                    await Task.yield()
                    scroller.scrollTo(Self.floor, anchor: .bottom)
                }
                // A message arriving, or being sent, belongs on screen.
                .onChange(of: shown.count) { _, _ in
                    scroller.scrollTo(Self.floor, anchor: .bottom)
                }
                // Changing subject is a different conversation as far as the reader is concerned,
                // and it should open where that one left off.
                .onChange(of: subthread) { _, _ in
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
            }
            Divider()
            composer
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

            if !find.trimmed.isEmpty {
                Text(matches.isEmpty
                     ? "none"
                     : "\(min(matchIndex, matches.count - 1) + 1) of \(matches.count)")
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
