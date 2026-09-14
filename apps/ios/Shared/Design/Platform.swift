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

/// Whether the app is in front of the person right now.
///
/// The question every notification has to answer before it is posted. A system banner over the app
/// that is already showing the message is the notification nobody wants — and on a Mac, where the
/// app hears about mail before APNs does, it was the only kind there was.
@MainActor
enum AppLife {
    static var isActive: Bool {
        #if canImport(UIKit)
        return UIApplication.shared.applicationState == .active
        #elseif canImport(AppKit)
        return NSApplication.shared.isActive
        #else
        return true
        #endif
    }
}

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

/// Copy the complete address, independently of any surrounding mailbox selector.
struct CopyAddressButton: View {
    let address: String
    @State private var copied = false
    @State private var revision = 0

    var body: some View {
        Button {
            Clipboard.copy(address)
            copied = true
            revision += 1
        } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc")
                .foregroundStyle(copied ? Color.accentColor : Color.secondary)
                #if os(iOS)
                .frame(width: 44, height: 44)
                #else
                .frame(width: 28, height: 28)
                #endif
                .contentShape(Rectangle())
        }
        .buttonStyle(.borderless)
        .disabled(address.isEmpty)
        .help(copied ? "Address copied" : "Copy address")
        .accessibilityLabel("Copy address \(address)")
        .accessibilityValue(copied ? "Copied" : "")
        .accessibilityIdentifier("copy-address:\(address)")
        .onChange(of: address) { _, _ in copied = false; revision += 1 }
        .task(id: revision) {
            guard copied else { return }
            do { try await Task.sleep(for: .seconds(2)) } catch { return }
            copied = false
        }
    }
}

struct PostAddressRow: View {
    let address: String

    var body: some View {
        HStack(spacing: 4) {
            Text(address)
                .font(.callout.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(2)
                .truncationMode(.middle)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            CopyAddressButton(address: address)
        }
    }
}

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
