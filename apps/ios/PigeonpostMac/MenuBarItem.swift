//  Pigeonpost in the menu bar.
//
//  The status item a Mac app of this kind is expected to have: always there, quiet when there is
//  nothing to say, and carrying the count when there is. Docker's whale, Dropbox's box — same idea,
//  and the same expectation that clicking it brings the app back.
//
//  `NSStatusItem` rather than SwiftUI's `MenuBarExtra`: `MenuBarExtra` is a `Scene`, which makes it
//  awkward to drive from the inbox's unread count without threading state through the App, and it
//  cannot easily show the app's own icon beside a number. This is a dozen lines of AppKit that do
//  exactly what is wanted.

import AppKit

@MainActor
final class MenuBarItem {
    static let shared = MenuBarItem()

    private var item: NSStatusItem?

    private init() {}

    /// Put the icon in the bar and say how much mail is waiting.
    ///
    /// Called on every change to the count, including the first, so this both creates the item and
    /// keeps it honest. Zero is not hidden: an icon that vanishes when there is no mail is one
    /// nobody can click to get the window back, which is half of what it is for.
    func show(unread: Int) {
        let item = existing()
        guard let button = item.button else { return }

        // Template mode is what lets the system draw it correctly in a light bar, a dark bar, and
        // inverted while the menu is open. A coloured icon would look right in exactly one of those.
        // A silhouette, not the app icon: the colour version would look right in exactly one of the
        // three ways the system draws a status item — light bar, dark bar, and inverted while its
        // menu is open. The asset is `assets/img/logo_only_symbol_black.png`, trimmed to its ink
        // and squared; the white twin is not needed here, because a template image *is* the black
        // one and the system paints it whichever way the bar requires.
        let icon = NSImage(named: "MenuBarIcon")
        icon?.isTemplate = true
        icon?.size = NSSize(width: 17, height: 17)
        button.image = icon

        // The count sits beside the icon rather than replacing it, so the app is still recognisable
        // at a glance and the number is the thing that changed.
        button.title = unread > 0 ? " \(unread)" : ""
        button.imagePosition = unread > 0 ? .imageLeading : .imageOnly
        button.toolTip = unread > 0
            ? "Pigeonpost — \(unread) waiting"
            : "Pigeonpost"
    }

    private func existing() -> NSStatusItem {
        if let item { return item }
        let created = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        created.button?.target = self
        created.button?.action = #selector(open)
        item = created
        return created
    }

    /// Bring the window back, creating one if every window has been closed. Clicking a status item
    /// and having nothing happen is the way these usually disappoint.
    @objc private func open() {
        NSApp.activate(ignoringOtherApps: true)
        if let window = NSApp.windows.first(where: { $0.canBecomeMain }) {
            window.makeKeyAndOrderFront(nil)
        } else {
            // No window left to raise. This is what the Dock icon does, and the status item should
            // not be the one place in the app where reopening is impossible.
            NSApp.sendAction(Selector(("newDocument:")), to: nil, from: nil)
        }
    }
}
