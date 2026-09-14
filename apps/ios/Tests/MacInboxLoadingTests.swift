// Native NSHostingView integration test of the actual MacInboxView and shared Inbox.
// Unrelated sheets/notifications are inert test doubles; no Keychain or real server is used.
import AppKit
import ApplicationServices
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
struct SettingsSheet: View { var body: some View { Text("Settings") } }
struct PeerInfoSheet: View {
    let conversation: Conversation
    let visit: (Mailbox) -> Void
    var body: some View { Text(conversation.name) }
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
extension Notification.Name {
    static let newConversation = Notification.Name("pigeonpost.newConversation")
    static let refreshInbox = Notification.Name("pigeonpost.refreshInbox")
}

@main @MainActor struct MacInboxLoadingTests {
    static func main() {
        setbuf(stdout, nil)
        let desktopSession = CGSessionCopyCurrentDictionary() as? [String: Any]
        guard desktopSession?["CGSSessionScreenIsLocked"] as? Bool != true else {
            FileHandle.standardError.write(Data("Native inbox tests require an unlocked macOS desktop session\n".utf8))
            exit(69)
        }
        let watchdog = DispatchSource.makeTimerSource(queue: .global(qos: .userInitiated))
        watchdog.schedule(deadline: .now() + 90)
        watchdog.setEventHandler {
            FileHandle.standardError.write(Data("Native inbox UI watchdog timed out\n".utf8))
            exit(124)
        }
        watchdog.resume()
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)
        URLProtocol.registerClass(ControlledURLProtocol.self)
        let (account, inbox) = InboxLoadingTests.make()
        let push = PushService()
        let view = MacInboxView().environment(account).environment(inbox).environment(push)
        let host = NSHostingView(rootView: view)
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 1100, height: 720),
                              styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.contentView = host
        window.makeKeyAndOrderFront(nil)
        app.activate(ignoringOtherApps: true)
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
            await repeatedPickerSwitches(window: window, host: host, account: account, inbox: inbox)
            print("Mac inbox integration: \(InboxLoadingTests.checks) checks, \(InboxLoadingTests.failures) failures")
            watchdog.cancel()
            window.orderOut(nil)
            exit(InboxLoadingTests.failures == 0 ? 0 : 1)
        }
        app.run()
    }

    static func settle() async {
        // Main-actor sleeps let AppKit lay out and animate; a blocked UI cannot complete this.
        for _ in 0..<10 { try? await Task.sleep(for: .milliseconds(30)) }
    }

    static func repeatedPickerSwitches(window: NSWindow, host: NSView, account: Account, inbox: Inbox) async {
        for index in 0..<12 {
            let target = index.isMultiple(of: 2) ? InboxLoadingTests.b : InboxLoadingTests.a
            let currentLabel = PeerFace.displayName(account.me!.key)
            print("Picker cycle \(index): opening \(currentLabel)")
            await click(x: 40, y: 14, window: window, host: host)
            await settle()
            snapshot(host, name: "mac-inbox-picker-open")
            await click(x: 70, y: target.address == InboxLoadingTests.a.address ? 44 : 74, window: window, host: host)
            await InboxLoadingTests.wait("picker selects \(target.key)") { account.me?.address == target.address }
            await InboxLoadingTests.pending(target.address, count: 5)
            InboxLoadingTests.check(inbox.loading && !inbox.hasLoaded, "picker switch shows loading without stale data")
            let marker = "picker-\(index)"
            InboxLoadingTests.finish(target.address, marker: marker, inboxBody: stressMessages(marker))
            await InboxLoadingTests.wait("picker data ready") { inbox.hasLoaded && !inbox.loading }
            await settle()
            await click(x: 90, y: 54, window: window, host: host)
            if index.isMultiple(of: 2) { await settle() }
            InboxLoadingTests.check(inbox.reading == "/test/peer", "conversation opens through the native List")
            await click(x: 350, y: 73, window: window, host: host)
            if index.isMultiple(of: 2) {
                await settle()
                InboxLoadingTests.check(hasVisibleMessage(in: host), "long subject renders message bubbles instead of a blank viewport")
            }
            InboxLoadingTests.check(account.me?.address == target.address && inbox.messages.first?.messageId == marker,
                                    "native picker cycle \(index) remains responsive on the selected mailbox")
        }
        await settle()
        InboxLoadingTests.check(hasVisibleMessage(in: host), "last switched subject has visible messages")
        snapshot(host, name: "mac-inbox-picker-stress")
    }

    static func hasVisibleMessage(in host: NSView) -> Bool {
        host.layoutSubtreeIfNeeded()
        guard let bitmap = host.bitmapImageRepForCachingDisplay(in: host.bounds) else { return false }
        host.cacheDisplay(in: host.bounds, to: bitmap)
        var ground: NSColor?
        host.effectiveAppearance.performAsCurrentDrawingAppearance {
            ground = NSColor(Theme.ground).usingColorSpace(.deviceRGB)
        }
        guard let ground else { return false }
        // Inspect only the conversation viewport, excluding the toolbar, composer and scrollbar.
        // Its raised bubble surface is distinct from the recessed empty conversation background.
        let scaleX = CGFloat(bitmap.pixelsWide) / host.bounds.width
        let scaleY = CGFloat(bitmap.pixelsHigh) / host.bounds.height
        var bubblePixels = 0
        for y in stride(from: Int(90 * scaleY), to: bitmap.pixelsHigh - Int(90 * scaleY), by: 8) {
            for x in stride(from: Int(540 * scaleX), to: bitmap.pixelsWide - Int(60 * scaleX), by: 8) {
                guard let pixel = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) else { continue }
                if pixel.alphaComponent > 0.9,
                   abs(pixel.redComponent - ground.redComponent) < 0.01,
                   abs(pixel.greenComponent - ground.greenComponent) < 0.01,
                   abs(pixel.blueComponent - ground.blueComponent) < 0.01 {
                    bubblePixels += 1
                }
            }
        }
        return bubblePixels > 100
    }

    static func stressMessages(_ marker: String) -> String {
        let rows: [[String: Any]] = (0..<360).map { index in
            ["message_id": index == 0 ? marker : "\(marker)-\(index)", "from": "/k/peer", "peer": "/test/peer",
             "body": String(repeating: "Regression message \(index), with a longer paragraph to exercise text wrapping and scrolling.\n", count: 30),
             "direction": "in", "read": true, "received_at": index + 1,
             "thread_id": index.isMultiple(of: 2) ? "topic-one" : "topic-two"]
        }
        return String(data: try! JSONSerialization.data(withJSONObject: ["messages": rows]), encoding: .utf8)!
    }

    static func click(x: CGFloat, y: CGFloat, window: NSWindow, host: NSView) async {
        NSApplication.shared.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
        await InboxLoadingTests.wait("native test window is active") { NSApplication.shared.isActive && window.isKeyWindow }
        // Dispatch through AppKit's event queue and yield between down/up, just as its normal
        // event loop does; sending both synchronously skips SwiftUI gesture recognition.
        let point = host.convert(NSPoint(x: x, y: host.isFlipped ? y : host.bounds.height - y), to: nil)
        for type in [NSEvent.EventType.leftMouseDown, .leftMouseUp] {
            let event = NSEvent.mouseEvent(with: type, location: point, modifierFlags: [],
                                          timestamp: ProcessInfo.processInfo.systemUptime,
                                          windowNumber: window.windowNumber, context: nil,
                                          eventNumber: 0, clickCount: 1, pressure: 1)!
            NSApplication.shared.postEvent(event, atStart: false)
            try? await Task.sleep(for: .milliseconds(40))
        }
    }

    static func snapshot(_ view: NSView, name: String) {
        guard let directory = ProcessInfo.processInfo.environment["PIGEONPOST_UI_TEST_OUTPUT"], !directory.isEmpty else { return }
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
