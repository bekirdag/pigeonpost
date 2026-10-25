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
    @Environment(Account.self) private var account

    var body: some View {
        GroupConversation(groupId: groupId)
            .id((account.me?.address ?? "") + ":" + groupId)
    }
}

/// State belongs to one mailbox and group. An old send completing after navigation cannot
/// replace another mailbox's composer, quote or attachment selection.
private struct GroupConversation: View {
    let groupId: String

    @Environment(GroupStore.self) private var groups
    @Environment(Account.self) private var account
    @Environment(CallHost.self) private var calls

    @State private var draft = ""
    @State private var staged: [StagedFile] = []
    @State private var sending = false
    @State private var sheet: GroupSheet?
    @State private var picking = false
    @State private var pickingPhotos = false
    @State private var photos: [PhotosPickerItem] = []
    @State private var confirmation: Confirmation?
    @State private var action: ComposeAction?
    @State private var ordinaryDraft: String?
    @State private var ordinaryFiles: [StagedFile] = []
    @State private var jump: Jump?
    @State private var highlighted: Int?
    @FocusState private var composing: Bool

    private var group: GroupSummary? { groups.group(groupId) }
    private var items: [GroupItem] { groups.histories[groupId] ?? [] }
    private var now: Int { Int(Date().timeIntervalSince1970) }
    private var editing: Bool { if case .edit = action { return true }; return false }

    private enum ComposeAction: Equatable {
        case reply(Int), edit(Int)
        var seq: Int { switch self { case let .reply(seq), let .edit(seq): return seq } }
    }

