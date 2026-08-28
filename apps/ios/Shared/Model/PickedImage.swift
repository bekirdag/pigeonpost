//  A photo chosen from the library, given a name.
//
//  A file picked from Files arrives with one; a photo does not. `PhotosPickerItem` deliberately
//  hands back bytes and a content type and nothing else — the original filename lives on the asset,
//  and reading the asset means asking for the whole photo library, which is a permission this app
//  has no business holding to send one picture. So the name is made here.
//
//  It has to be a name a recipient can use. `image` is not: an agent that saves three of them
//  overwrites two, and an extension is what tells whoever opens it what it is holding.

import Foundation
import UniformTypeIdentifiers

enum PickedImage {
    /// `photo-20260828-131500.heic`, and `-2`, `-3` when several are sent at once.
    ///
    /// The second and later get the suffix rather than all of them, so the common case — one photo —
    /// reads as a photo rather than as the first of a batch.
    static func filename(for type: UTType?, index: Int, at date: Date) -> String {
        let stamp = DateFormatter.pickedImage.string(from: date)
        let suffix = index > 0 ? "-\(index + 1)" : ""
        return "photo-\(stamp)\(suffix).\(type?.preferredFilenameExtension ?? "jpeg")"
    }

    /// What the postbox is told it is holding. A photo library returns HEIC on a modern phone and
    /// JPEG on an older one, and passing either through unconverted is the honest thing: re-encoding
    /// somebody's picture to make it more convenient for the recipient loses detail they never
    /// agreed to lose.
    static func mediaType(for type: UTType?) -> String {
        type?.preferredMIMEType ?? "image/jpeg"
    }
}

private extension DateFormatter {
    /// Fixed locale and fixed zone: this is a filename, not something anybody reads as a date, and
    /// a Buddhist or Japanese calendar would put a different year in it on the same phone.
    static let pickedImage: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyyMMdd-HHmmss"
        return formatter
    }()
}
