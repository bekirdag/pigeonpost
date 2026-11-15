//  Where the widget snapshot lives: one item in a keychain access group shared by the app and the
//  widget extension, and by nothing else.
//
//  A keychain item rather than a file in an App Group container: App Groups cannot be registered or
//  assigned to an App ID through the App Store Connect API, while a team-prefixed keychain access
//  group is already covered by every profile this team issues (`AH277897AV.*`). The protection is
//  the same class a file would get — `AfterFirstUnlockThisDeviceOnly` is the keychain's
//  `completeUntilFirstUserAuthentication`: unreadable until the phone has been unlocked once after
//  a restart, never synced, never in a backup that leaves the device.
//
//  The group (`<team>.dev.pigeonpost.widgets`) holds only this item. The app's tokens stay in its own
//  default group, which the extension's entitlements do not name, so the extension cannot read them.
//
//  The file functions below serve the tests (and any later move to a shared container).

import Foundation
import Security

enum WidgetStore {
    /// The kinds the extension registers, so the app can reload exactly what changed.
    enum Kind {
        static let inbox = "dev.pigeonpost.widgets.inbox"
        static let quickActions = "dev.pigeonpost.widgets.quick-actions"
        static let nests = "dev.pigeonpost.widgets.nests"
        static let agenda = "dev.pigeonpost.widgets.agenda"
        static let unread = "dev.pigeonpost.widgets.unread"
        static let latest = "dev.pigeonpost.widgets.latest"
    }

    static let service = "dev.pigeonpost.widgets.snapshot"
    static let account = "snapshot"

    /// `$(AppIdentifierPrefix)dev.pigeonpost.widgets`, from the bundle's Info.plist
    /// (`PPWidgetKeychainGroup`). Nil in an unsigned build, where there is nothing to share.
    static var accessGroup: String? {
        guard let group = Bundle.main.object(forInfoDictionaryKey: "PPWidgetKeychainGroup") as? String,
              group.hasSuffix(".dev.pigeonpost.widgets"), !group.hasPrefix("."), !group.contains("$(") else { return nil }
        return group
    }

    private static func query(_ group: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrAccessGroup as String: group,
            kSecAttrSynchronizable as String: false,
        ]
    }

    // MARK: Shared keychain (the app and the extension)

    static func read() -> WidgetSnapshot? {
        guard let group = accessGroup else { return nil }
        var request = query(group)
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        guard SecItemCopyMatching(request as CFDictionary, &item) == errSecSuccess, let data = item as? Data else { return nil }
        return WidgetCodec.decode(data)
    }

    struct WriteFailed: Error { let status: OSStatus }

    static func write(_ snapshot: WidgetSnapshot) throws {
        guard let group = accessGroup else { return }
        let data = try WidgetCodec.encode(snapshot)
        let attributes: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        var status = SecItemUpdate(query(group) as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            status = SecItemAdd(query(group).merging(attributes) { $1 } as CFDictionary, nil)
        }
        guard status == errSecSuccess else { throw WriteFailed(status: status) }
    }

    /// Signed out, another account, or nothing to show: the item goes, not just its contents.
    static func clear() {
        guard let group = accessGroup else { return }
        SecItemDelete(query(group) as CFDictionary)
    }

    // MARK: Files (tests)

    static func url(inContainer container: URL) -> URL {
        container.appendingPathComponent("Library/Application Support/Widgets", isDirectory: true)
            .appendingPathComponent("snapshot.json", isDirectory: false)
    }

    static func read(from url: URL) -> WidgetSnapshot? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return WidgetCodec.decode(data)
    }

    static func write(_ snapshot: WidgetSnapshot, to url: URL) throws {
        let data = try WidgetCodec.encode(snapshot)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        #if os(iOS)
        try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        #else
        try data.write(to: url, options: [.atomic])
        #endif
    }

    static func clear(at url: URL) {
        try? FileManager.default.removeItem(at: url)
    }
}
