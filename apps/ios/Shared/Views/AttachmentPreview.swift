import SwiftUI
import QuickLookThumbnailing
import UniformTypeIdentifiers

/// A bounded, static system thumbnail. Documents never become embedded web content.
struct AttachmentPreview: View {
    let name: String
    let mediaType: String
    let bytes: Int
    let cacheKey: String
    var compact = false
    let load: () async throws -> Data
    @State private var image: CGImage?

    var body: some View {
        if AttachmentThumbnails.supports(name: name, mediaType: mediaType), bytes <= AttachmentThumbnails.maxBytes {
            ZStack {
                RoundedRectangle(cornerRadius: 8).fill(Theme.wash)
                if let image {
                    Image(decorative: image, scale: 1).resizable().scaledToFit()
                } else {
                    Image(systemName: mediaType.hasPrefix("video/") ? "play.rectangle" : "doc.richtext")
                        .foregroundStyle(Theme.muted)
                }
                if mediaType.hasPrefix("video/") {
                    Image(systemName: "play.circle.fill").font(.title2).foregroundStyle(.white, .black.opacity(0.65))
                }
            }
            .frame(width: compact ? 60 : 220, height: compact ? 48 : 150)
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .accessibilityLabel("Preview of \(name)")
            .task(id: cacheKey) {
                image = nil
                guard let result = try? await AttachmentThumbnails.load(key: cacheKey, name: name, mediaType: mediaType, data: load),
                      !Task.isCancelled else { return }
                image = result
            }
        }
    }
}

@MainActor
enum AttachmentThumbnails {
    static let maxBytes = 25 * 1024 * 1024
    private final class Entry: NSObject { let image: CGImage; init(_ image: CGImage) { self.image = image } }
    private static let cache: NSCache<NSString, Entry> = {
        let result = NSCache<NSString, Entry>()
        result.totalCostLimit = 24 * 1024 * 1024
        result.countLimit = 96
        return result
    }()
    private static var active = 0

    static func supports(name: String, mediaType: String) -> Bool {
        let mime = mediaType.lowercased().split(separator: ";").first.map(String.init) ?? ""
        let type = (mime.isEmpty || mime == "application/octet-stream")
            ? UTType(filenameExtension: (name as NSString).pathExtension) : UTType(mimeType: mime)
        guard let type, !type.conforms(to: .html), type.identifier != "public.svg-image" else { return false }
        return type.conforms(to: .image) || type.conforms(to: .movie) || type.conforms(to: .audio)
            || type.conforms(to: .pdf) || type.conforms(to: .plainText)
    }

    static func load(key: String, name: String, mediaType: String, data: () async throws -> Data) async throws -> CGImage? {
        if let hit = cache.object(forKey: key as NSString) { return hit.image }
        while active >= 3 { try await Task.sleep(nanoseconds: 50_000_000) }
        try Task.checkCancellation()
        active += 1
        defer { active -= 1 }
        if let hit = cache.object(forKey: key as NSString) { return hit.image }
        let bytes = try await data()
        guard !bytes.isEmpty, bytes.count <= maxBytes else { return nil }
        try Task.checkCancellation()
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let ext = UTType(mimeType: mediaType)?.preferredFilenameExtension ?? (name as NSString).pathExtension
        let url = folder.appendingPathComponent("preview").appendingPathExtension(ext.filter { $0.isLetter || $0.isNumber }.prefix(12).description)
        try bytes.write(to: url, options: .atomic)
        let request = QLThumbnailGenerator.Request(fileAt: url, size: CGSize(width: 440, height: 300), scale: 1, representationTypes: .thumbnail)
        let image: CGImage? = await withCheckedContinuation { continuation in
            QLThumbnailGenerator.shared.generateBestRepresentation(for: request) { thumbnail, _ in
                continuation.resume(returning: thumbnail?.cgImage)
            }
        }
        try Task.checkCancellation()
        if let image { cache.setObject(Entry(image), forKey: key as NSString, cost: image.bytesPerRow * image.height) }
        return image
    }
}
