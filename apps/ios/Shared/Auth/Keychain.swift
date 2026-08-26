//  The two tokens, in the Keychain.
//
//  The web app keeps its refresh token in `localStorage` because a browser has nothing better. A
//  phone does: `kSecAttrAccessibleAfterFirstUnlock` keeps the item readable to a background refresh
//  after the device has been unlocked once since boot, and unreadable to anything that lifts the
//  file off a powered-down device.

import Foundation
import Security

enum Keychain {
    private static let service = "dev.pigeonpost.inbox.oidc"

    static func read(_ account: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data,
              let value = String(data: data, encoding: .utf8)
        else { return nil }
        return value
    }

    static func write(_ account: String, _ value: String?) {
        guard let value else { return delete(account) }
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        let attributes: [String: Any] = [
            kSecValueData as String: Data(value.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock,
        ]
        // Update first: SecItemAdd on an existing account fails with errSecDuplicateItem, and
        // deleting-then-adding leaves a window where the session has no token at all.
        let updated = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if updated == errSecItemNotFound {
            SecItemAdd(query.merging(attributes) { $1 } as CFDictionary, nil)
        }
    }

    static func delete(_ account: String) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(query as CFDictionary)
    }
}
