//  Incoming calls on the Mac, as notifications.
//
//  A Mac has no CallKit to borrow and does not need one: the app's own window already has a ring
//  screen. What it lacks is a way to be heard when that window is not in front, or the app is not
//  running at all. Both are a notification with the `CALL_RING` category — Answer and Decline on the
//  banner itself:
//
//  - App closed: the postbox sends an alert push with that category (Mac App Store build only; the
//    website build has no push entitlement). Answer opens the app straight into the call; Decline
//    declines with the ring token from the push, without opening anything.
//  - App running but not frontmost: the call page's ring is posted here as a local notification
//    with the identifier `call-<room>` — the same as the push's collapse id, so if both arrive they
//    replace each other instead of stacking. When the ring ends, it is removed.
//  - App frontmost: nothing is posted. The in-app ring screen is in front of the person already.
//
//  Contract: docs/call-alerting.md §5 (Mac).

import AppKit
import UserNotifications

@MainActor
final class MacCallAlerts: NSObject {
    static let shared = MacCallAlerts()

    static let category = "CALL_RING"
    static let answerAction = "ANSWER"
    static let declineAction = "DECLINE"

    /// Rooms this app has a ring notification out for, from either route.
    private var ringing: Set<String> = []

    /// Register the category (its two buttons) and start listening to the call page. Called at
    /// launch, before any notification can be answered.
    func start() {
        let answer = UNNotificationAction(identifier: Self.answerAction, title: "Answer", options: [.foreground])
        let decline = UNNotificationAction(identifier: Self.declineAction, title: "Decline", options: [.destructive])
        let ring = UNNotificationCategory(identifier: Self.category, actions: [answer, decline],
                                          intentIdentifiers: [], options: [])
        UNUserNotificationCenter.current().setNotificationCategories([ring])
        CallHost.shared.events = self
        #if DEBUG
        // Local call tests only: quiet (provisional) permission, which needs no dialog, so a test run
        // can post and inspect notifications without asking anybody anything.
        if CallHost.debugEnabled {
            UNUserNotificationCenter.current().requestAuthorization(options: [.provisional, .alert, .sound]) { granted, _ in
                NSLog("pigeonpost: provisional notification permission \(granted)")
            }
        }
        // `-simulate-call-response 'ANSWER {payload}'` (or DECLINE): a click on a `CALL_RING` push's
        // button, handed to the same handler the notification delegate calls, after
        // `-simulate-call-response-delay` seconds (default 5).
        let args = CommandLine.arguments
        if let flag = args.firstIndex(of: "-simulate-call-response"), flag + 1 < args.count,
           case let text = args[flag + 1],
           let space = text.firstIndex(of: " "),
           let payload = (try? JSONSerialization.jsonObject(with: Data(text[space...].utf8))) as? [String: Any],
           let ring = CallRing(push: payload) {
            let action = String(text[..<space])
            let delay = UserDefaults.standard.double(forKey: "simulate-call-response-delay")
            Task {
                try? await Task.sleep(nanoseconds: UInt64((delay > 0 ? delay : 5) * 1_000_000_000))
                NSLog("pigeonpost: simulated notification response \(action) room=\(ring.room)")
                self.respond(to: ring, action: action)
            }
        }
        #endif
        // Brought to the front while ringing: the window's ring screen takes over from the banner.
        NotificationCenter.default.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated {
                let shown = MacCallAlerts.shared.ringing
                guard !shown.isEmpty else { return }
                MacCallAlerts.shared.remove(rooms: shown)
            }
        }
    }

    static func identifier(_ room: String) -> String { "call-\(room)" }

    /// The words on a ring banner. The postbox writes the same ones on the push.
    static func body(for ring: CallRing) -> String {
        ring.isVideo ? "Incoming video call" : "Incoming voice call"
    }

    private func post(_ ring: CallRing) {
        let content = UNMutableNotificationContent()
        content.title = ring.callerName { PeerFace.displayName($0) }
        content.body = Self.body(for: ring)
        content.sound = .default
        content.categoryIdentifier = Self.category
        content.threadIdentifier = "calls"
        content.interruptionLevel = .timeSensitive
        var from: [String: Any] = [:]
        if let address = ring.fromAddress { from["address"] = address }
        if let handle = ring.fromHandle { from["handle"] = handle }
        var info: [String: Any] = ["type": "call_ring", "room": ring.room, "kind": ring.kind, "from": from]
        if let to = ring.to { info["to"] = to }
        content.userInfo = info
        ringing.insert(ring.room)
        let request = UNNotificationRequest(identifier: Self.identifier(ring.room), content: content, trigger: nil)
        let id = request.identifier
        UNUserNotificationCenter.current().add(request) { error in
            if let error {
                NSLog("pigeonpost: call notification \(id) not posted: \(error.localizedDescription)")
            } else {
                NSLog("pigeonpost: call notification \(id) posted")
                #if DEBUG
                MacCallAlerts.dumpDelivered()
                #endif
            }
        }
    }

    #if DEBUG
    /// Local call tests only: what Notification Center holds for this app, after a short settle.
    nonisolated static func dumpDelivered() {
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
            UNUserNotificationCenter.current().getDeliveredNotifications { list in
                let rows = list.map { "\($0.request.identifier)[\($0.request.content.categoryIdentifier)] \($0.request.content.title): \($0.request.content.body)" }
                NSLog("pigeonpost: delivered = \(rows)")
            }
        }
    }
    #endif

    private func remove(rooms: Set<String>) {
        let ids = rooms.map(Self.identifier)
        let centre = UNUserNotificationCenter.current()
        centre.removeDeliveredNotifications(withIdentifiers: ids)
        centre.removePendingNotificationRequests(withIdentifiers: ids)
        ringing.subtract(rooms)
        NSLog("pigeonpost: call notification removed \(ids.joined(separator: ","))")
        #if DEBUG
        Self.dumpDelivered()
        #endif
    }
}

