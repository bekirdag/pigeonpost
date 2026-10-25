import Foundation

@main
enum DevicePairingTests {
    static func main() throws {
        var checked = 0
        func check(_ result: Bool, _ name: String) {
            guard result else { fatalError(name) }; checked += 1
        }
        for input in ["ABCD1234", "abcd1234", "ABCD-1234", "  abcd-1234\n"] {
            check(DevicePairingCode(input)?.rawValue == "ABCD1234", "normalize manual code")
        }
        check(DevicePairingCode("ABCD1234")?.displayValue == "ABCD-1234", "display code")
        for input in ["", "ABCD123", "ABCD12345", "AB-CD1234", "ABCD--1234", "ABCD 1234", "ABCD/1234",
                      "ABCD\u{0}1234", "ABCD\n1234", "ABCD12İ4", "ABCD1ß4", "ＡＢＣＤ1234", "ABCD-1234?next=x",
                      String(repeating: " ", count: 129) + "ABCD1234"] {
            check(DevicePairingCode(input) == nil, "reject malformed manual code")
        }
        let good = "https://inbox.pigeonpost.dev/pair.html#ABCD-1234"
        check(DevicePairingLink.code(in: URL(string: good)!)?.rawValue == "ABCD1234", "trusted terminal link")
        check(DevicePairingLink.code(in: URL(string: "https://inbox.pigeonpost.dev/pair.html#abcd1234")!)?.rawValue == "ABCD1234", "compact link")
        for value in [
            "http://inbox.pigeonpost.dev/pair.html#ABCD1234", "https://elsewhere.invalid/pair.html#ABCD1234",
            "https://inbox.pigeonpost.dev.evil.invalid/pair.html#ABCD1234", "https://user@inbox.pigeonpost.dev/pair.html#ABCD1234",
            "https://inbox.pigeonpost.dev:443/pair.html#ABCD1234", "https://inbox.pigeonpost.dev/pair.html?code=ABCD1234",
            "https://inbox.pigeonpost.dev/pair.html?x=y#ABCD1234", "https://inbox.pigeonpost.dev/pair.html#ABCD1234&x=y",
            "https://inbox.pigeonpost.dev/pair.html#%20ABCD1234", "https://inbox.pigeonpost.dev/pair.html#ABCD1234%0A",
            "https://inbox.pigeonpost.dev/pair.html/extra#ABCD1234", "https://inbox.pigeonpost.dev/%70air.html#ABCD1234",
            "dev.pigeonpost.inbox://pair.html#ABCD1234", "https://inbox.pigeonpost.dev/pair.html"
        ] { check(DevicePairingLink.code(in: URL(string: value)!) == nil, "reject untrusted pairing link") }

        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let base: [String: Any] = ["pairing_id": "pair_fixture", "org": ["id": "org_fixture", "name": "Example"],
            "handle": "/fixture/build", "address": "/k/fixture", "label": "Build machine", "confirmation": "123 456",
            "capabilities": ["mailbox"], "creates_agent": false, "state": "pending", "ip_country": "TR", "requested_at": 1_800_000_000,
            "expires_at": 1_800_000_600, "can_approve": true]
        func decode(_ updates: [String: Any] = [:]) throws -> DevicePairingPreview {
            let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
            return try decoder.decode(DevicePairingPreview.self, from: JSONSerialization.data(withJSONObject: base.merging(updates) { _, new in new }))
        }
        let preview = try decode()
        check(preview.isValid, "valid preview")
        check(preview.displayConfirmation == "123 456", "terminal confirmation")
        check(preview.allowsApproval(at: now), "authorized unexpired preview")
        check(!preview.allowsApproval(at: now.addingTimeInterval(600)), "expiry is exclusive")
        check(try !decode(["can_approve": false, "reason": "delegation_required"]).allowsApproval(at: now), "permission denied")
        check(try !decode(["requested_at": 1_800_000_061]).allowsApproval(at: now), "future request blocked")
        check(try decode(["address": NSNull(), "creates_agent": true]).allowsApproval(at: now), "explicit new-agent request")
        check(try !decode(["address": NSNull()]).isValid, "existing agent needs an address")
        check(try !decode(["state": "approved"]).allowsApproval(at: now), "decided request cannot approve twice")
        check(try !decode(["state": "unknown"]).isValid, "unknown request state is not actionable")
        for updates: [String: Any] in [
            ["pairing_id": "../different"], ["confirmation": "12 3456"], ["confirmation": "12345x"],
            ["confirmation": "１２３４５６"], ["capabilities": ["enroll", "enroll"]], ["capabilities": []],
            ["label": "Build\nnew action"], ["expires_at": 1_800_000_000], ["handle": "https://elsewhere.invalid"]
        ] { check(try !decode(updates).isValid, "malformed preview cannot approve") }
        print("Device pairing: \(checked) checks passed")
    }
}
