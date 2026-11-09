//  One Nest: status, files, approvals, usage, and pause/resume, rename, export, delete
//  (Nests Phase B4/B5). Operators of a company Nest see what they may use; managing is for its
//  owner (a person's own Nest, or the organization's Nest admins). The postbox enforces both.

import SwiftUI
import UniformTypeIdentifiers

struct NestDetailView: View {
    @Environment(Account.self) private var account
    @State var nest: NestView
    @State private var files: NestFiles?
    @State private var approvals: [NestApproval] = []
    @State private var usage: NestUsage?
    @State private var message: String?
    @State private var busy = false
    @State private var importing = false
    @State private var exporting: ZipDocument?
    @State private var renaming = false
    @State private var newLabel = ""
    @State private var deleting = false
    @State private var confirmName = ""

    init(nest: NestView) { _nest = State(initialValue: nest) }

    private var client: PostboxClient { account.client }

    var body: some View {
        List {
            Section {
                NestStatusHeader(nest: nest)
            }
            Section("Files") {
                if let files {
                    LabeledContent("Stored", value: "\(NestWords.bytes(files.usedBytes)) of \(NestWords.bytes(files.quotaBytes))")
                    ForEach(files.files) { file in
                        NestFileRow(file: file)
                            .swipeActions {
                                Button("Delete", role: .destructive) { run { try await client.deleteNestFile(nest.nestId, file: file.fileId) } }
                            }
                            .contextMenu {
                                Button("Delete", role: .destructive) { run { try await client.deleteNestFile(nest.nestId, file: file.fileId) } }
                            }
                    }
                }
                Button { importing = true } label: { Label("Add a file", systemImage: "doc.badge.plus") }
                    .disabled(busy)
                    .accessibilityIdentifier("nest-add-file")
            }
            Section {
                let pending = approvals.filter { $0.status == "pending" }
                if pending.isEmpty { Text("Nothing is waiting for you.").foregroundStyle(.secondary) }
                ForEach(pending) { card in
                    NestApprovalCard(card: card, busy: busy) { decision in
                        run { try await client.decideNestApproval(nest.nestId, approval: card.approvalId,
                                                                  decision: decision, argsHash: card.argsHash) }
                    }
                }
            } header: { Text("Needs your approval") } footer: {
                Text("A Nest never sends anything by itself. Approve exactly what is shown, or reject it.")
            }
            if let usage {
                Section("This month") {
                    LabeledContent("Input tokens", value: tokens(usage.inputTokens))
                    LabeledContent("Output tokens", value: tokens(usage.outputTokens))
                    LabeledContent("Running time", value: hours(usage.runningSecs ?? 0, of: usage.package?.activeHoursPooled))
                    LabeledContent("Files", value: NestWords.bytes(usage.filesBytes))
                }
            }
            if nest.isOwner {
                Section("Manage") {
                    if nest.state == "paused" {
                        Button("Resume") { run { try await client.nestAction(nest.nestId, "resume") } }
                    } else {
                        Button("Pause") { run { try await client.nestAction(nest.nestId, "pause") } }
                    }
                    Button("Rename") { newLabel = nest.label; renaming = true }
                    Button("Export files and conversations") {
                        run {
                            let data = try await client.exportNest(nest.nestId)
                            exporting = ZipDocument(data: data)
                        }
                    }
                    Button("Delete Nest", role: .destructive) { confirmName = ""; deleting = true }
                        .accessibilityIdentifier("nest-delete")
                }
            }
            if let message {
                Section { Text(message).foregroundStyle(.red).accessibilityIdentifier("nest-detail-message") }
            }
        }
        .navigationTitle(nest.label)
        .disabled(busy)
        .refreshable { await load() }
        .task { await load() }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.item]) { result in
            guard case let .success(url) = result else { return }
            run {
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                let data = try Data(contentsOf: url)
                _ = try await client.uploadNestFile(nest.nestId, name: url.lastPathComponent, data: data)
            }
        }
        .fileExporter(isPresented: Binding(get: { exporting != nil }, set: { if !$0 { exporting = nil } }),
                      document: exporting, contentType: .zip,
                      defaultFilename: "nest-\(nest.label).zip") { _ in exporting = nil }
        .alert("Rename Nest", isPresented: $renaming) {
            TextField("Name", text: $newLabel)
            Button("Save") { run { try await client.renameNest(nest.nestId, label: newLabel) } }
            Button("Cancel", role: .cancel) {}
        }
        .alert("Delete \(nest.label)?", isPresented: $deleting) {
            TextField("Type the Nest's name", text: $confirmName)
            Button("Delete", role: .destructive) {
                run { try await client.deleteNest(nest.nestId, confirm: confirmName) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Its files and conversations are deleted for good. Export them first. Type \(nest.label) to confirm.")
        }
    }

    private func tokens(_ t: NestUsage.Tokens) -> String {
        let used = (t.used ?? 0).formatted(.number.notation(.compactName))
        guard let limit = t.limit else { return used }
        return "\(used) of \(limit.formatted(.number.notation(.compactName)))"
    }

    private func hours(_ secs: Int64, of pooled: Int?) -> String {
        let h = Double(secs) / 3600
        let text = h.formatted(.number.precision(.fractionLength(1)))
        return pooled.map { "\(text) of \($0) hours (shared)" } ?? "\(text) hours"
    }

    private func run(_ work: @escaping () async throws -> Void) {
        Task {
            busy = true
            defer { busy = false }
            do { try await work(); message = nil } catch { message = NestWords.refusal(error) }
            await load()
        }
    }

    private func load() async {
        #if DEBUG
        if Fixtures.enabled { return }
        #endif
        if let fresh = try? await client.nest(nest.nestId) { nest = fresh }
        files = try? await client.nestFiles(nest.nestId)
        approvals = (try? await client.nestApprovals(nest.nestId)) ?? []
        usage = try? await client.nestUsage(nest.nestId)
        await account.nests.reload()
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
    let decide: (String) -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if card.tool == "send_message", let args = card.call?.args {
                Text("Send a message").font(.headline)
                LabeledContent("To", value: args.to ?? "—")
                Text(args.body ?? "").font(.body).padding(8)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                    .textSelection(.enabled)
            } else {
                Text(card.tool).font(.headline)
            }
            Text("Expires \(Date(timeIntervalSince1970: TimeInterval(card.expiresAt)).formatted(date: .abbreviated, time: .shortened))")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Reject", role: .destructive) { decide("reject") }
                    .accessibilityIdentifier("nest-approval-reject")
                Spacer()
                Button("Approve") { decide("approve") }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("nest-approval-approve")
            }
            .disabled(busy)
        }
        .padding(.vertical, 4)
    }
}

/// Above a Nest's conversation, and at the top of its page: what it is doing right now.
struct NestStatusHeader: View {
    let nest: NestView
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
            Text("Nest").font(.caption).foregroundStyle(.secondary)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Nest status: \(NestWords.status(nest))")
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

/// In a conversation with one of the account's Nests: its live status, refreshed while open.
struct NestConversationHeader: View {
    @Environment(Account.self) private var account
    let peer: String
    @State private var view: NestView?

    var body: some View {
        Group {
            if let view {
                NestStatusHeader(nest: view).background(.bar)
            }
        }
        .task(id: peer) {
            guard account.features.nests else { view = nil; return }
            if account.nests.snapshot == nil { await account.nests.refresh() }
            guard let nest = account.nests.nests.first(where: { $0.address == peer }) else { view = nil; return }
            view = nest
            #if DEBUG
            if Fixtures.enabled { return }
            #endif
            while !Task.isCancelled {
                if let fresh = try? await account.client.nest(nest.nestId) { view = fresh }
                try? await Task.sleep(nanoseconds: 15_000_000_000)
            }
        }
    }
}
