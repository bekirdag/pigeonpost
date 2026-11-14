//  One Nest: status, files, approvals, usage, and pause/resume, rename, export, delete
//  (Nests Phase B4/B5). Operators of a company Nest see what they may use; managing is for its
//  owner (a person's own Nest, or the organization's Nest admins). The postbox enforces both.

import SwiftUI
import UniformTypeIdentifiers

struct NestDetailView: View {
    @Environment(Account.self) private var account
    let nest: NestView

    var body: some View {
        NestDetailContent(store: account.nestDetail(nest))
    }
}

struct NestDetailContent: View {
    @Environment(\.dismiss) private var dismiss
    @State private var store: NestDetailStore
    @State private var importing = false
    @State private var renaming = false
    @State private var newLabel = ""
    @State private var deleting = false
    @State private var confirmName = ""

    init(store: NestDetailStore) { _store = State(initialValue: store) }

    private var nest: NestView { store.nest }

    var body: some View {
        List {
            Section {
                NestStatusHeader(nest: nest)
                if let why = NestWords.pausedExplanation(nest) {
                    Text(why).font(.footnote).foregroundStyle(.secondary)
                        .accessibilityIdentifier("nest-paused-reason")
                }
            }
            Section("Files") {
                if let files = store.files {
                    LabeledContent("Stored", value: "\(NestWords.bytes(files.usedBytes)) of \(NestWords.bytes(files.quotaBytes))")
                    ForEach(files.files) { file in
                        NestFileRow(file: file)
                            .swipeActions {
                                Button("Delete", role: .destructive) { act { await store.deleteFile(file) } }
                            }
                            .contextMenu {
                                Button("Delete", role: .destructive) { act { await store.deleteFile(file) } }
                            }
                    }
                }
                Button { importing = true } label: { Label("Add a file", systemImage: "doc.badge.plus") }
                    .disabled(store.busy)
                    .accessibilityIdentifier("nest-add-file")
            }
            Section {
                if store.pending.isEmpty {
                    Text(store.approvalsLoaded ? "Nothing is waiting for you." : "Checking…").foregroundStyle(.secondary)
                }
                ForEach(store.pending) { card in
                    NestApprovalCard(card: card, busy: store.busy) { approve in
                        act { await store.decide(card, approve: approve) }
                    }
                }
            } header: { Text("Needs your approval") } footer: {
                Text("A Nest never sends anything by itself. Approve exactly what is shown, or reject it.")
            }
            if !store.decided.isEmpty {
                Section("Recent decisions") {
                    ForEach(store.decided) { card in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(card.call?.args.to ?? card.tool).lineLimit(1)
                            Text(NestWords.outcome(card)).font(.footnote).foregroundStyle(.secondary)
                        }
                        .accessibilityElement(children: .combine)
                    }
                }
            }
            if let usage = store.usage {
                Section("This month") {
                    LabeledContent("Input tokens", value: NestWords.tokens(usage.inputTokens))
                    LabeledContent("Output tokens", value: NestWords.tokens(usage.outputTokens))
                    LabeledContent("Running time", value: hours(usage.runningSecs ?? 0, of: usage.package?.activeHoursPooled))
                    LabeledContent("Files", value: NestWords.bytes(usage.filesBytes))
                }
            }
            if store.canManage {
                Section("Manage") {
                    if store.canResume {
                        Button("Resume") { act { await store.resume() } }.accessibilityIdentifier("nest-resume")
                    } else if store.canPause {
                        Button("Pause") { act { await store.pause() } }.accessibilityIdentifier("nest-pause")
                    }
                    Button("Rename") { newLabel = nest.label; renaming = true }
                        .accessibilityIdentifier("nest-rename")
                    Button("Export files and conversations") { act { await store.export() } }
                        .accessibilityIdentifier("nest-export")
                    Button("Delete Nest", role: .destructive) { confirmName = ""; deleting = true }
                        .accessibilityIdentifier("nest-delete")
                }
            }
            if let message = store.message {
                Section { Text(message).foregroundStyle(Theme.danger).accessibilityIdentifier("nest-detail-message") }
            }
        }
        .navigationTitle(nest.label)
        .disabled(store.busy)
        .refreshable { await store.load() }
        .task { await store.load() }
        .onChange(of: store.gone) { _, gone in if gone { dismiss() } }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.item]) { result in
            guard case let .success(url) = result else { return }
            act { await upload(url) }
        }
        .fileExporter(isPresented: Binding(get: { store.exported != nil }, set: { if !$0 { store.clearExport() } }),
                      document: store.exported.map(ZipDocument.init(data:)), contentType: .zip,
                      defaultFilename: NestWords.exportName(nest.label)) { _ in store.clearExport() }
        .alert("Rename Nest", isPresented: $renaming) {
            TextField("Name", text: $newLabel)
            Button("Save") { act { await store.rename(newLabel) } }
                .disabled(NestWords.validLabel(newLabel) == nil)
            Button("Cancel", role: .cancel) {}
        }
        .alert("Delete \(nest.label)?", isPresented: $deleting) {
            TextField("Type the Nest's name", text: $confirmName)
            Button("Delete", role: .destructive) { act { await store.delete(typed: confirmName) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Its files and conversations are deleted for good. Export them first. Type \(nest.label) to confirm.")
        }
    }

    private func hours(_ secs: Int64, of pooled: Int?) -> String {
        let h = Double(secs) / 3600
        let text = h.formatted(.number.precision(.fractionLength(1)))
        return pooled.map { "\(text) of \($0) hours (shared)" } ?? "\(text) hours"
    }

    private func act(_ work: @escaping () async -> Void) {
        Task { await work() }
    }

    /// The picked file is read only after its size is known, and off the main thread.
    private func upload(_ url: URL) async {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize).flatMap { $0 }.map(Int64.init)
        await store.upload(name: url.lastPathComponent, size: size) {
            try await Task.detached(priority: .userInitiated) { try Data(contentsOf: url, options: .mappedIfSafe) }.value
        }
    }
}

