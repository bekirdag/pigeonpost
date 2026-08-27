//  A notification this app posts about its own mail.
//
//  Remote push (`PushService`, and `crates/pigeonpost-postbox/src/push.rs` at the other end) is what
//  wakes a device that is asleep. This is the other half, and the one that matters on a desktop:
//  the app is running, it is already long-polling the postbox, and it knows about the message
//  before APNs would. Posting it here means the notification carries the app's own name and icon
//  and can be clicked, which is the whole complaint about announcing mail with `osascript`.
//
//  The payload is deliberately the same shape as the remote one — a `peer` in `userInfo` — so a
//  click lands in the same conversation whichever route the notification arrived by.

import Foundation
import UserNotifications

@MainActor
enum LocalNotifier {
    /// Announce one message. The identifier is the message id, so the same message announced twice
    /// replaces its own notification rather than stacking a duplicate.
    static func announce(title: String, subtitle: String, body: String, peer: String, messageId: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        // Which of the account's mailboxes it landed in. The postbox puts it in the same place on a
        // remote notification, and somebody holding a fleet cannot tell two of them apart without
        // it — "bdya wrote" is half a sentence when four mailboxes can be written to.
        content.subtitle = subtitle
        content.body = body
        content.sound = .default
        content.userInfo = ["peer": peer]
        // Grouped by conversation, so a burst from one sender collapses the way Mail and Messages
        // collapse theirs instead of filling the corner of the screen.
        content.threadIdentifier = peer
        let request = UNNotificationRequest(identifier: messageId, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request) { error in
            // Silence here would be indistinguishable from a notification nobody looked at, and the
            // usual cause — permission refused, or never asked for — is one a log line can settle.
            if let error {
                NSLog("pigeonpost: notification not posted: \(error.localizedDescription)")
            } else {
                NSLog("pigeonpost: notification posted for \(peer)")
            }
        }
    }
}
