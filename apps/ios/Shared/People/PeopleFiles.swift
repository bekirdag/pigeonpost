import Foundation
import CryptoKit

/// Local aliases are not contact permission grants. No server, iCloud, or backup copies.
@MainActor final class PeopleFiles: PeopleLocalStorage {
    private struct Record: Codable { var aliases: [String: String] = [:]; var promptSeen = false }
    private let root: URL
    init(root: URL? = nil) {
        self.root = root ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Pigeonpost/LocalPeople", isDirectory: true)
    }
    private func hash(_ s: String) -> String { SHA256.hash(data: Data(s.utf8)).map { String(format: "%02x", $0) }.joined() }
    private func file(_ subject: String, _ mailbox: String) -> URL { root.appendingPathComponent(hash(subject), isDirectory: true).appendingPathComponent(hash(mailbox) + ".json") }
    private func read(_ subject: String, _ mailbox: String) -> Record {
        guard let data = try? Data(contentsOf: file(subject, mailbox)), data.count <= 1024 * 1024,
              let value = try? JSONDecoder().decode(Record.self, from: data) else { return Record() }
        return value
    }
    private func write(_ subject: String, _ mailbox: String, _ value: Record) throws {
        var dir = file(subject, mailbox).deletingLastPathComponent()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        var excluded = URLResourceValues(); excluded.isExcludedFromBackup = true; try dir.setResourceValues(excluded)
        let data = try JSONEncoder().encode(value)
        #if os(iOS)
        try data.write(to: file(subject, mailbox), options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        #else
        try data.write(to: file(subject, mailbox), options: .atomic)
        #endif
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file(subject, mailbox).path)
    }
    func aliases(subject: String, mailbox: String) -> [String: String] {
        read(subject, mailbox).aliases.filter { PeopleInput.address($0.key) && (try? PeopleInput.name($0.value, alias: true)) == $0.value }
    }
    func setAlias(subject: String, mailbox: String, peer: String, name: String?) throws {
        var value = read(subject, mailbox)
        guard value.aliases.count < 5000 || value.aliases[peer] != nil else { throw PeopleInput.invalid("The local contact-name limit was reached.") }
        value.aliases[peer] = name; try write(subject, mailbox, value)
    }
    func promptSeen(subject: String, mailbox: String) -> Bool { read(subject, mailbox).promptSeen }
    func markPromptSeen(subject: String, mailbox: String) throws { var value = read(subject, mailbox); value.promptSeen = true; try write(subject, mailbox, value) }
    func clear(subject: String) throws {
        let dir = root.appendingPathComponent(hash(subject), isDirectory: true)
        if FileManager.default.fileExists(atPath: dir.path) { try FileManager.default.removeItem(at: dir) }
    }
}

