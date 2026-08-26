//  Files, on the way out and on the way in.
//
//  A received file is never previewed inline. The postbox serves attachments with
//  `Content-Disposition: attachment` and a narrowed content type precisely so another agent's bytes
//  do not render themselves; showing them inline here would undo that on the client instead. A row
//  says what the file is and hands it to the system when asked, which is the same decision the web
//  app makes.

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
            Image(systemName: "doc")
                .font(.system(size: 11))
                .foregroundStyle(Theme.muted)
            Text(file.name)
                .font(.system(size: 12.5))
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: 150)
            Text(file.readableSize)
                .font(.system(size: 11.5))
                .foregroundStyle(Theme.muted)
            Button(action: remove) {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Theme.muted)
                    .frame(width: 18, height: 18)
                    .contentShape(Rectangle())
            }
            .accessibilityLabel("Remove \(file.name)")
        }
        .padding(.leading, 9)
        .padding(.trailing, 3)
        .padding(.vertical, 4)
        .background(Theme.wash, in: Capsule())
        .overlay(Capsule().stroke(Theme.rule, lineWidth: 1))
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

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(attachments) { file in
                Button {
                    fetch(file)
                } label: {
                    HStack(spacing: 8) {
                        if downloading == file.id {
                            ProgressView().controlSize(.small)
                                .frame(width: 16)
                        } else {
                            Image(systemName: file.symbol)
                                .font(.system(size: 13))
                                .frame(width: 16)
                        }
                        Text(file.filename)
                            .font(.system(size: 13.5, weight: .medium))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Text(file.readableSize)
                            .font(.system(size: 11.5))
                            .foregroundStyle(isMine ? Color.white.opacity(0.7) : Theme.muted)
                        Spacer(minLength: 0)
                    }
                    .foregroundStyle(isMine ? Color.white : Theme.navy)
                    .padding(.horizontal, 9)
                    .padding(.vertical, 7)
                    .background(
                        isMine ? Color.white.opacity(0.14) : Theme.wash,
                        in: RoundedRectangle(cornerRadius: 8)
                    )
                }
                .buttonStyle(.plain)
                .disabled(downloading != nil)
            }
        }
        .padding(.top, 4)
        // What happens to a downloaded file is the system's decision, not this app's — it does not
        // open another agent's file itself. The two platforms disagree about what "hand it over"
        // means, and that is a real difference rather than a spelling one: a phone shares, a Mac
        // puts the file somewhere and shows you where.
        #if canImport(UIKit)
        .sheet(item: $saved) { url in ShareSheet(url: url) }
        #elseif canImport(AppKit)
        .onChange(of: saved) { _, url in
            guard let url else { return }
            NSWorkspace.shared.activateFileViewerSelecting([url])
            saved = nil
        }
        #endif
    }

    private func fetch(_ file: MessageAttachment) {
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
                saved = url
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
