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
        (nsView.documentView as? AttachmentTextView)?.fittingSize(width: proposal.width)
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
    private var measuredText = ""
    private var measuredFont: NSFont?
    private var measuredParagraph: NSParagraphStyle?
    private var measuredInsets = NSSize.zero
    private var measuredPadding: CGFloat = -1
    private var measuredSizes: [CGFloat: CGSize] = [:]

    func fittingSize(width proposedWidth: CGFloat?) -> CGSize? {
        // SwiftUI probes zero, ideal and infinite widths during a single layout pass. Never
        // change the live, width-tracking text container here: forcing its layout resizes the
        // NSTextView and retiles its NSScrollView while SwiftUI is still measuring (Sonoma hang).
        let width = max(0, proposedWidth.flatMap { $0.isFinite ? $0 : nil } ?? 300)
        guard width > 0 else { return CGSize(width: 0, height: 32) }
        let font = self.font ?? .systemFont(ofSize: 13)
        let paragraph = defaultParagraphStyle ?? .default
        let padding = textContainer?.lineFragmentPadding ?? 5
        if measuredText != string || measuredFont != font || measuredParagraph != paragraph
            || measuredInsets != textContainerInset || measuredPadding != padding {
            measuredText = string
            measuredFont = font
            measuredParagraph = paragraph
            measuredInsets = textContainerInset
            measuredPadding = padding
            measuredSizes.removeAll(keepingCapacity: true)
        }
        let scrollerWidth: CGFloat
        if let scroll = enclosingScrollView, scroll.hasVerticalScroller, scroll.scrollerStyle == .legacy {
            scrollerWidth = NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy)
        } else { scrollerWidth = 0 }
        let textWidth = max(1, width - scrollerWidth - 2 * textContainerInset.width)
        if let cached = measuredSizes[textWidth] { return CGSize(width: width, height: cached.height) }

        // A separate TextKit stack has no view to resize. Lay out only enough lines to fill
        // the capped composer; the real editor retains the entire draft and scrolls normally.
        let maximumHeight: CGFloat = 160
        let verticalInset = 2 * textContainerInset.height
        let storage = NSTextStorage(attributedString: NSAttributedString(string: string,
            attributes: [.font: font, .paragraphStyle: paragraph]))
        let layout = NSLayoutManager()
        layout.backgroundLayoutEnabled = false
        let container = NSTextContainer(size: NSSize(width: textWidth, height: max(1, maximumHeight - verticalInset)))
        container.lineFragmentPadding = padding
        layout.addTextContainer(container)
        storage.addLayoutManager(layout)
        layout.ensureLayout(for: container)
        let height: CGFloat
        if layout.firstUnlaidCharacterIndex() < storage.length {
            height = maximumHeight
        } else {
            // Include the insertion line after a trailing newline, which has no glyph of its own.
            let used = max(layout.usedRect(for: container).maxY, layout.extraLineFragmentRect.maxY)
            height = min(maximumHeight, max(32, ceil(used + verticalInset)))
        }
        let size = CGSize(width: width, height: height)
        // Remember SwiftUI's few width probes, without growing a cache throughout window resizing.
        if measuredSizes.count >= 4 { measuredSizes.removeAll(keepingCapacity: true) }
        measuredSizes[textWidth] = size
        return size
    }

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