struct NestFileRow: View {
    let file: NestFile
    private var detail: String {
        let size = NestWords.bytes(file.bytes)
        switch file.status {
        case "parsed": return "\(size) · ready"
        case "partial":
            if let total = file.coverage.unitsTotal, let text = file.coverage.unitsText {
                return "\(size) · partly read (\(text) of \(total))"
            }
            return "\(size) · partly read"
        case "rejected": return "\(size) · not readable" + (file.reason.map { ": \($0)" } ?? "")
        case "failed": return "\(size) · upload failed"
        default: return "\(size) · reading…"
        }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(file.name).lineLimit(1)
            Text(detail).font(.footnote).foregroundStyle(file.status == "rejected" || file.status == "failed" ? .red : .secondary)
            ForEach(file.warnings.prefix(3), id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
        }
        .accessibilityElement(children: .combine)
    }
}

struct NestApprovalCard: View {
    let card: NestApproval
    let busy: Bool
    let decide: (Bool) -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if card.isReviewable, let args = card.call?.args {
                Text("Send a message").font(.headline)
                LabeledContent("To", value: args.to ?? "")
                Text(args.body ?? "").font(.body).padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                    .textSelection(.enabled)
                    .accessibilityIdentifier("nest-approval-body")
            } else {
                Text(card.tool).font(.headline)
                Text("This version of the app cannot show what this action would do. You can reject it, or update the app to review it.")
                    .font(.footnote).foregroundStyle(.secondary)
            }
            Text("Expires \(Date(timeIntervalSince1970: TimeInterval(card.expiresAt)).formatted(date: .abbreviated, time: .shortened))")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Reject", role: .destructive) { decide(false) }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier("nest-approval-reject")
                Spacer()
                if card.isReviewable {
                    Button("Approve") { decide(true) }
                        .buttonStyle(.borderedProminent)
                        .accessibilityIdentifier("nest-approval-approve")
                }
            }
            .disabled(busy)
        }
        .padding(.vertical, 4)
    }
}

