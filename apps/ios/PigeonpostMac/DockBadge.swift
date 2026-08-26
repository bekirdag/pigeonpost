//  The red circle on the Dock icon.

import AppKit

/// `NSApp.dockTile.badgeLabel` rather than `UNUserNotificationCenter.setBadgeCount`.
///
/// The two look identical on the Dock and differ in what they depend on: the notification centre's
/// badge is part of the notification permission, so declining alerts also silently removes the
/// count. A number on your own Dock icon is a fact about the app's own window, and nothing worth
/// asking permission for.
///
/// AppKit draws the circle, so the digits are the system's own and match every other app's badge
/// rather than approximating one.
@MainActor
enum DockBadge {
    static func show(_ count: Int) {
        // nil, not "0" or "": a label of any kind draws a circle, and an empty red dot on the Dock
        // says there is something waiting when there is not.
        NSApp.dockTile.badgeLabel = count > 0 ? String(count) : nil
    }
}
