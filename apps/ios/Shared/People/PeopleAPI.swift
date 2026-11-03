import Foundation

@MainActor protocol PeopleAPI {
    func phones(_ mailbox: String) async throws -> [PPPhoneRecord]
    func sendCode(_ mailbox: String, phone: String) async throws -> PPPhoneChallenge
    func verify(_ mailbox: String, challenge: String, code: String, discoverable: Bool) async throws -> PPPhoneRecord
    func discoverable(_ mailbox: String, id: String, value: Bool) async throws -> PPPhoneRecord
    func unlink(_ mailbox: String, id: String) async throws
    func discover(_ mailbox: String, hashes: [String]) async throws -> PPDiscoveryResult
    func invite(_ mailbox: String, phone: String) async throws -> PPInviteResult
    func ownName(_ mailbox: String) async throws -> PPDisplayName?
    func saveName(_ mailbox: String, name: String, visibility: String) async throws -> PPDisplayName
    func removeName(_ mailbox: String) async throws
    func profiles(_ mailbox: String, addresses: [String]) async throws -> [String: PPProfile?]
}

/// Its client retains the original account + acting-inbox authorization, including token refresh.
@MainActor struct PeopleClient: PeopleAPI {
    let client: PostboxClient
    private struct PhoneResponse: Decodable { let number: PPPhoneRecord }
    private struct NameResponse: Decodable { let name: PPDisplayName? }
    private struct ProfilesResponse: Decodable { let profiles: [String: PPProfile?] }
    private func bad() -> APIError { APIError(status: 200, code: "bad_response", detail: "The people response could not be confirmed.") }
    private func request<T: Decodable>(_ mailbox: String, _ path: String, _ type: T.Type, method: String = "GET", json: [String: Any]? = nil, query: [URLQueryItem] = []) async throws -> T {
        guard PeopleInput.mailbox(mailbox) else { throw PeopleInput.invalid("Choose an inbox first.") }
        let body = try json.map { try JSONSerialization.data(withJSONObject: $0) }
        let data = try await client.sendRaw(path, method: method, query: query, body: body,
            contentType: body == nil ? nil : "application/json", headers: ["x-pigeonpost-identity": mailbox])
        guard data.count <= 512 * 1024 else { throw bad() }
        return try PostboxClient.decode(type, from: data)
    }
    private func phone(_ p: PPPhoneRecord) throws -> PPPhoneRecord {
        guard PeopleInput.identifier(p.id), PeopleInput.mailbox(p.identity), PeopleInput.matches(p.countryCode, "^[0-9]{1,3}$"),
              (1...64).contains(p.masked.count), !p.masked.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }), p.verifiedAt > 0 else { throw bad() }
        return p
    }
    private func name(_ n: PPDisplayName) throws -> PPDisplayName {
        guard PeopleInput.visibilities.contains(n.visibility), n.updatedAt > 0, (try? PeopleInput.name(n.displayName)) == n.displayName else { throw bad() }; return n
    }
    private func peer(_ address: String, _ handle: String?, _ name: String?) throws {
        guard PeopleInput.mailbox(address), handle.map({ PeopleInput.address($0) && !$0.hasPrefix("/k/") }) ?? true,
              name.map({ (try? PeopleInput.name($0)) == $0 }) ?? true else { throw bad() }
    }
    func phones(_ mailbox: String) async throws -> [PPPhoneRecord] {
        let r = try await request(mailbox, "/v1/phone", PPPhoneList.self)
        guard (1...3).contains(r.maxNumbers), r.numbers.count <= r.maxNumbers, Set(r.numbers.map(\.id)).count == r.numbers.count else { throw bad() }
        return try r.numbers.map(phone)
    }
    func sendCode(_ mailbox: String, phone: String) async throws -> PPPhoneChallenge {
        guard PeopleInput.phone(phone) else { throw PeopleInput.invalid("Use a phone number with its country code.") }
        let r = try await request(mailbox, "/v1/phone/verify", PPPhoneChallenge.self, method: "POST", json: ["phone": phone, "identity": mailbox])
        guard PeopleInput.identifier(r.challengeId), r.expiresAt > 0, r.resendAfter >= 0, r.codeLength == 6 else { throw bad() }; return r
    }
    func verify(_ mailbox: String, challenge: String, code: String, discoverable: Bool) async throws -> PPPhoneRecord {
        guard PeopleInput.identifier(challenge), PeopleInput.matches(code, "^[0-9]{6}$") else { throw PeopleInput.invalid("Enter the six-digit code.") }
        let r = try phone(try await request(mailbox, "/v1/phone/verify/" + challenge, PhoneResponse.self, method: "POST", json: ["code": code, "discoverable": discoverable]).number)
        guard r.identity == mailbox else { throw bad() }; return r
    }
    func discoverable(_ mailbox: String, id: String, value: Bool) async throws -> PPPhoneRecord {
        guard PeopleInput.identifier(id) else { throw bad() }
        let r = try phone(try await request(mailbox, "/v1/phone/" + id, PhoneResponse.self, method: "PATCH", json: ["discoverable": value]).number)
        guard r.id == id, r.discoverable == value else { throw bad() }; return r
    }
    func unlink(_ mailbox: String, id: String) async throws {
        guard PeopleInput.identifier(id) else { throw bad() }
        let _: [String: Bool] = try await request(mailbox, "/v1/phone/" + id, [String: Bool].self, method: "DELETE")
    }
    func discover(_ mailbox: String, hashes: [String]) async throws -> PPDiscoveryResult {
        guard (1...200).contains(hashes.count), Set(hashes).count == hashes.count, hashes.allSatisfy({ PeopleInput.matches($0, "^[a-f0-9]{32}$") }) else { throw bad() }
        let r = try await request(mailbox, "/v1/contacts/discover", PPDiscoveryResult.self, method: "POST", json: ["hashes": hashes])
        guard r.checked == hashes.count, (0...r.checked).contains(r.new), (0...1000).contains(r.remainingToday), r.matches.count <= hashes.count,
              Set(r.matches.map(\.hash)).count == r.matches.count else { throw bad() }
        for m in r.matches { guard hashes.contains(m.hash) else { throw bad() }; try peer(m.address, m.handle, m.displayName) }; return r
    }
    func invite(_ mailbox: String, phone: String) async throws -> PPInviteResult {
        guard PeopleInput.phone(phone) else { throw bad() }
        let r = try await request(mailbox, "/v1/contacts/invites", PPInviteResult.self, method: "POST", json: ["phone": phone, "confirm": true])
        guard (r.status == "sent" && r.inviteId.map(PeopleInput.identifier) == true) || (r.status == "not_sent" && ["already_invited", "unavailable"].contains(r.reason ?? "")) else { throw bad() }; return r
    }
    func ownName(_ mailbox: String) async throws -> PPDisplayName? { try await request(mailbox, "/v1/profile/name", NameResponse.self).name.map(name) }
    func saveName(_ mailbox: String, name: String, visibility: String) async throws -> PPDisplayName {
        guard PeopleInput.visibilities.contains(visibility) else { throw bad() }
        let r = try await request(mailbox, "/v1/profile/name", NameResponse.self, method: "PUT", json: ["display_name": try PeopleInput.name(name), "visibility": visibility])
        guard let n = r.name else { throw bad() }; return try self.name(n)
    }
    func removeName(_ mailbox: String) async throws {
        let _: [String: Bool] = try await request(mailbox, "/v1/profile/name", [String: Bool].self, method: "DELETE")
    }
    func profiles(_ mailbox: String, addresses: [String]) async throws -> [String: PPProfile?] {
        guard (1...100).contains(addresses.count), addresses.allSatisfy(PeopleInput.address) else { throw bad() }
        let r = try await request(mailbox, "/v1/profiles", ProfilesResponse.self, query: [.init(name: "addresses", value: addresses.joined(separator: ","))]).profiles
        for (key, value) in r {
            guard addresses.contains(key) else { throw bad() }
            if let value { try peer(value.address, value.handle, value.displayName); guard key == value.address || key == value.handle else { throw bad() } }
        }; return r
    }
}
