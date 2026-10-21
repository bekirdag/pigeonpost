//  One group: its encrypted conversation, who said what, and what changed in it.
//
//  The web's group view (site-inbox/groups.js `renderView`/`renderMessages`) in SwiftUI, shared by
//  the phone, the iPad and the Mac. Every line here was decrypted on this device; what the postbox
//  said happened is shown only where a verified commit cannot say it better, and a disagreement
//  between the two is drawn as a warning (P-2 §9).

import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

struct GroupView: View {
    let groupId: String

    @Environment(GroupStore.self) private var groups
    @Environment(Account.self) private var account
    @Environment(CallHost.self) private var calls

    @State private var draft = ""
    @State private var staged: [StagedFile] = []
    @State private var sending = false
    @State private var showingInfo = false
    @State private var picking = false
    @State private var pickingPhotos = false
    @State private var photos: [PhotosPickerItem] = []
    @State private var deleting: Int?
    @State private var blocking: BlockTarget?
    @FocusState private var composing: Bool

    private var group: GroupSummary? { groups.group(groupId) }

    var body: some View {
        if let group {
            content(group)
                .task(id: groupId) { groups.show(groupId) }
                .onDisappear { groups.close(groupId) }
        } else {
            ContentUnavailableView("This group is gone", systemImage: "person.3",
                                   description: Text("It was deleted, or this mailbox is no longer in it."))
        }
    }

    @ViewBuilder
    private func content(_ group: GroupSummary) -> some View {
        VStack(spacing: 0) {
            history(group)
            bars(group)
            if group.canPost, groups.inGroup.contains(group.id) {
                composer(group)
            }
        }
        .background(Theme.wash)
        .toolbar { toolbar(group) }
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(Theme.wash, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        #endif
        .sheet(isPresented: $showingInfo) {
            GroupInfoSheet(groupId: group.id)
                .environment(groups)
        }
        .confirmationDialog("Delete this message for everyone?",
                            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
                            titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                if let seq = deleting { Task { await groups.deleteMessage(group.id, seq: seq) } }
                deleting = nil
            }
            Button("Cancel", role: .cancel) { deleting = nil }
        } message: {
            Text("It is removed from the postbox. Copies people already saw on their screens stay where they are.")
        }
        .sheet(item: $blocking) { target in BlockSheet(target: target) }
        .onAppear { if Fixtures.sheet == "group-info" { showingInfo = true } }
    }

    // ---- the bar ------------------------------------------------------------------------------------

    @ToolbarContentBuilder
    private func toolbar(_ group: GroupSummary) -> some ToolbarContent {
        #if os(iOS)
        ToolbarItem(placement: .principal) { title(group) }
        ToolbarItemGroup(placement: .topBarTrailing) { actions(group) }
        #else
        ToolbarItem(placement: .navigation) { title(group) }
        ToolbarItemGroup(placement: .primaryAction) { actions(group) }
        #endif
    }