/// Above a Nest's conversation, and at the top of its page: what it is doing right now.
struct NestStatusHeader: View {
    let nest: NestView
    var waiting = 0
    static func spoken(_ nest: NestView, waiting: Int) -> String {
        let status = "Nest status: \(NestWords.status(nest))"
        return waiting > 0 ? "\(status). \(waiting) waiting for your approval" : status
    }
    private var color: Color {
        switch nest.status {
        case "ready": return .green
        case "working": return .blue
        case "needs_you": return .orange
        case "paused": return .gray
        default: return .red
        }
    }
    var body: some View {
        HStack(spacing: 8) {
            Circle().fill(color).frame(width: 9, height: 9).accessibilityHidden(true)
            if nest.status == "working" { ProgressView().controlSize(.small) }
            Text(NestWords.status(nest)).font(.subheadline.weight(.medium))
            Spacer()
            if waiting > 0 {
                Text("\(waiting) to approve").font(.caption.weight(.semibold)).foregroundStyle(Theme.amber)
                    .accessibilityIdentifier("nest-waiting")
            }
            Text("Nest").font(.caption).foregroundStyle(.secondary)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Self.spoken(nest, waiting: waiting))
        .accessibilityIdentifier("nest-status")
    }
}

/// The export, as a document `fileExporter` can save.
struct ZipDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.zip] }
    var data: Data
    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws { data = configuration.file.regularFileContents ?? Data() }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}

/// In a conversation with a Nest: its live status and what is waiting for approval, refreshed
/// while open. Tapping it opens the Nest's files, approvals and management.
struct NestConversationHeader: View {
    @Environment(Account.self) private var account
    let peer: String
    /// The mailbox addresses this peer's messages came from: a conversation is keyed by handle
    /// once the peer has one, and an organization Nest is known to the app by its address.
    var addresses: [String] = []
    @State private var store: NestDetailStore?
    @State private var showing = false

    private var candidates: [String] { [peer] + addresses.filter { $0 != peer } }
    private var watching: String { "\(candidates.joined(separator: ","))|\(account.features.nests)|\(account.session.subject ?? "")" }

    var body: some View {
        // The empty stack is deliberate: `.task` on a `Group` with nothing in it never runs, and
        // there is nothing in it until the task has found the Nest.
        VStack(spacing: 0) {
            if let store, !store.gone {
                Button { showing = true } label: {
                    HStack(spacing: 4) {
                        NestStatusHeader(nest: store.nest, waiting: store.pending.count)
                        Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.tertiary)
                            .padding(.trailing, 12).accessibilityHidden(true)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .background(.bar)
                .accessibilityElement(children: .ignore)
                .accessibilityAddTraits(.isButton)
                .accessibilityLabel(NestStatusHeader.spoken(store.nest, waiting: store.pending.count))
                .accessibilityHint("Opens files, approvals and settings for this Nest")
                .accessibilityIdentifier("nest-conversation-header")
            }
        }
        .sheet(isPresented: $showing) {
            if let store {
                NavigationStack {
                    NestDetailContent(store: store)
                        .toolbar {
                            ToolbarItem(placement: .cancellationAction) { Button("Done") { showing = false } }
                        }
                }
                #if os(macOS)
                .frame(minWidth: 480, minHeight: 560)
                #endif
            }
        }
        .task(id: watching) {
            store = nil
            guard account.features.nests else { return }
            guard let nest = await account.nests.nest(forPeer: candidates), !Task.isCancelled else { return }
            let found = account.nestDetail(nest)
            store = found
            #if DEBUG
            if Fixtures.enabled { await found.refreshStatus(); return }
            #endif
            while !Task.isCancelled {
                await found.refreshStatus()
                if found.gone { return }
                try? await Task.sleep(nanoseconds: 15_000_000_000)
            }
        }
    }
}

/// Picks up a Nest purchase that was paid for but never reached the postbox, when the account's
/// `nests` capability is known to be on, and keeps the StoreKit listener alive from launch.
private struct NestPurchaseRecovery: ViewModifier {
    @Environment(Account.self) private var account
    func body(content: Content) -> some View {
        content.task(id: account.features.nests) {
            if account.features.nests { await account.nests.refresh() }
        }
    }
}

extension View {
    func nestPurchaseRecovery() -> some View { modifier(NestPurchaseRecovery()) }
}
