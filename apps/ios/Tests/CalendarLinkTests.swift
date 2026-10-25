import Foundation

@main enum CalendarLinkTests {
    static func main() throws {
        var checks = 0
        func check(_ value: Bool, _ label: String) { precondition(value, label); checks += 1 }
        func link(_ value: String) -> CalendarLink? { URL(string: value).flatMap(CalendarLink.init(url:)) }
        check(link("https://inbox.pigeonpost.dev/m/ABCDEFGHJK") == .member("ABCDEFGHJK"), "member locator")
        check(link("https://inbox.pigeonpost.dev/m/abcde-fghjk") == .member("ABCDEFGHJK"), "display code normalization")
        check(link("dev.pigeonpost.inbox://m/ABCDEFGHJK") == .member("ABCDEFGHJK"), "explicit app scheme")
        let guest = "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=fixture_token&r=accepted"
        check(link(guest) == .guest(URL(string: guest)!), "guest stays a browser URL")
        let nativeGuest = "dev.pigeonpost.inbox://m/ABCDEFGHJK#t=fixture_token&r=declined"
        check(link(nativeGuest) == .guest(URL(string: "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=fixture_token&r=declined")!), "native guest canonical origin")
        for status in ["accepted", "tentative", "declined"] {
            check(link("https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=fixture_token&r=" + status) != nil, "explicit answer preset")
        }
        check(link("https://inbox.pigeonpost.dev/m/evt_" + String(repeating: "a", count: 26) + "#t=fixture_token") != nil, "non-call guest invitation")
        check(link("https://inbox.pigeonpost.dev/m/u#u=fixture_token") != nil, "unsubscribe browser flow")
        let rejected = [
            "http://inbox.pigeonpost.dev/m/ABCDEFGHJK", "https://inbox.pigeonpost.dev.evil.invalid/m/ABCDEFGHJK",
            "https://evil.invalid/m/ABCDEFGHJK", "https://user@inbox.pigeonpost.dev/m/ABCDEFGHJK",
            "https://user:pass@inbox.pigeonpost.dev/m/ABCDEFGHJK", "https://inbox.pigeonpost.dev:444/m/ABCDEFGHJK",
            "https://inbox.pigeonpost.dev:443/m/ABCDEFGHJK", "https://inbox.pigeonpost.dev/m/../m/ABCDEFGHJK",
            "https://inbox.pigeonpost.dev/m//ABCDEFGHJK", "https://inbox.pigeonpost.dev/m/ABCDEFGHJK/extra",
            "https://inbox.pigeonpost.dev/m/%41BCDEFGHJK", "https://inbox.pigeonpost.dev/m/ABCDE%2FFGHJK",
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK?role=host", "https://inbox.pigeonpost.dev/m/ABCDEFGHJK?",
            "https://inbox.pigeonpost.dev/m/ABCDEFGHIJ", "https://inbox.pigeonpost.dev/m/ABC0EFGHJK",
            "https://inbox.pigeonpost.dev/m/ABC-DEFGHJK", "https://inbox.pigeonpost.dev/m/u",
            "https://inbox.pigeonpost.dev/m/evt_" + String(repeating: "a", count: 26),
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=one&t=two",
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=one&%74=two",
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=one&role=host",
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#r=accepted",
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=one&r=auto",
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=", "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=one%0A",
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=one%2Ftwo", "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#u=one",
            "https://inbox.pigeonpost.dev/m/u#u=one&scope=all", "https://inbox.pigeonpost.dev/m/u#t=one",
            "dev.pigeonpost.inbox://evil/ABCDEFGHJK", "dev.pigeonpost.inbox://user@m/ABCDEFGHJK",
            "dev.pigeonpost.inbox://m/ABCDEFGHJK?token=one",
            "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=" + String(repeating: "a", count: 2049)
        ]
        for value in rejected { check(link(value) == nil, "untrusted URL was accepted: " + value) }

        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        func status(_ rows: [[String: Any]], state: String = "open") throws -> CalendarMeetingStatus {
            try decoder.decode(CalendarMeetingStatus.self, from: JSONSerialization.data(withJSONObject: ["state": state, "occurrences": rows]))
        }
        let row: [String: Any] = ["occurrence_id": "occ_opaque", "opens_at": 100, "starts_at": 200, "ends_at": 300, "closes_at": 400]
        check(try status([row]).choices.first?.id == "occ_opaque", "wire occurrence")
        check(try status([row], state: "not_open").choices.isEmpty, "not open")
        check(try status([row], state: "future_state").choices.isEmpty, "unknown status")
        check(try status([row, row]).choices.isEmpty, "duplicate IDs")
        check(try status(Array(repeating: row, count: 51)).choices.isEmpty, "bounded response")
        for (key, value) in [("occurrence_id", "occ_bad/path" as Any), ("occurrence_id", "occ_" as Any),
                             ("opens_at", -1 as Any), ("opens_at", 201 as Any), ("ends_at", 199 as Any),
                             ("closes_at", 299 as Any), ("closes_at", 253_402_300_800 as Any)] {
            var bad = row; bad[key] = value
            check(try status([bad]).choices.isEmpty, "invalid service occurrence")
        }
        print("Calendar links: \(checks) checks passed")
    }
}
