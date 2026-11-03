import Foundation
import CryptoKit

// The shared client decodes snake_case. Explicit CodingKeys would decode these twice.
struct PPPhoneRecord: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let identity: String
    let countryCode: String
    let masked: String
    let discoverable: Bool
    let verifiedAt: Int
}
struct PPPhoneList: Decodable, Equatable, Sendable { let numbers: [PPPhoneRecord]; let maxNumbers: Int }
struct PPPhoneChallenge: Decodable, Equatable, Sendable {
    let challengeId: String
    let expiresAt: Int
    let resendAfter: Int
    let codeLength: Int
}
struct PPDisplayName: Decodable, Equatable, Sendable { let displayName: String; let visibility: String; let updatedAt: Int }
struct PPProfile: Decodable, Equatable, Sendable { let address: String; let handle: String?; let displayName: String? }
struct PPDiscoveryMatch: Decodable, Equatable, Sendable {
    let hash: String
    let address: String
    let handle: String?
    let displayName: String?
}
struct PPDiscoveryResult: Decodable, Equatable, Sendable {
    let matches: [PPDiscoveryMatch]
    let checked: Int
    let new: Int
    let remainingToday: Int
}
struct PPInviteResult: Decodable, Equatable, Sendable { let status: String; let reason: String?; let inviteId: String? }

enum PeopleInput {
    static let visibilities = ["known", "everyone", "nobody"]
    static func matches(_ value: String, _ pattern: String) -> Bool {
        value.range(of: pattern, options: .regularExpression) == value.startIndex..<value.endIndex
    }
    static func phone(_ value: String) -> Bool { matches(value, "^\\+[1-9][0-9]{7,14}$") }
    static func identifier(_ value: String) -> Bool { matches(value, "^[A-Za-z0-9_-]{1,128}$") }
    static func address(_ value: String) -> Bool {
        (2...512).contains(value.utf8.count) && matches(value, "^/[A-Za-z0-9!$&'*+\\-=^_`{|}~.@/]+$")
            && !value.contains("//") && !value.split(separator: "/").contains(where: { $0 == "." || $0 == ".." })
    }
    static func mailbox(_ value: String) -> Bool { matches(value, "^/k/[a-z0-9]+$") }
    static func invalid(_ message: String) -> APIError { APIError(status: 0, code: "invalid_input", detail: message) }
    static func hash(_ phone: String) throws -> String {
        guard Self.phone(phone) else { throw invalid("Use a phone number with its country code.") }
        return SHA256.hash(data: Data(("pigeonpost:phone:v1:" + phone).utf8)).prefix(16).map { String(format: "%02x", $0) }.joined()
    }
    static func name(_ value: String, alias: Bool = false) throws -> String {
        for c in value.unicodeScalars {
            let v = c.value
            guard c != "/", c != "@", ![.control, .format, .surrogate, .lineSeparator, .paragraphSeparator].contains(c.properties.generalCategory),
                  ![0x034f, 0x115f, 0x1160, 0x17b4, 0x17b5, 0x2800, 0x3164, 0xffa0].contains(v),
                  !(0x180b...0x180f).contains(v), !(0xfe00...0xfe0f).contains(v),
                  !(0xfff0...0xfffb).contains(v), !(0xe0000...0xe007f).contains(v) else {
                throw invalid("Use a name without addresses or invisible characters.")
            }
        }
        let name = value.precomposedStringWithCanonicalMapping.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
        guard (1...(alias ? 64 : 40)).contains(name.unicodeScalars.count) else { throw invalid(alias ? "Use 1–64 characters." : "Use 1–40 characters.") }
        let plain = name.lowercased().filter { $0.isLetter || $0.isNumber }
        guard !plain.isEmpty else { throw invalid("Include at least one letter or digit.") }
        guard alias || (!plain.contains("pigeonpost") && !["admin", "support", "official", "verified", "security", "system", "moderator"].contains(plain)) else {
            throw invalid("Choose a personal name.")
        }
        return name
    }
}

/// Contact names and raw numbers stay in memory; only `hash` enters discovery requests.
struct PhoneContact: Identifiable, Equatable, Sendable {
    let phone: String
    let name: String
    let hash: String
    var id: String { hash }
    init(phone: String, name: String) throws {
        self.phone = phone
        self.name = String(name.prefix(128))
        self.hash = try PeopleInput.hash(phone)
    }
}
