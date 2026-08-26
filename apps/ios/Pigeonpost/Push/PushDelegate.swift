//  The device token, which only a UIKit delegate callback delivers.
//
//  Not in `Shared/`: the application object and its delegate protocol differ between the platforms,
//  and this is one of the few places where the difference is real rather than a matter of spelling.
//  The Mac target gets its own, ten lines long, against `NSApplicationDelegate`. Everything either
//  of them does with the token lives in `PushService`, which is shared.

import SwiftUI
import UIKit

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
