import Foundation

/// Short-lived proof from the terminal. Keep in view memory; never use it as a view identifier.
struct DevicePairingCode: Equatable, Hashable {
    let rawValue: String
    var displayValue: String { String(rawValue.prefix(4)) + "-" + String(rawValue.suffix(4)) }

    init?(_ input: String) {
        guard input.utf8.count <= 128 else { return nil }
        let value = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard value.utf8.allSatisfy({ $0 < 128 }) else { return nil }
        var bytes = Array(value.uppercased().utf8)
        if bytes.count == 9 {
            guard bytes[4] == 45 else { return nil }
            bytes.remove(at: 4)
        }
        guard bytes.count == 8, bytes.allSatisfy({ (65...90).contains($0) || (48...57).contains($0) }) else { return nil }
        rawValue = String(decoding: bytes, as: UTF8.self)
    }
}

enum DevicePairingLink {
    static func code(in url: URL) -> DevicePairingCode? {
        guard url.absoluteString.utf8.count <= 4096,
              let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.scheme == "https", parts.host == "inbox.pigeonpost.dev",
              parts.user == nil, parts.password == nil, parts.port == nil,
              parts.percentEncodedPath == "/pair.html", parts.query == nil,
              let fragment = parts.fragment, fragment == fragment.trimmingCharacters(in: .whitespacesAndNewlines)
        else { return nil }
        return DevicePairingCode(fragment)
    }
}

struct DevicePairingPreview: Decodable, Equatable {
    struct Organization: Decodable, Equatable { let id: String; let name: String }
    let pairingId: String
    let org: Organization
    let handle: String
    let address: String
    let label: String?
    let confirmation: String
    let capabilities: [String]
    let ipCountry: String?
    let requestedAt: Int
    let expiresAt: Int
    let canApprove: Bool
    let reason: String?

    var confirmationDigits: String { confirmation.replacingOccurrences(of: " ", with: "") }
    var displayConfirmation: String {
        let digits = confirmationDigits
        return String(digits.prefix(3)) + " " + String(digits.suffix(3))
    }
    var isValid: Bool {
        let safeIdentifier: (String) -> Bool = { value in
            !value.isEmpty && value.utf8.count <= 128 && value.utf8.allSatisfy {
                (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
            }
        }
        let safeText: (String, Int) -> Bool = { value, limit in
            !value.isEmpty && value.utf8.count <= limit && !value.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
        }
        return safeIdentifier(pairingId) && safeIdentifier(org.id) && safeText(org.name, 320)
            && handle.hasPrefix("/") && safeText(handle, 160)
            && address.hasPrefix("/k/") && safeText(address, 160)
            && (label == nil || safeText(label!, 320))
            && [6, 7].contains(confirmation.utf8.count) && confirmationDigits.utf8.count == 6
            && confirmationDigits.utf8.allSatisfy { (48...57).contains($0) }
            && (confirmation.utf8.count == 6 || confirmation.dropFirst(3).first == " ")
            && !capabilities.isEmpty && capabilities.count <= 32 && Set(capabilities).count == capabilities.count
            && capabilities.allSatisfy(safeIdentifier)
            && (ipCountry == nil || safeText(ipCountry!, 80)) && (reason == nil || safeText(reason!, 160))
            && requestedAt >= 0 && expiresAt > requestedAt && expiresAt <= 253_402_300_799
    }
    func allowsApproval(at now: Date) -> Bool {
        isValid && canApprove && Double(expiresAt) > now.timeIntervalSince1970
            && Double(requestedAt) <= now.timeIntervalSince1970 + 60
    }
}
