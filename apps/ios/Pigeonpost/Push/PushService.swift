//  Being told that mail arrived, without the app running.
//
//  The postbox does the pushing (`crates/pigeonpost-postbox/src/push.rs`); this is the half that
//  asks the system for a token, hands it over, and decides what happens when somebody taps the
//  notification. No SDK is involved and none should be: everything here is UserNotifications and
//  one POST.

import Foundation
import Observation
import UIKit
import UserNotifications

@MainActor
@Observable
final class PushService: NSObject {
    /// Where a tapped notification wants to go. The conversation list watches this and opens it.
    var pendingPeer: String?

    /// Whether this device has been asked yet. Asked once, on the first conversation opened, rather
    /// than at launch — a permission dialog in front of a signed-out sign-in screen is a dialog
    /// people decline, and a declined notification permission is expensive to get back.
    private(set) var hasAsked: Bool {
        get { UserDefaults.standard.bool(forKey: Self.askedKey) }
        set { UserDefaults.standard.set(newValue, forKey: Self.askedKey) }
    }
    private static let askedKey = "ppi_push_asked"

    /// The last token the system issued, kept so a mailbox switch can re-register without waiting
    /// for the system to hand it over again.
    private var deviceToken: String?
    private weak var account: Account?

    /// Which Apple minted the token. A build from Xcode talks to the sandbox; TestFlight and the
    /// App Store talk to production, and a token from one is meaningless to the other.
    static var environment: String {
        #if DEBUG
        return "sandbox"
        #else
        return "production"
        #endif
    }

    func attach(to account: Account) {
        self.account = account
        UNUserNotificationCenter.current().delegate = self
    }

    /// Ask, if this device has never been asked, and register either way when already granted.
    ///
    /// Called when a conversation is opened: by then the person has seen what the app is for, which
    /// is the only moment the question makes sense.
    func askIfNeeded() async {
        let centre = UNUserNotificationCenter.current()
        let settings = await centre.notificationSettings()
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral:
            UIApplication.shared.registerForRemoteNotifications()
        case .notDetermined:
            guard !hasAsked else { return }
            hasAsked = true
            let granted = (try? await centre.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
            if granted { UIApplication.shared.registerForRemoteNotifications() }
        case .denied:
            // Their answer. Asking again is what Settings is for.
            break
        @unknown default:
            break
        }
    }

    /// The system handed us a token. Give it to the postbox, against the mailbox being read.
    func adopt(deviceToken data: Data) async {
        let token = data.map { String(format: "%02x", $0) }.joined()
        deviceToken = token
        await register()
    }

    /// Re-register the held token — after a mailbox switch, so the phone rings for what is being
    /// watched rather than for whatever was open when the token arrived.
    func register() async {
        guard let account, let me = account.me, let token = deviceToken else { return }
        do {
            try await account.client.registerDevice(
                identity: me.address,
                token: token,
                environment: Self.environment
            )
        } catch {
            // Not fatal and not worth a toast: mail still arrives, the app still polls while it is
            // open, and the next launch tries again.
        }
    }

    /// Stop this device ringing for a mailbox it no longer holds.
    func unregister() async {
        guard let account, let token = deviceToken else { return }
        try? await account.client.unregisterDevice(token: token)
        deviceToken = nil
        clearBadge()
    }

    /// Clear the badge when the person is looking at the app; the count is only meaningful while
    /// they are not.
    func clearBadge() {
        UNUserNotificationCenter.current().setBadgeCount(0)
    }
}

extension PushService: UNUserNotificationCenterDelegate {
    /// A notification that lands while the app is open. Shown as a banner: the list only updates
    /// itself for the mailbox on screen, and mail for another of your mailboxes is exactly what you
    /// would want to be told about.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound]
    }

    /// Tapped. The payload carries the peer, so the app opens the conversation it is about rather
    /// than wherever it happened to be.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let peer = response.notification.request.content.userInfo["peer"] as? String
        await MainActor.run {
            self.pendingPeer = peer
            self.clearBadge()
        }
    }
}

/// The one thing SwiftUI has no equivalent for: `application(_:didRegisterForRemoteNotifications…)`
/// is a UIKit delegate callback and there is no other way to receive a device token.
final class PushDelegate: NSObject, UIApplicationDelegate {
    static let service = PushService()

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        Task { await Self.service.adopt(deviceToken: deviceToken) }
    }

    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        // Simulators before iOS 16 could not register at all, and a device in aeroplane mode cannot
        // either. Neither is worth interrupting somebody over.
        NSLog("push registration failed: \(error.localizedDescription)")
    }
}
