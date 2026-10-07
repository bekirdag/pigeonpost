import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// A plain-text editor with native Paste support for clipboard images and copied files.
struct MacMessageEditor: NSViewRepresentable {
    @Binding var text: String
    let attach: ([StagedFile]) -> Void
    let reportError: (String) -> Void
    let send: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        let editor = AttachmentTextView()
        editor.isRichText = false
        editor.importsGraphics = false
        editor.drawsBackground = false
        editor.font = .systemFont(ofSize: 13)
        editor.textColor = NSColor(Theme.ink)
        editor.insertionPointColor = NSColor(Theme.ink)
        editor.textContainerInset = NSSize(width: 5, height: 7)
        editor.isVerticallyResizable = true
        editor.isHorizontallyResizable = false
        editor.autoresizingMask = [.width]
        editor.textContainer?.widthTracksTextView = true
        editor.setAccessibilityLabel("Write a message")
        editor.delegate = context.coordinator
        scroll.documentView = editor
        updateNSView(scroll, context: context)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.parent = self
        guard let editor = scroll.documentView as? AttachmentTextView else { return }
        if editor.string != text { editor.string = text }
        editor.isEditable = context.environment.isEnabled
        editor.attach = attach
        editor.reportError = reportError
        editor.send = send
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: NSScrollView, context: Context) -> CGSize? {
        guard let editor = nsView.documentView as? NSTextView,
              let container = editor.textContainer, let layout = editor.layoutManager else { return nil }
        let width = proposal.width ?? 300
        container.containerSize = NSSize(width: max(1, width - 10), height: .greatestFiniteMagnitude)
        layout.ensureLayout(for: container)
        return CGSize(width: width, height: min(160, max(32, layout.usedRect(for: container).height + 14)))
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: MacMessageEditor
        init(_ parent: MacMessageEditor) { self.parent = parent }
        func textDidChange(_ notification: Notification) {
            guard let editor = notification.object as? NSTextView else { return }
            parent.text = editor.string
        }
    }
}

final class AttachmentTextView: NSTextView {
    var attach: ([StagedFile]) -> Void = { _ in }
    var reportError: (String) -> Void = { _ in }
    var send: () -> Void = {}

    override func paste(_ sender: Any?) {
        if !pasteAttachments(from: .general) { super.paste(sender) }
    }

    /// Return false only for text so AppKit preserves selection, undo and text input behaviour.
    @discardableResult func pasteAttachments(from pasteboard: NSPasteboard) -> Bool {
        guard isEditable, MacClipboardFiles.containsAttachments(pasteboard) else { return false }
        do { attach(try MacClipboardFiles.read(pasteboard)) }
        catch { reportError(error.localizedDescription) }
        return true
    }

    override func validateUserInterfaceItem(_ item: NSValidatedUserInterfaceItem) -> Bool {
        if item.action == #selector(paste(_:)), isEditable, MacClipboardFiles.containsAttachments(.general) { return true }
        return super.validateUserInterfaceItem(item)
    }

    override func insertNewline(_ sender: Any?) {
        if hasMarkedText() || NSApp.currentEvent?.modifierFlags.contains(.shift) == true {
            super.insertNewline(sender)
        } else { send() }
    }
}

enum MacClipboardFiles {
    static let maxBytes = 25 * 1024 * 1024

    static func containsAttachments(_ pasteboard: NSPasteboard) -> Bool {
        pasteboard.availableType(from: [.fileURL, .png, .tiff]) != nil
    }

    static func read(_ pasteboard: NSPasteboard) throws -> [StagedFile] {
        // Finder may also supply a thumbnail and a text path. Prefer the original files.
        if let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL], !urls.isEmpty {
            return try urls.map(readFile)
        }
        return try (pasteboard.pasteboardItems ?? []).compactMap { item in
            if let png = item.data(forType: .png) {
                return try image(png)
            }
            guard let tiff = item.data(forType: .tiff) else { return nil }
            guard tiff.count <= maxBytes,
                  let bitmap = NSBitmapImageRep(data: tiff),
                  bitmap.pixelsWide * bitmap.pixelsHigh <= 32_000_000,
                  let png = bitmap.representation(using: .png, properties: [:]) else {
                throw ClipboardError("Could not paste that image. Choose a smaller image.")
            }
            return try image(png)
        }
    }

    static func readFile(_ url: URL) throws -> StagedFile {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let values = try url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
        guard values.isRegularFile == true else { throw ClipboardError("Choose files, not folders.") }
        guard let size = values.fileSize, size > 0, size <= maxBytes else {
            throw ClipboardError("Choose a file between 1 byte and 25 MB.")
        }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        let data = try handle.read(upToCount: maxBytes + 1) ?? Data()
        try validate(data)
        return StagedFile(name: url.lastPathComponent,
                          mediaType: UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream",
                          data: data)
    }

    private static func image(_ data: Data) throws -> StagedFile {
        try validate(data)
        return StagedFile(name: "image-\(UUID().uuidString.prefix(8)).png", mediaType: "image/png", data: data)
    }

    private static func validate(_ data: Data) throws {
        guard !data.isEmpty, data.count <= maxBytes else { throw ClipboardError("Choose a file between 1 byte and 25 MB.") }
    }

    struct ClipboardError: LocalizedError {
        let errorDescription: String?
        init(_ message: String) { errorDescription = message }
    }
}
