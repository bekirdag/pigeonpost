//  Where a device's group state lives between launches.
//
//  The MLS state is the only copy of this device's group keys, and the decrypted history is the
//  only copy of what it has read — a device cannot decrypt a message twice. Both are kept in one
//  file per mailbox, sealed with AES-GCM under a 256-bit key that never leaves the Keychain
//  (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`: readable by a background refresh after the
//  first unlock, never in a backup, never restored onto another device). The MLS blob inside is
//  itself encrypted by the library under its own state key, which sits in the same sealed file.
//
//  Losing the Keychain key loses the device, deliberately: the next launch enrols a new device and
//  the postbox shows it to every co-member as one (docs/groups.md §3 step 5).

import CryptoKit
import Foundation
import Security

/// Everything this device keeps for one mailbox. The web client's IndexedDB record, in Swift.
struct GroupRecord: Codable, Equatable {
    var deviceId: String
    var registered: Bool = false
    var fingerprint: String?
    var stateKey: Data
    var stateBlob: Data?
    var groups: [String: Cursor] = [:]
    var history: [String: [GroupItem]] = [:]

    struct Cursor: Codable, Equatable {
        var cursor: Int = 0
        var inGroup: Bool = false
        var acked: UInt64 = 0
    }
}

/// Load and save sealed records. `KeySource` exists so the hand-run tests can seal with a key that
/// is not in anybody's Keychain.
struct GroupVault: Sendable {
    let directory: URL
    let key: @Sendable () throws -> SymmetricKey

    static func standard() -> GroupVault {
        let base = (try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask,
                                                 appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return GroupVault(directory: base.appendingPathComponent("Groups", isDirectory: true),
                          key: { try GroupKeychain.wrapKey() })
    }

    private func file(for mailbox: String) -> URL {
        let digest = SHA256.hash(data: Data(mailbox.utf8)).map { String(format: "%02x", $0) }.joined()
        return directory.appendingPathComponent(digest + ".sealed")
    }

    func load(_ mailbox: String) -> GroupRecord? {
        guard let sealed = try? Data(contentsOf: file(for: mailbox)),
              let box = try? AES.GCM.SealedBox(combined: sealed),
              let key = try? key(),
              let plain = try? AES.GCM.open(box, using: key, authenticating: Data(mailbox.utf8))
        else { return nil }
        return try? JSONDecoder().decode(GroupRecord.self, from: plain)
    }

    /// Atomic: the blob, its key and the cursors land together or not at all, so a crash cannot
    /// leave an old MLS state beside newer cursors.
    func save(_ record: GroupRecord, for mailbox: String) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let plain = try JSONEncoder().encode(record)
        // The mailbox is bound in as associated data: a file copied over another mailbox's does
        // not open.
        let sealed = try AES.GCM.seal(plain, using: try key(), authenticating: Data(mailbox.utf8))
        guard let combined = sealed.combined else { throw CocoaError(.fileWriteUnknown) }
        #if os(iOS)
        try combined.write(to: file(for: mailbox), options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        #else
        try combined.write(to: file(for: mailbox), options: [.atomic])
        #endif
    }

    func remove(_ mailbox: String) {
        try? FileManager.default.removeItem(at: file(for: mailbox))
    }

    /// Signing out forgets every mailbox's group state on this device.
    func removeAll() {
        try? FileManager.default.removeItem(at: directory)
    }
}

/// The one Keychain item behind every sealed record.
enum GroupKeychain {
    private static let service = "dev.pigeonpost.inbox.groups"
    private static let account = "state-wrap-key"

    struct Failure: Error { let status: OSStatus }

    static func wrapKey() throws -> SymmetricKey {
        if let existing = read() { return SymmetricKey(data: existing) }
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw Failure(status: errSecAllocate)
        }
        let data = Data(bytes)
        try write(data)
        // Read back rather than trusting the write: a second process racing this one may have won,
        // and both must use the same key.
        return SymmetricKey(data: read() ?? data)
    }

    static func forget() {
        for dataProtection in [true, false] {
            var query = base(dataProtection: dataProtection)
            query.removeValue(forKey: kSecAttrAccessible as String)
            SecItemDelete(query as CFDictionary)
        }
    }

    private static func base(dataProtection: Bool) -> [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: false,
        ]
        #if os(macOS)
        // The iOS-style keychain, where accessibility classes mean something. An unsigned build
        // (the hand-run tests) has no entitlement for it, and falls back to the login keychain.
        if dataProtection { query[kSecUseDataProtectionKeychain as String] = true }
        #endif
        return query
    }

    private static func read() -> Data? {
        for dataProtection in [true, false] {
            var query = base(dataProtection: dataProtection)
            query[kSecReturnData as String] = true
            query[kSecMatchLimit as String] = kSecMatchLimitOne
            var item: CFTypeRef?
            if SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess, let data = item as? Data, data.count == 32 {
                return data
            }
            #if !os(macOS)
            break
            #endif
        }
        return nil
    }

    private static func write(_ data: Data) throws {
        var status: OSStatus = errSecSuccess
        for dataProtection in [true, false] {
            var query = base(dataProtection: dataProtection)
            query[kSecValueData as String] = data
            query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            status = SecItemAdd(query as CFDictionary, nil)
            if status == errSecSuccess || status == errSecDuplicateItem { return }
            #if !os(macOS)
            break
            #endif
        }
        throw Failure(status: status)
    }
}
