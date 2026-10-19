//  Files, on the way out and on the way in.
//
//  Static system thumbnails identify supported media. A tap opens pictures, video, sound, PDFs and
//  text in place (Quick Look); other files are shared on a phone and saved on a Mac. On the Mac each
//  file also has its own download button.

import QuickLook
import SwiftUI
import UniformTypeIdentifiers
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

/// One chosen file, above the composer, with a way to change your mind.
struct StagedFileChip: View {
    let file: StagedFile
    let remove: () -> Void

    var body: some View {
        HStack(spacing: 6) {
            AttachmentPreview(name: file.name, mediaType: file.mediaType, bytes: file.data.count,
                              cacheKey: "draft:\(file.id)", compact: true) { file.data }
            Text(file.name)
                .font(.geist(12.5))
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: 150)
            Text(file.readableSize)
                .font(.geist(11.5))
                .foregroundStyle(Theme.muted)
            Button(action: remove) {
                Image(systemName: "xmark")
                    .font(.geist(10, .semibold))
                    .foregroundStyle(Theme.muted)
                    .frame(width: 18, height: 18)
                    .contentShape(Rectangle())
            }
            .accessibilityLabel("Remove \(file.name)")
        }
        .padding(.leading, 9)
        .padding(.trailing, 3)
        .padding(.vertical, 4)
        .background(Theme.field, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.strokeStrong, lineWidth: 1))
    }
}

/// The files on a received or sent message.
struct AttachmentList: View {
    let attachments: [MessageAttachment]
    let isMine: Bool

    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @State private var downloading: String?
    @State private var saved: URL?
    /// A file the app can show itself, opened in place (Quick Look).
    @State private var previewing: URL?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(attachments) { file in
                Button {
                    fetch(file)
                } label: {
                    VStack(alignment: .leading, spacing: 8) {
                        if let identity = account.me?.address {
                            AttachmentPreview(name: file.filename, mediaType: file.mediaType, bytes: file.bytes,
                                              cacheKey: "\(identity):\(file.id)") {
                                try await account.client.downloadAttachment(identity: identity, id: file.id)
                            }
                        }
                        HStack(spacing: 8) {
                        if downloading == file.id {
                            ProgressView().controlSize(.small)
                                .frame(width: 16)
                        } else {
                            Image(systemName: file.symbol)
                                .font(.geist(13))
                                .frame(width: 16)
                        }
                        Text(file.filename)
                            .font(.geist(13.5, .medium))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Text(file.readableSize)
                            .font(.geist(11.5))
                            .foregroundStyle(isMine ? Theme.Mine.meta : Theme.muted)
                        Spacer(minLength: 0)
                        }
                        #if os(macOS)
                        .padding(.trailing, 26)  // room for the download button
                        #endif
                    }
                    .foregroundStyle(isMine ? Theme.Mine.text : Theme.ink)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .background(isMine ? Theme.Mine.panel : Theme.Code.fill, in: RoundedRectangle(cornerRadius: 10))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(isMine ? Theme.Mine.stroke : Theme.Code.stroke, lineWidth: 1))
                }
                .buttonStyle(.plain)
                .disabled(downloading != nil)
                #if os(macOS)
                // Download, beside open: a picture can be looked at here *and* kept, and keeping
                // it should not need opening it first.
                .overlay(alignment: .bottomTrailing) {
                    Button { fetch(file, keep: true) } label: {
                        Image(systemName: "arrow.down.circle")
                            .font(.geist(15, .medium))
                            .foregroundStyle(isMine ? Theme.Mine.meta : Theme.secondary)
                            .frame(width: 30, height: 30)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(downloading != nil)
                    .padding(.trailing, 4)
                    .padding(.bottom, 2)
                    .help("Download \(file.filename)")
                    .accessibilityLabel("Download \(file.filename)")
                }
                #endif
            }
        }
        .padding(.top, 4)
        // What happens to a downloaded file is the system's decision, not this app's — it does not
        // open another agent's file itself. The two platforms disagree about what "hand it over"
        // means, and that is a real difference rather than a spelling one: a phone shares, a Mac
        // puts the file somewhere and shows you where.
        #if canImport(UIKit)
        .sheet(item: $saved) { url in ShareSheet(url: url) }
        // Pictures, video, sound, PDFs and plain text open inside the app (Quick Look), as on the
        // web; its share button saves to Photos or Files. Anything else is handed over as before.
        .quickLookPreview($previewing)
        #elseif canImport(AppKit)
        // The same in-app viewer on the Mac (the Quick Look panel), for the same files.
        .quickLookPreview($previewing)
        // Anything else, or a download asked for, goes where the person says — the save panel,
        // opening on Downloads — and is then shown in the Finder.
        .onChange(of: saved) { _, url in
            guard let url else { return }
            saved = nil
            keep(url)
        }
        #endif
    }

    /// Media and documents only — never markup (HTML, SVG) that could carry active content.
    static func viewable(_ url: URL) -> Bool {
        guard let type = UTType(filenameExtension: url.pathExtension) else { return false }
        if type.conforms(to: .html) || type.conforms(to: .svg) || type.conforms(to: .xml) { return false }
        return [UTType.image, .movie, .audiovisualContent, .audio, .pdf, .plainText].contains { type.conforms(to: $0) }
    }

    #if os(macOS)
    /// Copy a downloaded file to where the person chooses.
    private func keep(_ url: URL) {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = url.lastPathComponent
        panel.directoryURL = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let target = panel.url else { return }
        do {
            if FileManager.default.fileExists(atPath: target.path) { try FileManager.default.removeItem(at: target) }
            try FileManager.default.copyItem(at: url, to: target)
            NSWorkspace.shared.activateFileViewerSelecting([target])
        } catch {
            inbox.toast = "Could not save that file."
        }
    }
    #endif

    private func fetch(_ file: MessageAttachment, keep: Bool = false) {
        guard let me = account.me else { return }
        downloading = file.id
        Task {
            defer { downloading = nil }
            do {
                let data = try await account.client.downloadAttachment(identity: me.address, id: file.id)
                // Into this app's own temporary directory under the name it arrived with. The name
                // is already sanitised by the postbox; the last component is taken again here
                // because it is what a path join would act on.
                let name = (file.filename as NSString).lastPathComponent
                let url = FileManager.default.temporaryDirectory
                    .appendingPathComponent(name.isEmpty ? "attachment" : name)
                try data.write(to: url, options: .atomic)
                if Self.viewable(url), !keep { previewing = url } else { saved = url }
            } catch let failure as APIError {
                inbox.toast = failure.errorDescription ?? "Could not download that file."
            } catch {
                inbox.toast = "Could not download that file."
            }
        }
    }
}

/// So a URL can drive `.sheet(item:)`.
extension URL: @retroactive Identifiable {
    public var id: String { absoluteString }
}

#if canImport(UIKit)
private struct ShareSheet: UIViewControllerRepresentable {
    let url: URL

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: [url], applicationActivities: nil)
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
#endif
