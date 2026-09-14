// Native NSHostingView integration test of the actual MacInboxView and shared Inbox.
// Unrelated sheets/notifications are inert test doubles; no Keychain or real server is used.
import AppKit
import SwiftUI
import Observation

@MainActor @Observable final class PushService {
    var pendingPeer: String?
    func attach(to account: Account) {}
    func attach(to inbox: Inbox) {}
    // A delayed notification authorization must never block inbox loading.
    func askIfNeeded() async { try? await Task.sleep(for: .seconds(60)) }
}
enum LocalNotifier {
    static func announce(title: String, subtitle: String, body: String, peer: String, messageId: String) {}
}
enum DockBadge { static func show(_ count: Int) {} }
@MainActor final class MenuBarItem {
    static let shared = MenuBarItem()
    func show(unread: Int) {}
}
struct PillView: View {
    enum Kind { case held }
    let text: String
    let kind: Kind
    var body: some View { Text(text) }
}
struct UnreadBadge: View {
    let count: Int
    let inverted: Bool
    var body: some View { Text(String(count)) }
}
struct SettingsSheet: View { var body: some View { Text("Settings") } }
struct PeerInfoSheet: View {
    let conversation: Conversation
    let visit: (Mailbox) -> Void
    var body: some View { Text(conversation.name) }
}
struct MacThreadView: View {
    let peer: String
    let subthread: String?
    var body: some View { Text(peer) }
}
struct MacNewConversationSheet: View {
    let started: (String) -> Void
    var body: some View { Text("New conversation") }
}
struct MacNewThreadSheet: View {
    let peer: String
    let started: (String) -> Void
    var body: some View { Text("New thread") }
}
extension View {
    func announcements(_ value: Binding<Inbox.Announcement?>, open: @escaping (String) -> Void) -> some View { self }
}
extension Notification.Name {
    static let newConversation = Notification.Name("pigeonpost.newConversation")
    static let refreshInbox = Notification.Name("pigeonpost.refreshInbox")
}

@main @MainActor struct MacInboxLoadingTests {
    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        URLProtocol.registerClass(ControlledURLProtocol.self)
        let (account, inbox) = InboxLoadingTests.make()
        let push = PushService()
        let view = MacInboxView().environment(account).environment(inbox).environment(push)
        let host = NSHostingView(rootView: view)
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 1100, height: 720),
                              styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.contentView = host
        window.makeKeyAndOrderFront(nil)
        Task {
            let a = InboxLoadingTests.a
            let b = InboxLoadingTests.b
            await InboxLoadingTests.pending(a.address, count: 5)
            InboxLoadingTests.check(inbox.loading, "Mac inbox loads while notification permission is still pending")
            await settle()
            snapshot(host, name: "mac-inbox-loading-a")

            // This is the observable account change made by both Visit Inbox and the picker.
            let switched = Date()
            account.act(as: b)
            await InboxLoadingTests.pending(b.address, count: 5)
            InboxLoadingTests.check(Date().timeIntervalSince(switched) < 1,
                                    "SwiftUI starts the selected inbox without waiting for the old poll")
            InboxLoadingTests.check(!ControlledURLProtocol.requests().contains {
                ControlledURLProtocol.identity($0) == a.address
            }, "SwiftUI cancelled all previous mailbox requests")
            await settle()
            snapshot(host, name: "mac-inbox-loading-b")
            InboxLoadingTests.finish(b.address, marker: "current")
            await InboxLoadingTests.wait("B ready") { inbox.hasLoaded && !inbox.loading }
            await InboxLoadingTests.pending(b.address, count: 1)
            InboxLoadingTests.check(inbox.messages.first?.messageId == "current", "selected mailbox data appears")
            await settle()
            snapshot(host, name: "mac-inbox-ready")

            account.act(as: a)
            await InboxLoadingTests.pending(a.address, count: 5)
            InboxLoadingTests.finish(a.address, marker: "failed", inboxStatus: 503)
            await InboxLoadingTests.wait("failure displayed") { inbox.offline && !inbox.loading }
            await settle()
            snapshot(host, name: "mac-inbox-retry")
            NotificationCenter.default.post(name: .refreshInbox, object: nil)
            await InboxLoadingTests.pending(a.address, count: 6)
            InboxLoadingTests.check(inbox.loading, "native Refresh command starts a visible retry")
            InboxLoadingTests.finish(a.address, marker: "empty", empty: true)
            await InboxLoadingTests.wait("empty ready") { inbox.hasLoaded && !inbox.loading }
            InboxLoadingTests.check(!inbox.offline && inbox.visible.isEmpty, "successful empty inbox is distinguished from failure")
            await settle()
            snapshot(host, name: "mac-inbox-empty")
            print("Mac inbox integration: \(InboxLoadingTests.checks) checks, \(InboxLoadingTests.failures) failures")
            window.orderOut(nil)
            exit(InboxLoadingTests.failures == 0 ? 0 : 1)
        }
        app.run()
    }

    static func settle() async {
        // Main-actor sleeps let AppKit lay out and animate; a blocked UI cannot complete this.
        for _ in 0..<10 { try? await Task.sleep(for: .milliseconds(30)) }
    }

    static func snapshot(_ view: NSView, name: String) {
        guard let directory = ProcessInfo.processInfo.environment["PIGEONPOST_UI_TEST_OUTPUT"] else { return }
        if let number = view.window?.windowNumber {
            let capture = Process()
            capture.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
            capture.arguments = ["-x", "-o", "-l", String(number), directory + "/" + name + "-window.png"]
            try? capture.run()
            capture.waitUntilExit()
        }
        view.layoutSubtreeIfNeeded()
        guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        if let data = bitmap.representation(using: .png, properties: [:]) {
            try? data.write(to: URL(fileURLWithPath: directory).appendingPathComponent(name + ".png"))
        }
    }
}