    private func title(_ group: GroupSummary) -> some View {
        Button { showingInfo = true } label: {
            HStack(spacing: 9) {
                GroupFace(id: group.id, name: group.name, size: 28)
                VStack(alignment: .leading, spacing: 0) {
                    Text(group.name)
                        .font(.geist(14.5, .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                    Text(subtitle(group))
                        .font(.geist(11.5))
                        .foregroundStyle(Theme.muted)
                        .lineLimit(1)
                }
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Group info for \(group.name)")
        .accessibilityIdentifier("groupTitle")
        .help("Group info")
    }

    private func subtitle(_ group: GroupSummary) -> String {
        var bits = ["\(group.members) \(group.members == 1 ? "member" : "members")"]
        if let secs = group.ephemeralSecs { bits.append("⏱ " + GroupTimers.label(secs)) }
        return bits.joined(separator: " · ")
    }

    @ViewBuilder
    private func actions(_ group: GroupSummary) -> some View {
        if group.isActive {
            if groups.calls[group.id]?.active == true {
                Button("Join call") { calls.callGroup(group.id, name: group.name, video: groups.calls[group.id]?.kind == "video", join: true) }
                    .accessibilityLabel("Call in progress — Join")
                    .accessibilityIdentifier("groupCallJoin")
            }
            Button { calls.callGroup(group.id, name: group.name, video: false, join: false) } label: { Image(systemName: "phone") }
                .accessibilityLabel("Group voice call")
                .accessibilityIdentifier("groupVoiceCall")
                .disabled(calls.mode != .hidden)
            Button { calls.callGroup(group.id, name: group.name, video: true, join: false) } label: { Image(systemName: "video") }
                .accessibilityLabel("Group video call")
                .accessibilityIdentifier("groupVideoCall")
                .disabled(calls.mode != .hidden)
        }
        Button { showingInfo = true } label: { Image(systemName: "info.circle") }
            .accessibilityLabel("Group info")
            .accessibilityIdentifier("groupInfo")
    }

    // ---- the conversation -----------------------------------------------------------------------------

    private func history(_ group: GroupSummary) -> some View {
        let lines = groups.lines(group.id)
        return ScrollViewReader { reader in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 4) {
                    Label("Messages in this group are end-to-end encrypted. The postbox cannot read them.", systemImage: "lock")
                        .font(.geist(12))
                        .foregroundStyle(Theme.muted)
                        .frame(maxWidth: .infinity)
                        .multilineTextAlignment(.center)
                        .padding(.vertical, 10)
                        .accessibilityIdentifier("groupE2EENote")
                    if !lines.contains(where: { $0.kind == .message }) {
                        Text(group.ephemeralSecs.map { "Messages disappear after \(GroupTimers.label($0))." } ?? "No messages yet. Say hello.")
                            .font(.geist(13))
                            .foregroundStyle(Theme.secondary)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 8)
                    }
                    ForEach(lines) { line in
                        row(line, group: group).id(line.id)
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(.horizontal, 14)
                .padding(.bottom, 8)
                .frame(maxWidth: 860)
                .frame(maxWidth: .infinity)
            }
            .accessibilityIdentifier("group-history")
            .onAppear { reader.scrollTo("bottom", anchor: .bottom) }
            .onChange(of: lines.count) { _, _ in
                withAnimation(.easeOut(duration: 0.15)) { reader.scrollTo("bottom", anchor: .bottom) }
            }
        }
        .background { DoodleBackground() }
    }

    @ViewBuilder
    private func row(_ line: GroupLine, group: GroupSummary) -> some View {
        switch line.kind {
        case .day:
            DayDivider(label: line.text)
        case .notice:
            systemLine(line.text, colour: Theme.muted)
        case let .commit(security):
            VStack(spacing: 2) {
                systemLine(line.text, colour: security ? Theme.amber : Theme.muted, symbol: security ? "lock.shield" : nil)
                if let digits = line.fingerprint {
                    Text(GroupText.spaced(digits.filter(\.isNumber)).isEmpty ? digits : digits)
                        .font(.geistMono(11))
                        .foregroundStyle(Theme.muted)
                        .textSelection(.enabled)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier(security ? "groupSecurityNotice" : "groupNotice")
        case .warning:
            systemLine(line.text, colour: Theme.Pill.blockedText, symbol: "exclamationmark.triangle")
                .accessibilityIdentifier("groupWarning")
        case .message:
            if let item = line.item {
                GroupBubble(item: item, group: group, showSender: line.showSender, mine: item.sender == account.me?.address,
                            onDelete: { deleting = $0 },
                            onBlock: { sender in
                                blocking = BlockTarget(peer: groups.key(sender), name: groups.name(sender), origin: .group)
                            })
            }
        }
    }

    private func systemLine(_ text: String, colour: Color, symbol: String? = nil) -> some View {
        HStack(spacing: 5) {
            if let symbol { Image(systemName: symbol) }
            Text(text)
        }
        .font(.geist(12))
        .foregroundStyle(colour)
        .multilineTextAlignment(.center)
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity)
    }

    // ---- what stops a person posting ----------------------------------------------------------------

    @ViewBuilder
    private func bars(_ group: GroupSummary) -> some View {
        if let status = status(group) {
            bar(status, symbol: "hourglass").accessibilityIdentifier("groupStatus")
        }
        if group.isActive, group.role == "restricted" {
            bar("You can read this group but not send messages.", symbol: "eye").accessibilityIdentifier("groupAnnounce")
        } else if group.isActive, group.postPolicy == "admins", !group.canPost {
            bar("Only admins can send messages", symbol: "megaphone").accessibilityIdentifier("groupAnnounce")
        }
    }

    private func status(_ group: GroupSummary) -> String? {
        if group.isKeying {
            return "You have joined. Waiting for a member's device to add you to the encryption — this happens automatically when one is online."
        }
        if group.activationState == "provisioning" { return "Setting up encryption…" }
        if group.isActive, !groups.inGroup.contains(group.id) {
            return "This device is being added to the group's encryption. New messages will appear once it is."
        }
        return nil
    }

    private func bar(_ text: String, symbol: String) -> some View {
        Label(text, systemImage: symbol)
            .font(.geist(12.5))
            .foregroundStyle(Theme.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(Theme.field)
            .overlay(alignment: .top) { Rectangle().fill(Theme.rule).frame(height: 1) }
    }

    // ---- the composer ---------------------------------------------------------------------------------

    private func composer(_ group: GroupSummary) -> some View {
        VStack(spacing: 6) {
            if !staged.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(staged) { file in
                            StagedFileChip(file: file) { if !sending { staged.removeAll { $0.id == file.id } } }
                        }
                    }
                }
                .frame(maxWidth: 860)
                .padding(.horizontal, 12)
            }
            HStack(alignment: .bottom, spacing: 8) {
                attachMenu
                TextField("Write to the group", text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(.geist(15))
                    .lineLimit(1...6)
                    .focused($composing)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 8)
                    .accessibilityIdentifier("groupComposer")
                    #if os(macOS)
                    .onSubmit(send)
                    #endif
                Button(action: send) {
                    Image(systemName: "arrow.up")
                        .font(.geist(15, .bold))
                        .foregroundStyle(sendable ? Theme.onAccent : Theme.muted)
                        .frame(width: 34, height: 34)
                        .background(sendable ? Theme.accent : Theme.chip, in: RoundedRectangle(cornerRadius: 9))
                }
                .buttonStyle(.plain)
                .disabled(!sendable)
                .accessibilityLabel("Send")
                .accessibilityIdentifier("groupSend")
            }
            .padding(6)
            .background(Theme.composer, in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.strokeStrong, lineWidth: 1))
            .frame(maxWidth: 860)
            .padding(.horizontal, 12)
        }
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity)
        .background(Theme.wash)
        .overlay(alignment: .top) { Rectangle().fill(Theme.rule).frame(height: 1) }
        .fileImporter(isPresented: $picking, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            guard case let .success(urls) = result else { return }
            for url in urls { stage(url) }
        }
        #if os(iOS)
        .photosPicker(isPresented: $pickingPhotos, selection: $photos, maxSelectionCount: 5,
                      matching: .any(of: [.images, .videos]), photoLibrary: .shared())
        .onChange(of: photos) { _, chosen in
            guard !chosen.isEmpty else { return }
            Task { await stage(chosen) }
        }
        #endif
    }

