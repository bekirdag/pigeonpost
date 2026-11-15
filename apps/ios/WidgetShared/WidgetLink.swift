//  The places a widget can open, as `dev.pigeonpost.inbox://widget/...` URLs.
//
//  The app's own scheme, not a universal link: a widget tap should open this app and nothing else,
//  and must not depend on the associated-domains file being reachable. Parsing is strict — anything
//  that is not exactly one of these routes is not a widget link, and the app treats it as before.

import Foundation

enum WidgetRoute: Equatable, Sendable {
    case inbox
    case conversation(mailbox: String, peer: String)
    case compose
    case search
    case nests
    case getNest
    case calendar
    case event(mailbox: String, eventId: String, occurrenceId: String?)

    static let scheme = "dev.pigeonpost.inbox"
    static let host = "widget"

    var url: URL {
        var parts = URLComponents()
        parts.scheme = Self.scheme
        parts.host = Self.host
        switch self {
        case .inbox: parts.path = "/inbox"
        case .compose: parts.path = "/compose"
        case .search: parts.path = "/search"
        case .nests: parts.path = "/nests"
        case .getNest: parts.path = "/get-nest"
        case .calendar: parts.path = "/calendar"
        case let .conversation(mailbox, peer):
            parts.path = "/conversation"
            parts.queryItems = [URLQueryItem(name: "mailbox", value: mailbox), URLQueryItem(name: "peer", value: peer)]
        case let .event(mailbox, eventId, occurrenceId):
            parts.path = "/event"
            parts.queryItems = [URLQueryItem(name: "mailbox", value: mailbox), URLQueryItem(name: "event", value: eventId)]
                + (occurrenceId.map { [URLQueryItem(name: "occurrence", value: $0)] } ?? [])
        }
        // A handle's slashes and a /k/ address's characters are all legal in a query; `+` is not
        // decoded as a space by URLComponents, so encode it explicitly.
        parts.percentEncodedQuery = parts.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        return parts.url ?? URL(string: "\(Self.scheme)://\(Self.host)/inbox")!
    }

    init?(url: URL) {
        guard let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.scheme?.lowercased() == Self.scheme, parts.host?.lowercased() == Self.host else { return nil }
        let query = Dictionary((parts.queryItems ?? []).compactMap { item in item.value.map { (item.name, $0) } },
                               uniquingKeysWith: { first, _ in first })
        func value(_ name: String) -> String? {
            guard let text = query[name], !text.isEmpty, text.count <= 512 else { return nil }
            return text
        }
        switch parts.path {
        case "/inbox": self = .inbox
        case "/compose": self = .compose
        case "/search": self = .search
        case "/nests": self = .nests
        case "/get-nest": self = .getNest
        case "/calendar": self = .calendar
        case "/conversation":
            guard let mailbox = value("mailbox"), let peer = value("peer") else { return nil }
            self = .conversation(mailbox: mailbox, peer: peer)
        case "/event":
            guard let mailbox = value("mailbox"), let event = value("event") else { return nil }
            self = .event(mailbox: mailbox, eventId: event, occurrenceId: value("occurrence"))
        default: return nil
        }
    }
}
