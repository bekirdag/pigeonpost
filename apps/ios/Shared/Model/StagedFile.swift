//  A file chosen but not yet sent.
//
//  Read into memory at the moment it is picked, not at the moment it is sent. The document picker
//  hands back a URL into another process's sandbox and the permission to read it is scoped to that
//  callback — holding the URL and opening it later is how a file becomes unreadable exactly when
//  somebody presses send.

import Foundation

struct StagedFile: Identifiable, Equatable {
    let id = UUID()
    let name: String
    let mediaType: String
    let data: Data

    var readableSize: String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        formatter.allowedUnits = [.useKB, .useMB, .useGB]
        return formatter.string(fromByteCount: Int64(data.count))
    }
}

enum DroppedFile {
    /// `NSItemProvider` supplies `public.file-url` as a URL data representation, not reliably as a
    /// bridged `URL` object. Decode that representation first, with its textual form as a fallback
    /// for providers that serialize the absolute string directly.
    static func url(from data: Data) -> URL? {
        URL(dataRepresentation: data, relativeTo: nil)
            ?? String(data: data, encoding: .utf8).flatMap(URL.init(string:))
    }
}
