//  Being told that mail arrived, without the app running.
//
//  The postbox does the pushing (`crates/pigeonpost-postbox/src/push.rs`); this is the half that
//  asks the system for a token, hands it over, and decides what happens when somebody taps the
//  notification. No SDK is involved and none should be: everything here is UserNotifications and
//  one POST.

import Foundation
import Observation
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
    /// The mailbox on screen, so a notification about something it is already showing can be left
    /// unposted and said in the app instead.
    private weak var inbox: Inbox?

    /// Which Apple minted the token, read from the build's own provisioning profile.
    ///
    /// A token belongs to exactly one APNs environment and is meaningless to the other, so getting
    /// this wrong is a push that never arrives and never explains itself. `#if DEBUG` looks like the
    /// answer and is a guess about what Xcode did at export time: the archive is signed
    /// `aps-environment: development` and the App Store export is supposed to rewrite it to
    /// `production`. The entitlement embedded in the profile is not a guess.
    ///
    /// No profile means the simulator, which cannot register for remote notifications anyway.
    static let environment: String = {
        // Two names for one file. iOS embeds `embedded.mobileprovision` in the bundle root; macOS
        // embeds `embedded.provisionprofile` under `Contents/`, which is not in the resource search
        // path, so it has to be named directly.
        let embedded: URL? = {
            #if canImport(UIKit)
            return Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision")
            #else
            let url = Bundle.main.bundleURL
                .appendingPathComponent("Contents/embedded.provisionprofile")
            return FileManager.default.fileExists(atPath: url.path) ? url : nil
            #endif
        }()
        guard let url = embedded,
              let data = try? Data(contentsOf: url),
              // The profile is CMS-signed with a plain plist inside it. Nothing here needs the
              // signature — the entitlement is the app's own, and it is being read for a hint about
              // which host to name, not for a security decision.
              let text = String(data: data, encoding: .isoLatin1),
              let start = text.range(of: "<?xml"),
              let end = text.range(of: "</plist>")
        else {
            #if DEBUG
            return "sandbox"
            #else
            return "production"
            #endif
        }
        let plist = String(text[start.lowerBound..<end.upperBound])
        guard let parsed = try? PropertyListSerialization.propertyList(
                  from: Data(plist.utf8), format: nil) as? [String: Any],
              let entitlements = parsed["Entitlements"] as? [String: Any],
              let aps = entitlements["aps-environment"] as? String
        else { return "production" }
        return aps == "development" ? "sandbox" : "production"
    }()

    func attach(to account: Account) {
        self.account = account
        UNUserNotificationCenter.current().delegate = self
    }

    /// Which inbox is on screen. Held weakly and only read from the delegate below, which needs to
    /// know what is already being looked at before it decides to interrupt it.
    func attach(to inbox: Inbox) {
        self.inbox = inbox
    }

    /// Ask for a token when permission already exists, without ever asking for permission.
    ///
    /// Called at launch. Permission can arrive from somewhere this app never sees — Settings, a
    /// restore, a reinstall that kept the grant — and until `registerForRemoteNotifications` is
    /// called there is no token to give the postbox. Waiting for a conversation to be opened before
    /// asking the system for one means somebody can allow notifications, sit on the list, and never
    /// be woken.
    func refreshRegistrationIfAuthorized() async {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral:
            RemoteNotifications.register()
        default:
            break
        }
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
            RemoteNotifications.register()
        case .notDetermined:
            guard !hasAsked else { return }
            hasAsked = true
            let granted = (try? await centre.requestAuthorization(options: [.alert, .sound, .badge])) ?? false
            if granted { RemoteNotifications.register() }
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
    /// A notification that lands while the app is open.
    ///
    /// Never as a system banner. This method is only ever called with the app in front of somebody,
    /// and a banner from the system there is the app being interrupted by an announcement about
    /// itself: it covers the navigation bar, it has to be dismissed, and at its worst it is a
    /// banner about the very message on screen.
    ///
    /// So nothing is presented, and the app says it itself — a line at the top of the screen that
    /// is already open, which can be tapped to go there and which knows not to appear for the
    /// conversation being read. Mail for another of the account's mailboxes still gets said; that
    /// is the case the system banner was really covering, and the only one it was right about.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        let content = notification.request.content
        let info = content.userInfo
        let peer = info["peer"] as? String
        let messageId = info["message_id"] as? String ?? notification.request.identifier
        await MainActor.run {
            guard let peer, let inbox = self.inbox else { return }
            inbox.tell(
                remote: peer,
                // The postbox writes the sender's handle into the title and the mailbox it landed
                // in into the subtitle; both are worth more than "New message" and neither is
                // rebuilt here, so a notification says the same thing whichever way it arrived.
                title: content.subtitle.isEmpty
                    ? PeerFace.displayName(content.title)
                    : "\(PeerFace.displayName(content.title)) → \(PeerFace.displayName(content.subtitle))",
                body: content.body,
                messageId: messageId
            )
        }
        return []
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
