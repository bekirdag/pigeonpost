#if DEBUG
import Foundation

/// Explicit debug fixtures never contact the postbox or SMS provider.
@MainActor final class PeopleFixtureAPI: PeopleAPI {
    var name: PPDisplayName?
    var numbers: [PPPhoneRecord] = []
    var phoneReads = 0
    var batches: [[String]] = []
    var invitations: [String] = []
    var beforeDiscovery: (() async -> Void)?
    var beforeName: (() async -> Void)?
    var budget = 200
    func phones(_ mailbox: String) async throws -> [PPPhoneRecord] {
        phoneReads += 1
        if CommandLine.arguments.contains("-people-discovery-fixtures"), numbers.isEmpty {
            numbers = [.init(id: "fixture_phone", identity: mailbox, countryCode: "1", masked: "+1 •••• 0100", discoverable: false, verifiedAt: 1)]
        }
        return numbers
    }
    func sendCode(_ mailbox: String, phone: String) async throws -> PPPhoneChallenge {
        .init(challengeId: "fixture_challenge", expiresAt: Int(Date().timeIntervalSince1970) + 600, resendAfter: Int(Date().timeIntervalSince1970) + 60, codeLength: 6)
    }
    func verify(_ mailbox: String, challenge: String, code: String, discoverable: Bool) async throws -> PPPhoneRecord {
        guard code == "123456" else { throw PeopleInput.invalid("Incorrect code.") }
        let phone = PPPhoneRecord(id: "fixture_phone", identity: mailbox, countryCode: "44", masked: "+44 •••• 0123", discoverable: discoverable, verifiedAt: 1)
        numbers = [phone]; return phone
    }
    func discoverable(_ mailbox: String, id: String, value: Bool) async throws -> PPPhoneRecord {
        guard let p = numbers.first(where: { $0.id == id }) else { throw PeopleInput.invalid("Number not found.") }
        let updated = PPPhoneRecord(id: p.id, identity: p.identity, countryCode: p.countryCode, masked: p.masked, discoverable: value, verifiedAt: p.verifiedAt)
        numbers = numbers.map { $0.id == id ? updated : $0 }; return updated
    }
    func unlink(_ mailbox: String, id: String) async throws { numbers.removeAll { $0.id == id } }
    func discover(_ mailbox: String, hashes: [String]) async throws -> PPDiscoveryResult {
        batches.append(hashes); await beforeDiscovery?()
        budget = max(0, budget - hashes.count)
        return .init(matches: [], checked: hashes.count, new: hashes.count, remainingToday: budget)
    }
    func invite(_ mailbox: String, phone: String) async throws -> PPInviteResult {
        invitations.append(phone); return .init(status: "sent", reason: nil, inviteId: "fixture_invite")
    }
    func ownName(_ mailbox: String) async throws -> PPDisplayName? { await beforeName?(); return name }
    func saveName(_ mailbox: String, name: String, visibility: String) async throws -> PPDisplayName {
        let value = PPDisplayName(displayName: try PeopleInput.name(name), visibility: visibility, updatedAt: 1); self.name = value; return value
    }
    func removeName(_ mailbox: String) async throws { name = nil }
    func profiles(_ mailbox: String, addresses: [String]) async throws -> [String: PPProfile?] {
        Dictionary(uniqueKeysWithValues: addresses.map { ($0, PPProfile(address: $0.hasPrefix("/k/") ? $0 : "/k/fixture", handle: $0.hasPrefix("/k/") ? nil : $0, displayName: "Example Person")) })
    }
}

@MainActor final class PeopleFixtureStorage: PeopleLocalStorage {
    private var values: [String: [String: String]] = [:]
    private var seen: Set<String> = []
    private func key(_ subject: String, _ mailbox: String) -> String { subject + "\u{0}" + mailbox }
    func aliases(subject: String, mailbox: String) -> [String: String] { values[key(subject, mailbox)] ?? [:] }
    func setAlias(subject: String, mailbox: String, peer: String, name: String?) throws { values[key(subject, mailbox), default: [:]][peer] = name }
    func promptSeen(subject: String, mailbox: String) -> Bool { seen.contains(key(subject, mailbox)) }
    func markPromptSeen(subject: String, mailbox: String) throws { seen.insert(key(subject, mailbox)) }
    func clear(subject: String) throws { values = values.filter { !$0.key.hasPrefix(subject + "\u{0}") }; seen = seen.filter { !$0.hasPrefix(subject + "\u{0}") } }
}
extension PeopleStore {
    func installContactFixtureIfRequested() {
        guard CommandLine.arguments.contains("-people-discovery-fixtures"), state.contacts.isEmpty else { return }
        if let contact = try? PhoneContact(phone: "+15555550100", name: "Example Contact") { importContacts(scope, contacts: [contact]) }
    }
}
#endif