    @ViewBuilder
    private var attachMenu: some View {
        #if os(iOS)
        Menu {
            Button { pickingPhotos = true } label: { Label("Photo Library", systemImage: "photo.on.rectangle") }
            Button { picking = true } label: { Label("Files", systemImage: "folder") }
        } label: {
            Image(systemName: "paperclip")
                .font(.geist(17, .medium))
                .foregroundStyle(Theme.secondary)
                .frame(width: 34, height: 34)
                .contentShape(Rectangle())
        }
        .accessibilityLabel("Attach")
        .disabled(sending)
        #else
        Button { picking = true } label: {
            Image(systemName: "paperclip")
                .font(.geist(15, .medium))
                .foregroundStyle(Theme.secondary)
                .frame(width: 30, height: 30)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Attach")
        .disabled(sending)
        #endif
    }

    private var sendable: Bool {
        !sending && (!draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !staged.isEmpty)
    }

    private func send() {
        guard sendable else { return }
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let files = staged
        let group = groupId
        draft = ""
        staged = []
        sending = true
        Task {
            if await !groups.send(group, text: text, files: files) {
                // Put the words back: a message that did not go is not a message to retype.
                if draft.isEmpty { draft = text }
                if staged.isEmpty { staged = files }
            }
            sending = false
            composing = true
        }
    }

    private func stage(_ url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        guard staged.count < 5, let data = try? Data(contentsOf: url), data.count <= AttachmentThumbnails.maxBytes else {
            groups.say("Attach up to 5 files, at most 25 MB each.")
            return
        }
        staged.append(StagedFile(name: url.lastPathComponent,
                                 mediaType: UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream",
                                 data: data))
    }

    #if os(iOS)
    private func stage(_ chosen: [PhotosPickerItem]) async {
        let now = Date()
        for (index, item) in chosen.enumerated() {
            guard staged.count < 5, let data = try? await item.loadTransferable(type: Data.self),
                  data.count <= AttachmentThumbnails.maxBytes else { continue }
            let type = item.supportedContentTypes.first
            staged.append(StagedFile(name: PickedImage.filename(for: type, index: index, at: now),
                                     mediaType: PickedImage.mediaType(for: type), data: data))
        }
        photos = []
    }
    #endif
}

/// One decrypted message.
struct GroupBubble: View {
    let item: GroupItem
    let group: GroupSummary
    let showSender: Bool
    let mine: Bool
    let onDelete: (Int) -> Void
    var onBlock: (String) -> Void = { _ in }

    @Environment(GroupStore.self) private var groups

