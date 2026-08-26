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

    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @State private var draft = ""
    @State private var staged: [StagedFile] = []
    @State private var dropping = false

    private var conversation: Conversation? { inbox.conversation(with: peer) }
    private var shown: [ThreadMessage] { conversation?.messages ?? [] }

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
                            MessageBubble(message: message)
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
                .onChange(of: shown.count) { _, _ in scroller.scrollTo(Self.floor, anchor: .bottom) }
                .onAppear { scroller.scrollTo(Self.floor, anchor: .bottom) }
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
        .task(id: peer) { await inbox.acknowledge(peer: peer, subthread: nil) }
        .navigationTitle(conversation?.name ?? peer)
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
            HStack(alignment: .bottom, spacing: 8) {
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
        Task { await inbox.send(text, to: peer, threadId: nil, files: files) }
    }
}
