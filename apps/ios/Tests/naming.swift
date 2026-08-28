//  What a photo from the library is called by the time it leaves this app.
//
//  A file picked from Files brings its own name and this does not apply to it. A photo has none, so
//  one is made, and the making is worth pinning down: the name is the only thing the recipient has
//  to tell one picture from another and to know what it is holding.
//
//      cd apps/ios && ./Tests/naming.sh

import Foundation
import UniformTypeIdentifiers

@main
enum NamingTests {
    static var failures = 0

    static func equal(_ actual: String, _ expected: String, _ what: String) {
        if actual == expected {
            print("  ok   \(what)")
        } else {
            failures += 1
            print("  FAIL \(what) — got \(actual), wanted \(expected)")
        }
    }

    static func main() {
    // A fixed instant, so this asks about the format rather than about today.
    let at = Date(timeIntervalSince1970: 1_787_910_180)  // 2026-08-28 12:43:00 UTC
    let stamp = DateFormatter()
    stamp.locale = Locale(identifier: "en_US_POSIX")
    stamp.dateFormat = "yyyyMMdd-HHmmss"
    let expected = stamp.string(from: at)

    print("photo names")
    equal(
        PickedImage.filename(for: .heic, index: 0, at: at),
        "photo-\(expected).heic",
        "a phone's own format keeps its extension"
    )
    equal(
        PickedImage.filename(for: .jpeg, index: 0, at: at),
        "photo-\(expected).jpeg",
        "and so does an older one's"
    )
    // The first of a batch reads as a photo; the rest are numbered. Sending three at once must not
    // produce three files with one name, which is two of them lost on any machine that saves them.
    equal(
        PickedImage.filename(for: .png, index: 2, at: at),
        "photo-\(expected)-3.png",
        "the third of a batch is numbered"
    )
    equal(
        PickedImage.filename(for: nil, index: 0, at: at),
        "photo-\(expected).jpeg",
        "a picker that says nothing about the type still yields a usable name"
    )

    print("photo media types")
    equal(PickedImage.mediaType(for: .heic), "image/heic", "heic")
    equal(PickedImage.mediaType(for: .jpeg), "image/jpeg", "jpeg")
    equal(PickedImage.mediaType(for: .png), "image/png", "png")
    equal(PickedImage.mediaType(for: nil), "image/jpeg", "nothing known")

    if failures > 0 {
        print("\n\(failures) failed")
        exit(1)
    }
    print("\nall passed")
    }
}
