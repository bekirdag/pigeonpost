//  The few places the two platforms spell the same thing differently.
//
//  Kept deliberately small. A shim that grows becomes a third platform to reason about, so anything
//  that is genuinely a different *design* on the Mac belongs in that target's views rather than
//  behind a name that pretends the difference is only spelling.

#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

enum Clipboard {
    /// Put text on the pasteboard. AppKit needs the old contents cleared first, which UIKit does
    /// on assignment — the one real difference between them here.
    static func copy(_ text: String) {
        #if canImport(UIKit)
        UIPasteboard.general.string = text
        #elseif canImport(AppKit)
        let board = NSPasteboard.general
        board.clearContents()
        board.setString(text, forType: .string)
        #endif
    }
}

/// Asking the system to register this process for remote notifications.
///
/// UserNotifications is shared — the permission prompt, the delegate, the badge — but the call that
/// says "give me a device token" belongs to the application object, and the two platforms have
/// different ones. That is the whole difference; the postbox sees a token either way and a Mac row
/// in `devices` is the same `POST /v1/devices` as a phone's.
enum RemoteNotifications {
    static func register() {
        #if canImport(UIKit)
        UIApplication.shared.registerForRemoteNotifications()
        #elseif canImport(AppKit)
        NSApplication.shared.registerForRemoteNotifications()
        #endif
    }
}