extension MacCallAlerts: CallPageEvents {
    func callPage(_ type: String, _ message: [String: Any]) {
        switch type {
        case "ring":
            guard let ring = CallRing(page: message) else { return }
            // In front, the window's own ring screen is the ring.
            guard !NSApp.isActive else { return NSLog("pigeonpost: ring shown in the window room=\(ring.room)") }
            post(ring)
        case "ring_ended":
            guard let room = message["room"] as? String else { return }
            remove(rooms: [room])
        default:
            break
        }
    }
}

extension MacCallAlerts: CallNotificationHandling {
    /// A ring push while the app runs: a banner only when the window is not in front. In front, the
    /// window rings by itself and a banner over it would be the app interrupting itself.
    func presentation(for ring: CallRing) -> UNNotificationPresentationOptions {
        if NSApp.isActive { return [] }
        ringing.insert(ring.room)
        return [.banner, .list, .sound]
    }

    func respond(to ring: CallRing, action: String) {
        remove(rooms: [ring.room])
        switch action {
        case Self.answerAction:
            NSLog("pigeonpost: answer from the notification room=\(ring.room) token=\(ring.ringToken != nil)")
            // The button opens the app (a `.foreground` action), launching it if it was closed; the
            // window signs the call page in, and the answer waits in `CallHost` until it has.
            NSApp.activate(ignoringOtherApps: true)
            CallHost.shared.answer(room: ring.room, ringToken: ring.ringToken)
        case Self.declineAction:
            if let token = ring.ringToken {
                let page = CallHost.shared.pageURL
                Task {
                    let ok = await RingEndpoint.decline(page: page, room: ring.room, ringToken: token)
                    NSLog(ok ? "pigeonpost: declined from the notification room=\(ring.room)" : "pigeonpost: decline not delivered; the ring will time out")
                }
            } else {
                CallHost.shared.decline(room: ring.room, ringToken: nil)
            }
        default:
            // The banner itself: open the app. A ring still live rings in the window, whose page
            // joins it as soon as it connects.
            NSApp.activate(ignoringOtherApps: true)
        }
    }
}
