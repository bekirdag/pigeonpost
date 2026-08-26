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

//  The modifiers that exist on one platform and have no counterpart on the other.
//
//  Named for what they are asking for rather than for the API they call, so a shared view can say
//  it once. Each is a no-op where the concept does not exist: a Mac has no navigation bar to give a
//  display mode to, no sheet detents, and no software keyboard to tell about capitalisation.
//
//  Toolbar placement is deliberately *not* here. `.cancellationAction` and `.confirmationAction`
//  exist on both and already mean the right thing in both — leading and trailing on a phone, the
//  correct corners of a Mac sheet — so the shared sheets use those instead of the `topBar…`
//  placements they were written with.

import SwiftUI

extension View {
    /// A title that sits on one line with the bar rather than above it.
    @ViewBuilder
    func inlineTitle() -> some View {
        #if os(iOS)
        navigationBarTitleDisplayMode(.inline)
        #else
        self
        #endif
    }

    /// A sheet that covers half the screen. A Mac sheet is sized by its content instead.
    @ViewBuilder
    func mediumDetent() -> some View {
        #if os(iOS)
        presentationDetents([.medium])
        #else
        self
        #endif
    }

    /// A field whose text is an address or a name, where an automatic capital is always wrong.
    @ViewBuilder
    func noAutocapitalize() -> some View {
        #if os(iOS)
        textInputAutocapitalization(.never)
        #else
        self
        #endif
    }

    /// What the keyboard's return key should say. There is no software keyboard on a Mac.
    @ViewBuilder
    func doneKey() -> some View {
        #if os(iOS)
        submitLabel(.done)
        #else
        self
        #endif
    }
}
