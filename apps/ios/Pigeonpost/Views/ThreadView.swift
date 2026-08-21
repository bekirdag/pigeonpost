//  One conversation.
//
//  Where this diverges from the web app, deliberately: the web app gives a peer's several subjects a
//  pane of their own, and on a phone it stops there and waits to be told which. A phone screen that
//  is only ever a list of two or three names is a dead end, so the subjects are a strip along the
//  top of the thread instead — the same choice, made without leaving the conversation.

import SwiftUI

struct ThreadView: View {
    let peer: String

    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(PushService.self) private var push

    @State private var draft = ""
    @State private var subthread: String?
    @State private var showingInfo = false
    @State private var showingNewThread = false
    @FocusState private var composing: Bool

    private var conversation: Conversation? { inbox.conversation(with: peer) }
    private var subthreads: [Subthread] { inbox.subthreads(of: peer) }

    private var shown: [ThreadMessage] {
        guard let conversation else { return [] }
        guard let subthread else { return conversation.messages }
        return conversation.messages.filter { ($0.threadId ?? "") == subthread }
    }

    var body: some View {
        ScrollViewReader { scroller in
            ScrollView {
                LazyVStack(spacing: 2) {
                    ForEach(Array(shown.enumerated()), id: \.element.id) { index, message in
                        if index == 0 || !Time.sameDay(shown[index - 1].at, message.at) {
                            DayBreak(label: Time.dayLabel(message.at))
                        }
                        MessageBubble(message: message)
                            .id(message.id)
                    }
                    // The floor of the conversation, and a target that exists before the messages
                    // do. Scrolling to the last message means naming the thing that is moving;
                    // this stays put.
                    Color.clear
                        .frame(height: 1)
                        .id(Self.floor)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
            }
            .background { DoodleBackground() }
            // Open on the newest message, which is what a thread is for. Doing this in `onAppear`
            // was a guess at the timing — it runs before the scroll view has laid its content out,
            // so a long conversation opened at the top often enough to be a complaint. This is the
            // same intent stated as a property of the scroll view rather than as an event.
            .defaultScrollAnchor(.bottom)
            // Everything that can change what "the bottom" means, in one place.
            //
            // `defaultScrollAnchor(.bottom)` handles the first paint and nothing after it, and the
            // first paint is not the only moment this gets decided: the subject filter is applied
            // in `onAppear`, so the content changes once more immediately afterwards; mail lands
            // while the thread is open; and the keyboard takes half the screen without the scroll
            // view moving to compensate. Each of those left the conversation somewhere other than
            // its newest message, which is the complaint.
            .onChange(of: shown.count) { _, _ in scrollToFloor(scroller, animated: true) }
            .onChange(of: subthread) { _, _ in scrollToFloor(scroller, animated: false) }
            .onChange(of: composing) { _, focused in
                if focused { scrollToFloor(scroller, animated: true) }
            }
            .task(id: peer) {
                // After the first layout, not during it. `onAppear` runs before the scroll view has
                // measured its content, which is what made this unreliable rather than wrong.
                await Task.yield()
                scrollToFloor(scroller, animated: false)
            }

        }
        // Always, even for a peer with one conversation. The strip is where a second subject is
        // started, so hiding it until a second subject exists means there is no way to make one —
        // and the layout no longer changes shape underneath somebody the moment they do.
        .safeAreaInset(edge: .top, spacing: 0) { subjects }
        .safeAreaInset(edge: .bottom, spacing: 0) { composer }
        .navigationTitle(conversation?.name ?? PeerFace.displayName(peer))
        .navigationBarTitleDisplayMode(.inline)
        // Glass rather than paint. This was opaque for a while, because the doodle showed through
        // the bar and ran under the title — but the answer to a pattern competing with a title is
        // to blur the pattern, not to hide it. A material does that and keeps the depth; painting
        // it flat threw the depth away to solve the legibility.
        .toolbarBackground(.ultraThinMaterial, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Button {
                    Task { await inbox.setArchived(peer, archived: !inbox.archived.contains(peer)) }
                } label: {
                    Image(systemName: inbox.archived.contains(peer) ? "tray.and.arrow.up" : "archivebox")
                }
                .accessibilityLabel(inbox.archived.contains(peer) ? "Move back to the inbox" : "Archive this conversation")

                Button { showingInfo = true } label: { Image(systemName: "info.circle") }
                    .accessibilityLabel("About this sender")
            }
        }
        .sheet(isPresented: $showingInfo) {
            if let conversation {
                PeerInfoSheet(conversation: conversation) { mailbox in
                    showingInfo = false
                    account.act(as: mailbox)
                }
            }
        }
        .sheet(isPresented: $showingNewThread) {
            NewThreadSheet(peer: peer) { id in subthread = id }
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
            if Fixtures.sheet == "peer" { showingInfo = true }
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
                Button { showingNewThread = true } label: {
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
        HStack(alignment: .bottom, spacing: 8) {
            TextField("Write a message", text: $draft, axis: .vertical)
                .lineLimit(1...5)
                .font(.system(size: 15))
                .padding(.horizontal, 12)
                .padding(.vertical, 9)
                .background(Theme.wash, in: RoundedRectangle(cornerRadius: 18))
                .overlay(RoundedRectangle(cornerRadius: 18).stroke(Theme.rule, lineWidth: 1))
                .focused($composing)

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
        .padding(.vertical, 8)
        .background(.bar)
        .overlay(alignment: .top) { Divider().background(Theme.rule) }
    }

    private var sendable: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// The bottom, named once.
    private static let floor = "thread-floor"

    private func scrollToFloor(_ scroller: ScrollViewProxy, animated: Bool) {
        guard !shown.isEmpty else { return }
        if animated {
            withAnimation(.easeOut(duration: 0.2)) { scroller.scrollTo(Self.floor, anchor: .bottom) }
        } else {
            scroller.scrollTo(Self.floor, anchor: .bottom)
        }
    }

    private func send() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        draft = ""
        // Whichever subject is on screen — including the one a peer with a single conversation has,
        // where no strip is drawn. Sending into the conversation you are reading is the only
        // behaviour that does not surprise: the alternative is a reply that leaves the thread it
        // answers.
        let threadId = ConversationBuilder.targetThread(subthreads: subthreads, selected: subthread)
        Task { await inbox.send(text, to: peer, threadId: threadId) }
    }
}

private struct DayBreak: View {
    let label: String

    var body: some View {
        Text(label)
            .font(.system(size: 11.5))
            .foregroundStyle(Theme.muted)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(.thinMaterial, in: Capsule())
            .overlay(Capsule().stroke(Theme.rule, lineWidth: 1))
            .frame(maxWidth: .infinity)
            .padding(.vertical, 8)
    }
}
