//  The device token, which only an AppKit delegate callback delivers.
//
//  The twin of `Pigeonpost/Push/PushDelegate.swift`. Not in `Shared/` for the same reason that one
//  is not: the application object and its delegate protocol genuinely differ between the platforms.
//  Everything either of them does with a token lives in `PushService`, which is shared.

import AppKit
import SwiftUI
import UserNotifications

// `NSApplicationDelegate` carries no actor isolation of its own, unlike its UIKit counterpart, and
// `PushService` is main-actor bound. Saying so here is the difference between this compiling and
// not.
@MainActor
final class MacPushDelegate: NSObject, NSApplicationDelegate {
    static let service = PushService()

    /// Before launch finishes, because a click on a notification's Answer button can be what launched
    /// the app, and the system hands that click to whichever delegate is set by then. Set later (when
    /// a view appears) and the click is lost. The call buttons are registered here for the same reason.
    func applicationWillFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = Self.service
        Self.service.calls = MacCallAlerts.shared
        MacCallAlerts.shared.start()
    }

    func application(
        _ application: NSApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        Task { await Self.service.adopt(deviceToken: deviceToken) }
    }

    func application(
        _ application: NSApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        // Expected on an unsigned local build, which has no APNs entitlement to register with. The
        // app still announces its own mail — see `LocalNotifier` — so this is not the difference
        // between hearing about a message and not.
        NSLog("push registration failed: \(error.localizedDescription)")
    }

    /// Clicking the Dock icon with every window closed should give the window back. Without this a
    /// closed window leaves the app running and apparently unreachable.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        true
    }
}