    private var deletable: Bool {
        item.deleted != true && item.seq != nil && item.pending != true && (mine || group.isAdmin)
    }

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            if mine { Spacer(minLength: 40) }
            if !mine {
                Avatar(peer: groups.key(item.sender ?? ""), size: 26)
                    .opacity(showSender ? 1 : 0)
            }
            VStack(alignment: .leading, spacing: 4) {
                if showSender, !mine {
                    Text(groups.name(item.sender ?? ""))
                        .font(.geist(12, .semibold))
                        .foregroundStyle(PeerFace.toneInk(groups.key(item.sender ?? "")))
                }
                if item.deleted == true {
                    Text("This message was deleted").italic().foregroundStyle(Theme.muted)
                } else if item.undecryptable == true {
                    Label("Encrypted message — not readable on this device", systemImage: "lock.slash")
                        .foregroundStyle(Theme.muted)
                } else {
                    if let text = item.text, !text.isEmpty {
                        Text(text)
                            .foregroundStyle(mine ? Theme.Mine.text : Theme.body)
                            .textSelection(.enabled)
                    }
                    if let files = item.files, !files.isEmpty {
                        GroupFileList(groupId: group.id, files: files, mine: mine)
                    }
                }
                meta
            }
            .font(.geist(15))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(mine ? Theme.Mine.fill : Theme.ground, in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(mine ? Theme.Mine.stroke : Theme.stroke, lineWidth: 1))
            .frame(maxWidth: 560, alignment: mine ? .trailing : .leading)
            .contextMenu {
                if let text = item.text, !text.isEmpty, item.deleted != true {
                    Button { Clipboard.copy(text) } label: { Label("Copy", systemImage: "doc.on.doc") }
                }
                if deletable, let seq = item.seq {
                    Button(role: .destructive) { onDelete(seq) } label: { Label("Delete for everyone", systemImage: "trash") }
                }
                if !mine, let sender = item.sender {
                    Button(role: .destructive) { onBlock(sender) } label: { Label("Block sender…", systemImage: "hand.raised") }
                }
            }
            if !mine { Spacer(minLength: 40) }
        }
        .padding(.vertical, 1)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("groupMessage:" + (item.seq.map(String.init) ?? "pending"))
    }

    private var meta: some View {
        HStack(spacing: 4) {
            if let expires = item.expiresAt {
                Image(systemName: "timer")
                    .accessibilityLabel("Disappears " + Date(timeIntervalSince1970: TimeInterval(expires)).formatted(date: .abbreviated, time: .shortened))
            }
            Text(item.pending == true ? "Sending…" : (item.createdAt.map(Time.clockTime) ?? ""))
            if item.editOf != nil { Text("· edited") }
        }
        .font(.geist(11))
        .foregroundStyle(mine ? Theme.Mine.meta : Theme.muted)
    }
}

/// Files inside an encrypted message: fetched sealed, opened on this device, then shown in place
/// (Quick Look) or handed to the system, as conversation attachments are.
struct GroupFileList: View {
    let groupId: String
    let files: [GroupFile]
    let mine: Bool

    @Environment(GroupStore.self) private var groups
    @State private var opening: String?
    @State private var previewing: URL?
    @State private var handing: URL?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(files) { file in
                Button { open(file) } label: {
                    HStack(spacing: 8) {
                        if opening == file.id {
                            ProgressView().controlSize(.small).frame(width: 16)
                        } else {
                            Image(systemName: file.mime.hasPrefix("image/") ? "photo" : "doc").frame(width: 16)
                        }
                        Text(file.name).font(.geist(13.5, .medium)).lineLimit(1).truncationMode(.middle)
                        Text(ByteCountFormatter.string(fromByteCount: Int64(file.size), countStyle: .file))
                            .font(.geist(11.5))
                            .foregroundStyle(mine ? Theme.Mine.meta : Theme.muted)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .background(mine ? Theme.Mine.panel : Theme.Code.fill, in: RoundedRectangle(cornerRadius: 10))
                }
                .buttonStyle(.plain)
                .disabled(opening != nil || file.ref == nil)
                .accessibilityLabel("Open \(file.name)")
            }
        }
        .quickLookPreview($previewing)
        #if canImport(UIKit)
        .sheet(item: $handing) { url in GroupShareSheet(url: url) }
        #else
        .onChange(of: handing) { _, url in
            guard let url else { return }
            handing = nil
            NSWorkspace.shared.activateFileViewerSelecting([url])
        }
        #endif
    }

    private func open(_ file: GroupFile) {
        opening = file.id
        Task {
            defer { opening = nil }
            guard let data = await groups.openFile(groupId, file: file) else { return }
            let name = (file.name as NSString).lastPathComponent
            let folder = FileManager.default.temporaryDirectory.appendingPathComponent("group-files", isDirectory: true)
            try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            let url = folder.appendingPathComponent(name.isEmpty ? "file" : name)
            do {
                try data.write(to: url, options: [.atomic])
                if AttachmentList.viewable(url) { previewing = url } else { handing = url }
            } catch {
                groups.say("Could not open the file.")
            }
        }
    }
}

#if canImport(UIKit)
private struct GroupShareSheet: UIViewControllerRepresentable {
    let url: URL
    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: [url], applicationActivities: nil)
    }
    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
#endif