    private struct Jump: Equatable { let id = UUID(); let seq: Int }
    private enum GroupSheet: Identifiable {
        case group, block(BlockTarget), message(Int)
        var id: String {
            switch self { case .group: return "group"; case .block: return "block"; case let .message(seq): return "message:\(seq)" }
        }
    }
    private enum Confirmation {
        case delete(Int), discard(String)
        var title: String {
            switch self { case .delete: return "Delete this message for everyone?"; case .discard: return "Dismiss this unconfirmed send?" }
        }
        var detail: String {
            switch self {
            case .delete: return "Every version is removed from the postbox and this device. Other devices remove their local copies when they reconnect. Files someone already saved elsewhere remain with them."
            case .discard: return "It is removed from this device's pending sends. It may already have arrived. Retry reuses the saved message without sending it twice."
            }
        }
    }

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
        .sheet(item: $sheet) { selected in
            switch selected {
            case .group: GroupInfoSheet(groupId: group.id).environment(groups)
            case let .block(target): BlockSheet(target: target)
            case let .message(seq): GroupMessageInfo(groupId: group.id, seq: seq).environment(groups)
            }
        }
        .confirmationDialog(confirmation?.title ?? "",
                            isPresented: Binding(get: { confirmation != nil }, set: { if !$0 { confirmation = nil } }),
                            titleVisibility: .visible, presenting: confirmation) { selected in
            switch selected {
            case let .delete(seq):
                Button("Delete", role: .destructive) { Task { await groups.deleteMessage(group.id, seq: seq) }; confirmation = nil }
            case let .discard(cmid):
                Button("Dismiss failed send", role: .destructive) { Task { await groups.discardMessage(group.id, cmid: cmid) }; confirmation = nil }
            }
            Button("Cancel", role: .cancel) { confirmation = nil }
        } message: { selected in Text(selected.detail) }
        .onAppear { if Fixtures.sheet == "group-info" { sheet = .group } }
        .onChange(of: items) { _, _ in
            guard !sending, let action else { return }
            if !actionAvailable(action) { cancelAction() }
        }
        .onChange(of: sending) { _, busy in
            if !busy, let action, !actionAvailable(action) { cancelAction() }
        }
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
        Button { sheet = .group } label: {
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
        Button { sheet = .group } label: { Image(systemName: "info.circle") }
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
            .onChange(of: jump) { _, destination in
                guard let destination else { return }
                withAnimation(.easeInOut(duration: 0.2)) { reader.scrollTo("m:\(destination.seq)", anchor: .center) }
                highlighted = destination.seq
                Task { @MainActor in
                    try? await Task.sleep(for: .seconds(1.8))
                    if jump?.id == destination.id { highlighted = nil }
                }
            }
            .onChange(of: composing) { _, focused in
                guard focused else { return }
                Task { @MainActor in
                    // The keyboard first changes the viewport, then the scroll anchor. Waiting
                    // for that layout keeps the latest message's controls above the composer.
                    try? await Task.sleep(for: .milliseconds(250))
                    guard composing else { return }
                    withAnimation(.easeOut(duration: 0.15)) { reader.scrollTo("bottom", anchor: .bottom) }
                }
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
                            editable: account.features.messageEdit && canEdit(item),
                            replyable: account.features.messageReply && group.canPost && groups.inGroup.contains(group.id) && available(item),
                            busy: sending, highlighted: item.seq == highlighted,
                            onReply: { beginReply(item) }, onEdit: { beginEdit(item) },
                            onInfo: { if let seq = item.seq { sheet = .message(seq) } },
                            onJump: { jump = Jump(seq: $0) },
                            onRetry: { retry(item) },
                            onDiscard: { if let cmid = item.cmid { confirmation = .discard(cmid) } },
                            onDelete: { confirmation = .delete($0) },
                            onBlock: { sender in
                                sheet = .block(BlockTarget(peer: groups.key(sender), name: groups.name(sender), origin: .group))
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
            if let action { actionBanner(action) }
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
                TextField(editing ? "Edit message" : "Write to the group", text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(.geist(15))
                    .lineLimit(1...6)
                    .focused($composing)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 8)
                    .accessibilityIdentifier("groupComposer")
                    .disabled(sending)
                    #if os(macOS)
                    .onSubmit(send)
                    #endif
                Button(action: send) {
                    Image(systemName: editing ? "checkmark" : "arrow.up")
                        .font(.geist(15, .bold))
                        .foregroundStyle(sendable ? Theme.onAccent : Theme.muted)
                        .frame(width: 34, height: 34)
                        .background(sendable ? Theme.accent : Theme.chip, in: RoundedRectangle(cornerRadius: 9))
                }
                .buttonStyle(.plain)
                .disabled(!sendable)
                .accessibilityLabel(editing ? "Save edit" : "Send")
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
        .disabled(sending || editing)
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
        .disabled(sending || editing)
        #endif
    }

    private var sendable: Bool {
        guard !sending else { return false }
        if case let .edit(seq) = action {
            guard let item = items.first(where: { $0.seq == seq }), canEdit(item) else { return false }
            return draft != item.text && (!draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !(item.files ?? []).isEmpty)
        }
        return !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !staged.isEmpty
    }

    private func send() {
        guard sendable else { return }
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let files = staged
        let group = groupId
        if let action, !actionAvailable(action) { cancelAction(); return }
        let mode = action
        let replyTo: Int?
        let editOf: Int?
        switch mode {
        case let .reply(seq): replyTo = seq; editOf = nil
        case let .edit(seq):
            editOf = seq
            replyTo = items.first(where: { $0.seq == seq && $0.actionsVerified == true }).flatMap { GroupActions.sequence($0.replyTo) }
        case nil: replyTo = nil; editOf = nil
        }
        sending = true
        Task {
            if await groups.send(group, text: text, files: files, replyTo: replyTo, editOf: editOf) {
                if case .edit = mode { cancelAction() }
                else { draft = ""; staged = []; action = nil }
            }
            sending = false
            composing = true
        }
    }

    private func available(_ item: GroupItem) -> Bool {
        item.kind == .message && item.seq != nil && item.pending != true && item.deleted != true
            && item.undecryptable != true && (item.expiresAt ?? .max) > now
    }

    private func canEdit(_ item: GroupItem) -> Bool {
        guard group?.canPost == true, let me = account.me?.address,
              (item.files ?? []).allSatisfy({ $0.ref != nil }) else { return false }
        return GroupActions.editable(item, in: items, me: me, now: now)
    }

    private func actionAvailable(_ action: ComposeAction) -> Bool {
        guard let target = items.first(where: { $0.seq == action.seq }), available(target) else { return false }
        switch action {
        case .reply: return account.features.messageReply
        case .edit: return account.features.messageEdit && canEdit(target)
        }
    }

    private func beginReply(_ item: GroupItem) {
        guard !sending, available(item), let seq = item.seq else { return }
        if editing { cancelAction() }
        action = .reply(seq)
        composing = true
    }

    private func retry(_ item: GroupItem) {
        guard !sending, let cmid = item.cmid else { return }
        let replyTo: Int?
        let editOf: Int?
        switch action {
        case let .reply(seq): replyTo = seq; editOf = nil
        case let .edit(seq):
            replyTo = items.first(where: { $0.seq == seq && $0.actionsVerified == true }).flatMap { GroupActions.sequence($0.replyTo) }
            editOf = seq
        case nil: replyTo = nil; editOf = nil
        }
        let files = staged.map { OutgoingGroupFile(name: $0.name, mime: $0.mediaType, data: $0.data) }
        let fingerprint = try? GroupSendIntent.fingerprint(groupId, text: draft.trimmingCharacters(in: .whitespacesAndNewlines),
                                                         files: files, replyTo: replyTo, editOf: editOf, mentions: [])
        let matchesDraft = item.sendFingerprint != nil && fingerprint == item.sendFingerprint
        sending = true
        Task {
            let success = await groups.retryMessage(groupId, cmid: cmid)
            if success, matchesDraft {
                if editing { cancelAction() } else { draft = ""; staged = []; action = nil }
            }
            sending = false
        }
    }

    private func beginEdit(_ item: GroupItem) {
        guard !sending, canEdit(item), let seq = item.seq else { return }
        if editing { cancelAction() }
        ordinaryDraft = draft
        ordinaryFiles = staged
        draft = item.text ?? ""
        staged = []
        action = .edit(seq)
        composing = true
    }

    private func cancelAction() {
        if editing { draft = ordinaryDraft ?? ""; staged = ordinaryFiles }
        ordinaryDraft = nil
        ordinaryFiles = []
        action = nil
    }

    private func actionBanner(_ action: ComposeAction) -> some View {
        HStack(spacing: 10) {
            Image(systemName: editing ? "pencil" : "arrowshape.turn.up.left")
            VStack(alignment: .leading, spacing: 3) {
                Text(editing ? "Editing message" : "Replying to message").font(.geist(12, .semibold))
                Text(items.first(where: { $0.seq == action.seq })?.text ?? "Message").font(.geist(12)).lineLimit(2)
            }
            Spacer(minLength: 0)
            Button(action: cancelAction) { Image(systemName: "xmark").frame(width: 44, height: 44) }
                .buttonStyle(.plain)
                .keyboardShortcut(.cancelAction)
                .accessibilityLabel(editing ? "Cancel edit" : "Cancel reply")
                .accessibilityIdentifier("cancel-group-action")
                .disabled(sending)
        }
        .foregroundStyle(Theme.secondary)
        .padding(.leading, 12)
        .background(Theme.chip, in: RoundedRectangle(cornerRadius: 10))
        .padding(.horizontal, 12)
        .frame(maxWidth: 884)
    }

    private func stage(_ url: URL) {
        guard !sending, !editing else { return }
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
            guard !sending, !editing else { break }
            guard staged.count < 5, let data = try? await item.loadTransferable(type: Data.self),
                  data.count <= AttachmentThumbnails.maxBytes else { continue }
            guard !sending, !editing else { break }
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
    let editable: Bool
    let replyable: Bool
    let busy: Bool
    let highlighted: Bool
    let onReply: () -> Void
    let onEdit: () -> Void
    let onInfo: () -> Void
    let onJump: (Int) -> Void
    let onRetry: () -> Void
    let onDiscard: () -> Void
    let onDelete: (Int) -> Void
    var onBlock: (String) -> Void = { _ in }

    @Environment(GroupStore.self) private var groups
    @Environment(Account.self) private var account

    private var deletable: Bool {
        item.deleted != true && item.seq != nil && item.pending != true
            && (group.isAdmin || (mine && GroupActions.editable(item, in: groups.histories[group.id] ?? [],
                me: account.me?.address ?? "", now: Int(Date().timeIntervalSince1970))))
    }

    var body: some View {
        VStack(alignment: mine ? .trailing : .leading, spacing: 0) {
            bubble
            if item.pending == true, item.sendFailed == true, let cmid = item.cmid {
                HStack(spacing: 14) {
                    if item.code != "send_unavailable" {
                        Button(action: onRetry) { Label("Retry", systemImage: "arrow.clockwise") }
                            .accessibilityIdentifier("group-retry:" + cmid)
                    }
                    Button(action: onDiscard) { Label("Dismiss", systemImage: "xmark") }
                        .accessibilityIdentifier("group-discard:" + cmid)
                }
                .font(.geist(12))
                .foregroundStyle(Theme.secondary)
                .buttonStyle(MessageActionButtonStyle())
                .disabled(busy)
                .padding(.horizontal, 8)
            }
            if item.deleted != true, item.undecryptable != true, item.pending != true, let seq = item.seq {
                HStack(spacing: 12) {
                    if replyable {
                        Button(action: onReply) { Label("Reply", systemImage: "arrowshape.turn.up.left") }
                            .accessibilityIdentifier("group-reply:\(seq)")
                    }
                    if editable {
                        Button(action: onEdit) { Label("Edit", systemImage: "pencil") }
                            .accessibilityIdentifier("group-edit:\(seq)")
                    }
                    if account.features.messageEdit || account.features.receipts {
                        Button(action: onInfo) { Label("Info", systemImage: "info.circle") }
                            .accessibilityLabel("Message info")
                            .accessibilityIdentifier("group-info:\(seq)")
                    }
                }
                .font(.geist(11.5))
                .foregroundStyle(Theme.muted)
                .buttonStyle(MessageActionButtonStyle())
                .disabled(busy)
                .padding(mine ? .trailing : .leading, mine ? 8 : 42)
            }
        }
    }

    private var bubble: some View {
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
                    if item.actionsVerified == true, item.replyTo != nil { quote }
                    if let text = item.text, !text.isEmpty {
                        Text(text)
                            .foregroundStyle(mine ? Theme.Mine.text : Theme.body)
                            .textSelection(.enabled)
                    }
                    if let files = item.files, !files.isEmpty {
                        GroupFileList(groupId: group.id, files: files, mine: mine)
                            .disabled(item.pending == true)
                    }
                }
                meta
            }
            .font(.geist(15))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(mine ? Theme.Mine.fill : Theme.ground, in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(highlighted ? Theme.accent : (mine ? Theme.Mine.stroke : Theme.stroke), lineWidth: highlighted ? 2 : 1))
            .frame(maxWidth: 560, alignment: mine ? .trailing : .leading)
            .contextMenu {
                if replyable { Button(action: onReply) { Label("Reply", systemImage: "arrowshape.turn.up.left") }.disabled(busy) }
                if editable { Button(action: onEdit) { Label("Edit", systemImage: "pencil") }.disabled(busy) }
                if item.seq != nil, item.deleted != true, account.features.messageEdit || account.features.receipts {
                    Button(action: onInfo) { Label("Message info", systemImage: "info.circle") }
                }
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
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("groupMessage:" + (item.seq.map(String.init) ?? "pending"))
    }

    private var meta: some View {
        HStack(spacing: 4) {
            if let expires = item.expiresAt {
                Image(systemName: "timer")
                    .accessibilityLabel("Disappears " + Date(timeIntervalSince1970: TimeInterval(expires)).formatted(date: .abbreviated, time: .shortened))
            }
            Text(item.pending == true ? (item.sendFailed == true ? "Delivery unconfirmed" : "Sending…") : (item.createdAt.map(Time.clockTime) ?? ""))
            if item.actionsVerified == true, item.editOf != nil { Text("· edited") }
            if mine, item.deleted != true, item.pending != true, let seq = item.seq, account.features.receipts {
                let state = groups.receipts[group.id]?.state(at: seq) ?? .sent
                MessageReceiptMark(receipt: MessageReceipt(state: state.rawValue, deliveredAt: nil, seenAt: nil))
                    .accessibilityLabel(state == .seen ? "Seen by members sharing read receipts" : state.rawValue.capitalized)
            }
        }
        .font(.geist(11))
        .foregroundStyle(mine ? Theme.Mine.meta : Theme.muted)
    }

    private var quote: some View {
        let parent = GroupActions.replyTarget(of: item, in: groups.histories[group.id] ?? [], now: Int(Date().timeIntervalSince1970))
        return Button {
            if let seq = parent?.seq { onJump(seq) }
        } label: {
            HStack(spacing: 8) {
                RoundedRectangle(cornerRadius: 1).fill(Theme.accent).frame(width: 2)
                VStack(alignment: .leading, spacing: 2) {
                    if let parent { Text(groups.name(parent.sender ?? "")).font(.geist(11, .semibold)) }
                    Text(parent.map { ($0.text?.isEmpty == false ? $0.text : $0.files?.first?.name) ?? "Message" }
                         ?? "Original message unavailable")
                        .font(.geist(12)).lineLimit(2)
                }
                Spacer(minLength: 0)
            }
            .foregroundStyle(Theme.secondary)
            .padding(7)
            .background(mine ? Theme.Mine.panel : Theme.chip, in: RoundedRectangle(cornerRadius: 7))
            .fixedSize(horizontal: false, vertical: true)
        }
        .buttonStyle(.plain)
        .disabled(parent == nil)
        .accessibilityLabel(parent.map { "Go to message from " + groups.name($0.sender ?? "") } ?? "Original message unavailable")
        .accessibilityIdentifier("group-quote:\(item.seq ?? 0)")
    }
}

private struct GroupMessageInfo: View {
    let groupId: String
    let seq: Int
    @Environment(GroupStore.self) private var groups
    @Environment(Account.self) private var account
    @Environment(\.dismiss) private var dismiss

    private var history: [GroupItem] { groups.histories[groupId] ?? [] }
    private var current: GroupItem? {
        GroupActions.visible(history, now: Int(Date().timeIntervalSince1970))
            .first { $0.kind == .message && $0.seq == seq && $0.deleted != true }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let current {
                        let versions = GroupActions.versions(of: current, in: history)
                        LabeledContent("From", value: groups.name(current.sender ?? ""))
                        LabeledContent("Sent", value: date(versions.first?.createdAt))
                        if current.actionsVerified == true, current.editOf != nil {
                            LabeledContent("Edited", value: date(current.createdAt))
                        }
                        if let expires = current.expiresAt { LabeledContent("Disappears", value: date(expires)) }
                        if current.sender == account.me?.address, account.features.receipts,
                           let receipts = groups.receipts[groupId], let members = receipts.members {
                            Text("Delivery").font(.geist(13, .semibold))
                            ForEach(members) { member in
                                LabeledContent(groups.name(member.address), value: receipt(member, shares: receipts.readReceipts != false))
                                    .accessibilityElement(children: .ignore)
                                    .accessibilityLabel(groups.name(member.address))
                                    .accessibilityValue(receipt(member, shares: receipts.readReceipts != false))
                                    .accessibilityIdentifier("group-receipt:" + member.address)
                            }
                            Text("Read status is shown only for members who share read receipts.")
                                .font(.geist(12)).foregroundStyle(Theme.muted)
                        }
                        Divider()
                        Text("Versions on this device").font(.geist(13, .semibold))
                        Text("Earlier versions are available only if this device received them before they were replaced.")
                            .font(.geist(12)).foregroundStyle(Theme.muted)
                        ForEach(versions) { version in
                            VStack(alignment: .leading, spacing: 5) {
                                Text(date(version.createdAt)).font(.geist(11)).foregroundStyle(Theme.muted)
                                Text(version.text ?? "Encrypted message unavailable").textSelection(.enabled)
                            }
                            .padding(12)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Theme.chip, in: RoundedRectangle(cornerRadius: 10))
                            .accessibilityElement(children: .combine)
                            .accessibilityIdentifier("group-version:\(version.seq ?? 0)")
                        }
                    } else {
                        Text("This message is no longer available.").foregroundStyle(Theme.secondary)
                    }
                }
                .font(.geist(13))
                .padding(20)
            }
            .sheetSurface()
            .navigationTitle("Message info")
            .inlineTitle()
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        #if os(macOS)
        .frame(minWidth: 420, minHeight: 400)
        #endif
        .task { await groups.loadMembers(groupId); await groups.loadReceipts(groupId) }
    }

    private func date(_ timestamp: Int?) -> String {
        timestamp.map { Date(timeIntervalSince1970: TimeInterval($0)).formatted(date: .abbreviated, time: .shortened) } ?? "Unknown"
    }

    private func receipt(_ member: GroupMemberReceipt, shares: Bool) -> String {
        if shares, let seen = member.seenSeq, seen >= seq { return "Seen" }
        if let delivered = member.deliveredSeq, delivered >= seq { return "Delivered" }
        return "Sent"
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
    @State private var openingVisit = UUID()
    @State private var temporaryFolder = FileManager.default.temporaryDirectory
        .appendingPathComponent("group-preview-" + UUID().uuidString, isDirectory: true)

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
        .onDisappear { discardPreview() }
        .onChange(of: files) { _, _ in discardPreview() }
        .onChange(of: groups.histories[groupId]) { _, _ in
            if files.contains(where: { !groups.hasFile(groupId, file: $0) }) { discardPreview() }
        }
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
        let visit = UUID()
        openingVisit = visit
        Task {
            defer { if openingVisit == visit { opening = nil } }
            guard let data = await groups.openFile(groupId, file: file), openingVisit == visit,
                  groups.hasFile(groupId, file: file) else { return }
            let name = (file.name as NSString).lastPathComponent
            // An owned directory per file view prevents collisions between names/mailboxes;
            // only this directory is removed when the message is erased or the view goes away.
            let folder = temporaryFolder
            try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            let url = folder.appendingPathComponent(name.isEmpty ? "file" : name)
            do {
                #if os(iOS)
                try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
                #else
                try data.write(to: url, options: [.atomic])
                #endif
                if AttachmentList.viewable(url) { previewing = url } else { handing = url }
            } catch {
                groups.say("Could not open the file.")
            }
        }
    }

    private func discardPreview() {
        openingVisit = UUID()
        opening = nil
        previewing = nil
        handing = nil
        try? FileManager.default.removeItem(at: temporaryFolder)
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
