import Foundation

/// A public locator or a personal invitation. Neither is member authorization.
enum CalendarLink: Equatable {
    case member(String)
    case guest(URL)

    init?(url: URL) {
        guard url.absoluteString.utf8.count <= 4096,
              var parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.user == nil, parts.password == nil, parts.port == nil,
              parts.percentEncodedQuery == nil else { return nil }
        let locator: String
        switch parts.scheme?.lowercased() {
        case "https":
            guard parts.host?.lowercased() == "inbox.pigeonpost.dev", parts.percentEncodedPath.hasPrefix("/m/") else { return nil }
            locator = String(parts.percentEncodedPath.dropFirst(3))
        case "dev.pigeonpost.inbox":
            guard parts.host == "m", parts.percentEncodedPath.hasPrefix("/") else { return nil }
            locator = String(parts.percentEncodedPath.dropFirst())
            parts.scheme = "https"; parts.host = "inbox.pigeonpost.dev"; parts.path = "/m/" + locator
        default: return nil
        }
        let code = Self.meetingCode(locator)
        guard let fragment = parts.percentEncodedFragment, !fragment.isEmpty else {
            guard let code else { return nil }
            self = .member(code)
            return
        }
        guard fragment.utf8.count <= 2300,
              let items = URLComponents(string: "https://fragment.invalid/?" + fragment)?.queryItems,
              Set(items.map(\.name)).count == items.count,
              items.allSatisfy({ $0.value != nil }) else { return nil }
        if locator == "u" {
            guard items.count == 1, items[0].name == "u", Self.token(items[0].value) else { return nil }
        } else {
            guard code != nil || Self.eventId(locator), (1...2).contains(items.count),
                  items.allSatisfy({ $0.name == "t" || $0.name == "r" }),
                  Self.token(items.first(where: { $0.name == "t" })?.value) else { return nil }
            if let status = items.first(where: { $0.name == "r" })?.value,
               !["accepted", "tentative", "declined"].contains(status) { return nil }
        }
        guard let browser = parts.url else { return nil }
        self = .guest(browser)
    }

    static func meetingCode(_ input: String) -> String? {
        let bytes = Array(input.utf8)
        guard bytes.count == 10 || (bytes.count == 11 && bytes[5] == 45) else { return nil }
        let raw = input.uppercased().replacingOccurrences(of: "-", with: "")
        let alphabet = Set("ABCDEFGHJKMNPQRSTUVWXYZ23456789".utf8)
        return raw.utf8.count == 10 && raw.utf8.allSatisfy(alphabet.contains) ? raw : nil
    }

    private static func eventId(_ value: String) -> Bool {
        value.hasPrefix("evt_") && value.utf8.count == 30 && value.utf8.allSatisfy(unreserved)
    }
    private static func token(_ value: String?) -> Bool {
        guard let value else { return false }
        return (1...2048).contains(value.utf8.count) && value.utf8.allSatisfy(unreserved)
    }
    private static func unreserved(_ c: UInt8) -> Bool {
        (65...90).contains(c) || (97...122).contains(c) || (48...57).contains(c) || c == 95 || c == 45
    }
}
